//! Document-level sanitization over a real [`pdfxi_core::Document`] plus an
//! independent verifier (T-0257).
//!
//! [`sanitize_document_full`] removes the hidden-information categories that
//! must not survive a redaction or a shared-file export:
//!
//! - the trailer `/Info` dictionary is replaced with an empty dictionary;
//! - XMP metadata is removed from the `/Metadata` entry of every dictionary in
//!   the reachable object graph (catalog, pages, image and form XObjects,
//!   annotations, patterns, and any other dictionary carrying one), and each
//!   referenced XMP stream subtree is nulled;
//! - `/Names /EmbeddedFiles` attachments, their file specifications and their
//!   embedded-file streams are removed;
//! - catalog, page and annotation `/AA` actions, `/OpenAction`, the
//!   `/Names /JavaScript` name tree and every `/S /JavaScript` action object are
//!   removed;
//! - `/OCProperties` and every `/OC` optional-content key are removed when the
//!   layers are visible by default; default-hidden layers fail closed because
//!   removing the properties would reveal them;
//! - page `/Thumb` thumbnails are removed.
//!
//! Every pass is a full [`pdfxi_core::Document::save`] rewrite, never an
//! incremental append, so removed bytes are not left in an earlier revision. A second pass
//! over the reparsed classic-cross-reference output removes category objects
//! that were not reachable from the catalog. If a selected category still
//! survives both passes, the call returns [`SecurityError::SanitizeResidual`]
//! instead of a partial result.
//!
//! [`verify_sanitized`] is deliberately independent of the removal code: it
//! reparses the saved bytes, re-extracts page text, scans the whole file and
//! every text-candidate stream (stored and Flate-decompressed) for caller
//! secrets in UTF-8 and UTF-16LE, and re-derives a category inventory. A
//! candidate stream that cannot be decoded is a survivor, so an unreadable
//! stream fails closed. The report never carries document contents or secret
//! text; a survivor carries a category, page, object number and secret index.

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::fmt;

use pdfxi_core::{
    Dictionary, Document, Filter, FlateParams, Object, ObjectId, decode_stream, decode_text_string,
};

use crate::encryption_writer::collect_object_ids;
use crate::error::{Result, SecurityError};
use crate::sanitize_document::trailer_reference;

fn core(error: pdfxi_core::Error) -> SecurityError {
    SecurityError::SanitizationCoreFailed(error.to_string())
}

/// A category of hidden information removed by a document sanitize pass.
///
/// Declaration order is canonical for plans, inventories and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum SanitizeCategory {
    /// Trailer `/Info` fields (title, author, subject, and the rest).
    DocumentInfo,
    /// XMP metadata streams reached through any dictionary's `/Metadata` entry.
    XmpMetadata,
    /// Embedded files, file specifications and embedded-file streams.
    Attachments,
    /// Catalog, page, annotation and object-level JavaScript actions.
    JavaScript,
    /// Optional-content configuration and layer keys.
    OptionalContent,
    /// Page `/Thumb` thumbnails.
    Thumbnails,
}

impl SanitizeCategory {
    /// Every category in canonical order.
    pub const ALL: [Self; 6] = [
        Self::DocumentInfo,
        Self::XmpMetadata,
        Self::Attachments,
        Self::JavaScript,
        Self::OptionalContent,
        Self::Thumbnails,
    ];

    /// Stable identifier suitable for persistence or comparison.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::DocumentInfo => "document_info",
            Self::XmpMetadata => "xmp_metadata",
            Self::Attachments => "attachments",
            Self::JavaScript => "javascript",
            Self::OptionalContent => "optional_content",
            Self::Thumbnails => "thumbnails",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_id() == id)
    }
}

impl fmt::Display for SanitizeCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// A deterministic selection of categories to remove.
///
/// [`Self::default`] selects every category. The sanitizer always fails closed
/// when a selected category survives; there is no permissive mode.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeRequest {
    categories: BTreeSet<SanitizeCategory>,
}

impl Default for SanitizeRequest {
    fn default() -> Self {
        Self::all()
    }
}

impl SanitizeRequest {
    /// Select every category.
    pub fn all() -> Self {
        Self {
            categories: SanitizeCategory::ALL.into_iter().collect(),
        }
    }

    /// Select no category.
    pub fn empty() -> Self {
        Self {
            categories: BTreeSet::new(),
        }
    }

    /// Build a request from an explicit set of categories.
    pub fn new(categories: impl IntoIterator<Item = SanitizeCategory>) -> Self {
        Self {
            categories: categories.into_iter().collect(),
        }
    }

    /// Add a category to the selection.
    pub fn include(&mut self, category: SanitizeCategory) {
        self.categories.insert(category);
    }

    /// Remove a category from the selection.
    pub fn exclude(&mut self, category: SanitizeCategory) {
        self.categories.remove(&category);
    }

    /// Whether `category` is selected.
    pub fn contains(&self, category: SanitizeCategory) -> bool {
        self.categories.contains(&category)
    }

    /// Selected categories in canonical order.
    pub fn categories(&self) -> impl Iterator<Item = SanitizeCategory> + '_ {
        self.categories.iter().copied()
    }

    /// Number of selected categories.
    pub fn len(&self) -> usize {
        self.categories.len()
    }

    /// Whether nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.categories.is_empty()
    }
}

/// One category item removed by a sanitize pass.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemovedSanitizeItem {
    category: SanitizeCategory,
    target: String,
}

impl RemovedSanitizeItem {
    /// Build a removed-item record.
    pub fn new(category: SanitizeCategory, target: impl Into<String>) -> Self {
        Self {
            category,
            target: target.into(),
        }
    }

    /// Category of the removed item.
    pub const fn category(&self) -> SanitizeCategory {
        self.category
    }

    /// Stable identifier of the removed item (never document content).
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// A deterministic report of a document sanitize run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeReport {
    /// The categories the caller selected, in canonical order.
    pub requested: Vec<SanitizeCategory>,
    /// Concrete items removed, in canonical order and deduplicated.
    pub removed: Vec<RemovedSanitizeItem>,
    /// Categories present in the source before the run.
    pub before: BTreeSet<SanitizeCategory>,
    /// Categories present in the final re-parsed output.
    pub after: BTreeSet<SanitizeCategory>,
    /// Number of discovered attachment names that were unsafe (traversal,
    /// absolute paths, drive letters, control characters, or empty).
    pub unsafe_attachment_names: usize,
}

impl SanitizeReport {
    /// Selected categories still present in the output.
    pub fn residuals(&self) -> Vec<SanitizeCategory> {
        self.requested
            .iter()
            .copied()
            .filter(|category| self.after.contains(category))
            .collect()
    }

    /// Whether no selected category remains.
    pub fn is_clean(&self) -> bool {
        self.residuals().is_empty()
    }
}

/// The owned result of a full document sanitize.
#[derive(Debug, Clone)]
pub struct SanitizedPdf {
    bytes: Vec<u8>,
    report: SanitizeReport,
}

impl SanitizedPdf {
    /// The sanitized, standalone PDF bytes from a full rewrite.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The deterministic report of what was removed.
    pub fn report(&self) -> &SanitizeReport {
        &self.report
    }

    /// Re-parse the sanitized bytes with the core parser.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the produced bytes cannot be parsed.
    pub fn document(&self) -> Result<Document> {
        Document::from_bytes(self.bytes.clone()).map_err(core)
    }
}

// ---------------------------------------------------------------------------
// Public sanitize entry point
// ---------------------------------------------------------------------------

/// Remove the selected hidden-information categories from `source`.
///
/// The source is borrowed immutably. The returned bytes come from
/// [`pdfxi_core::Document::save`] (a full, single-revision rewrite), never
/// from [`pdfxi_core::Document::save_incremental`]. A second pass over the reparsed output
/// removes orphan category objects.
///
/// # Errors
///
/// Returns a typed error when the source cannot be read or saved, when
/// default-hidden optional content would be revealed, or when a selected
/// category survives both passes. No document contents are included in an
/// error.
pub fn sanitize_document_full(
    source: &Document,
    request: &SanitizeRequest,
) -> Result<SanitizedPdf> {
    let before = sanitize_inventory(source)?;
    let requested: Vec<SanitizeCategory> = request.categories().collect();
    let mut working = source.clone();
    let mut removed = BTreeSet::new();
    let mut unsafe_names = BTreeSet::new();
    let mut output = None;

    // Two passes: the first removes reachable categories, the second catches
    // category objects that were present in the source but not reachable from
    // the catalog. The core writer emits a classic table, so the second pass
    // can enumerate every live object of the reparsed output.
    for _ in 0..2 {
        apply_pass(&mut working, request, &mut removed, &mut unsafe_names)?;
        let candidate = working.save().map_err(core)?;
        let reparsed = Document::from_bytes(candidate.clone()).map_err(core)?;
        let after = sanitize_inventory(&reparsed)?;
        if requested.iter().all(|category| !after.contains(category)) {
            output = Some((candidate, after));
            break;
        }
        working = reparsed;
    }

    let Some((bytes, after)) = output else {
        let after = sanitize_inventory(&working)?;
        let residual = requested
            .iter()
            .find(|category| after.contains(category))
            .copied()
            .unwrap_or(SanitizeCategory::DocumentInfo);
        return Err(SecurityError::SanitizeResidual(residual.as_id()));
    };

    Ok(SanitizedPdf {
        bytes,
        report: SanitizeReport {
            requested,
            removed: removed.into_iter().collect(),
            before,
            after,
            unsafe_attachment_names: unsafe_names.len(),
        },
    })
}

/// Return `true` when an attachment name is a plain file name.
///
/// Rejected forms include an empty name, longer than 255 bytes, `.` or `..`,
/// names with leading or trailing whitespace, control characters, `/` or `\\`
/// separators (so traversal and absolute paths are rejected), and any `:`
/// (Windows drive letters and alternate data streams).
pub fn is_safe_attachment_name(name: &str) -> bool {
    if name.is_empty() || name.len() > 255 {
        return false;
    }
    if name == "." || name == ".." || name.trim() != name {
        return false;
    }
    if name.chars().any(|character| {
        character.is_control() || character == ':' || character == '/' || character == '\\'
    }) {
        return false;
    }
    true
}

// ---------------------------------------------------------------------------
// Category inventory
// ---------------------------------------------------------------------------

/// Derive a category inventory from a parsed document.
///
/// Categories are detected from the object graph, not from the caller's
/// request, so the same function measures the source and the sanitized output.
///
/// # Errors
///
/// Returns a typed error when a referenced object or `/Info` cannot be read.
pub fn sanitize_inventory(document: &Document) -> Result<BTreeSet<SanitizeCategory>> {
    let objects = load_graph(document)?;
    let root = document.root().map_err(core)?;
    let mut present = BTreeSet::new();

    let info = document.info().map_err(core)?;
    let mut info_present = info.is_some_and(|info| {
        info.title.is_some()
            || info.author.is_some()
            || info.subject.is_some()
            || info.keywords.is_some()
            || info.creator.is_some()
            || info.producer.is_some()
            || info.creation_date.is_some()
            || info.modification_date.is_some()
    });
    if let Some(id) = trailer_reference(document.bytes(), "Info") {
        info_present |= objects
            .get(&id)
            .and_then(Object::as_dictionary)
            .is_some_and(|dictionary| !dictionary.is_empty());
    }
    if info_present {
        present.insert(SanitizeCategory::DocumentInfo);
    }

    let catalog = objects.get(&root).and_then(Object::as_dictionary);
    if catalog.is_some_and(|dictionary| dictionary.contains_key("OCProperties")) {
        present.insert(SanitizeCategory::OptionalContent);
    }
    if catalog.is_some_and(|dictionary| dictionary.contains_key("Collection")) {
        present.insert(SanitizeCategory::Attachments);
    }
    if catalog.is_some_and(|dictionary| {
        dictionary.contains_key("OpenAction") || dictionary.contains_key("AA")
    }) {
        present.insert(SanitizeCategory::JavaScript);
    }
    if let Some(names) = catalog.and_then(|dictionary| dictionary.get("Names")) {
        if resolve_has(&objects, names, "EmbeddedFiles") {
            present.insert(SanitizeCategory::Attachments);
        }
        if resolve_has(&objects, names, "JavaScript") {
            present.insert(SanitizeCategory::JavaScript);
        }
    }

    for page in document.pages().map_err(core)? {
        if let Some(dictionary) = objects.get(&page.id).and_then(Object::as_dictionary) {
            if dictionary.contains_key("Thumb") {
                present.insert(SanitizeCategory::Thumbnails);
            }
            if dictionary.contains_key("AA") {
                present.insert(SanitizeCategory::JavaScript);
            }
            if dictionary.contains_key("AF") {
                present.insert(SanitizeCategory::Attachments);
            }
        }
    }

    for object in objects.values() {
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        // Every dictionary's `/Metadata` entry is XMP, regardless of the value's
        // type. We deliberately do not inspect or validate that value: a present
        // key is sufficient, so a direct stream/dictionary, an indirect
        // reference, and a malformed or unreadable value all fail closed as
        // "XMP present". `load_graph` resolves every reachable reference
        // strictly, so an unreadable indirect `/Metadata` target makes this
        // inventory call fail instead of reporting a false clean.
        if dictionary.contains_key("Metadata") {
            present.insert(SanitizeCategory::XmpMetadata);
        }
        let type_name = dictionary.get("Type").and_then(Object::as_name);
        let action_name = dictionary.get("S").and_then(Object::as_name);
        if matches!(type_name, Some("Filespec" | "EmbeddedFile"))
            || dictionary.contains_key("EmbeddedFile")
            || dictionary.contains_key("AF")
        {
            present.insert(SanitizeCategory::Attachments);
        }
        if action_name == Some("JavaScript")
            || type_name == Some("JavaScript")
            || dictionary.contains_key("AA")
            || dictionary.contains_key("OpenAction")
        {
            present.insert(SanitizeCategory::JavaScript);
        }
        if matches!(type_name, Some("OCG" | "OCMD"))
            || dictionary.contains_key("OC")
            || dictionary.contains_key("OCGs")
        {
            present.insert(SanitizeCategory::OptionalContent);
        }
        if dictionary.contains_key("Thumb") {
            present.insert(SanitizeCategory::Thumbnails);
        }
    }

    Ok(present)
}

/// Whether `value` resolves to a dictionary containing `key`.
fn resolve_has(objects: &BTreeMap<ObjectId, Object>, value: &Object, key: &str) -> bool {
    resolve_dictionary(objects, value).is_some_and(|dictionary| dictionary.contains_key(key))
}

// ---------------------------------------------------------------------------
// One sanitize pass
// ---------------------------------------------------------------------------

fn apply_pass(
    document: &mut Document,
    request: &SanitizeRequest,
    removed: &mut BTreeSet<RemovedSanitizeItem>,
    unsafe_names: &mut BTreeSet<String>,
) -> Result<()> {
    let objects = load_graph(document)?;
    let root = document.root().map_err(core)?;

    if request.contains(SanitizeCategory::OptionalContent) {
        ensure_optional_content_safe(&objects, root)?;
    }

    let mut stage = Stage::new(objects);
    stage.apply_catalog(request, root);
    stage.apply_object_categories(request, root);

    stage.write_back(document)?;
    for item in stage.removed {
        removed.insert(item);
    }
    unsafe_names.append(&mut stage.unsafe_names);

    if request.contains(SanitizeCategory::DocumentInfo) && document.info().map_err(core)?.is_some()
    {
        document
            .set_info(&pdfxi_core::DocumentInfo::default())
            .map_err(core)?;
        removed.insert(RemovedSanitizeItem::new(
            SanitizeCategory::DocumentInfo,
            "trailer/Info",
        ));
    }
    Ok(())
}

/// Fail closed when default-hidden optional-content layers are declared.
fn ensure_optional_content_safe(
    objects: &BTreeMap<ObjectId, Object>,
    root: ObjectId,
) -> Result<()> {
    let Some(catalog) = objects.get(&root).and_then(Object::as_dictionary) else {
        return Ok(());
    };
    let Some(value) = catalog.get("OCProperties") else {
        return Ok(());
    };
    let Some(properties) = resolve_dictionary(objects, value) else {
        return Err(SecurityError::UnsafeOptionalContent);
    };
    let Some(default) = properties.get("D") else {
        return Ok(());
    };
    let Some(default) = resolve_dictionary(objects, default) else {
        return Err(SecurityError::UnsafeOptionalContent);
    };
    let base_off = default.get("BaseState").and_then(Object::as_name) == Some("OFF");
    let off_entries = default
        .get("OFF")
        .and_then(|value| resolve_filter_value(objects, value))
        .and_then(Object::as_array)
        .is_some_and(|array| !array.is_empty());
    if base_off || off_entries {
        return Err(SecurityError::UnsafeOptionalContent);
    }
    Ok(())
}

struct Stage {
    objects: BTreeMap<ObjectId, Object>,
    changed: BTreeSet<ObjectId>,
    neutralized: BTreeSet<ObjectId>,
    removed: BTreeSet<RemovedSanitizeItem>,
    unsafe_names: BTreeSet<String>,
}

impl Stage {
    fn new(objects: BTreeMap<ObjectId, Object>) -> Self {
        Self {
            objects,
            changed: BTreeSet::new(),
            neutralized: BTreeSet::new(),
            removed: BTreeSet::new(),
            unsafe_names: BTreeSet::new(),
        }
    }

    fn apply_catalog(&mut self, request: &SanitizeRequest, root: ObjectId) {
        if request.contains(SanitizeCategory::Attachments) {
            self.remove_names_key(
                root,
                "EmbeddedFiles",
                "Catalog/Names/EmbeddedFiles",
                SanitizeCategory::Attachments,
            );
            self.remove_key(
                root,
                "Collection",
                "Catalog/Collection",
                SanitizeCategory::Attachments,
            );
        }
        if request.contains(SanitizeCategory::JavaScript) {
            self.remove_names_key(
                root,
                "JavaScript",
                "Catalog/Names/JavaScript",
                SanitizeCategory::JavaScript,
            );
        }
    }

    fn apply_object_categories(&mut self, request: &SanitizeRequest, root: ObjectId) {
        let ids: Vec<ObjectId> = self.objects.keys().copied().collect();
        for id in ids {
            if self.objects.get(&id) == Some(&Object::Null) {
                continue;
            }
            let Some(object) = self.objects.get(&id).cloned() else {
                continue;
            };
            let Some(dictionary) = object.as_dictionary() else {
                continue;
            };
            if request.contains(SanitizeCategory::XmpMetadata)
                && dictionary.contains_key("Metadata")
            {
                let label = if id == root {
                    "Catalog/Metadata".to_owned()
                } else {
                    format!("Object/{id}/Metadata")
                };
                self.remove_key(id, "Metadata", label, SanitizeCategory::XmpMetadata);
            }
            let type_name = dictionary.get("Type").and_then(Object::as_name);
            let action_name = dictionary.get("S").and_then(Object::as_name);

            if request.contains(SanitizeCategory::Attachments)
                && (matches!(type_name, Some("Filespec" | "EmbeddedFile"))
                    || dictionary.contains_key("EmbeddedFile"))
            {
                if let Some(name) = filespec_name(dictionary) {
                    self.record_attachment_name(&name);
                }
                self.neutralize_tree(id, SanitizeCategory::Attachments, format!("Object/{id}"));
                continue;
            }
            if request.contains(SanitizeCategory::JavaScript)
                && (action_name == Some("JavaScript") || type_name == Some("JavaScript"))
            {
                self.neutralize_tree(id, SanitizeCategory::JavaScript, format!("Object/{id}"));
                continue;
            }
            if request.contains(SanitizeCategory::OptionalContent)
                && matches!(type_name, Some("OCG" | "OCMD"))
            {
                self.neutralize_tree(
                    id,
                    SanitizeCategory::OptionalContent,
                    format!("Object/{id}"),
                );
                continue;
            }

            if request.contains(SanitizeCategory::Attachments) {
                self.remove_key(
                    id,
                    "AF",
                    format!("Object/{id}/AF"),
                    SanitizeCategory::Attachments,
                );
            }
            if request.contains(SanitizeCategory::JavaScript) {
                self.remove_key(
                    id,
                    "AA",
                    format!("Object/{id}/AA"),
                    SanitizeCategory::JavaScript,
                );
                self.remove_key(
                    id,
                    "OpenAction",
                    format!("Object/{id}/OpenAction"),
                    SanitizeCategory::JavaScript,
                );
                self.remove_js_action_key(id);
            }
            if request.contains(SanitizeCategory::OptionalContent) {
                self.remove_key(
                    id,
                    "OCProperties",
                    format!("Object/{id}/OCProperties"),
                    SanitizeCategory::OptionalContent,
                );
                self.remove_key(
                    id,
                    "OC",
                    format!("Object/{id}/OC"),
                    SanitizeCategory::OptionalContent,
                );
            }
            if request.contains(SanitizeCategory::Thumbnails) {
                self.remove_key(
                    id,
                    "Thumb",
                    format!("Object/{id}/Thumb"),
                    SanitizeCategory::Thumbnails,
                );
            }
        }
    }

    fn remove_js_action_key(&mut self, owner: ObjectId) {
        let is_js = self
            .objects
            .get(&owner)
            .and_then(Object::as_dictionary)
            .and_then(|dictionary| dictionary.get("A"))
            .and_then(|value| resolve_dictionary(&self.objects, value))
            .is_some_and(|action| {
                action.get("S").and_then(Object::as_name) == Some("JavaScript")
                    || action.get("Type").and_then(Object::as_name) == Some("JavaScript")
            });
        if is_js {
            self.remove_key(
                owner,
                "A",
                format!("Object/{owner}/A"),
                SanitizeCategory::JavaScript,
            );
        }
    }

    fn remove_names_key(
        &mut self,
        root: ObjectId,
        inner: &str,
        label: &str,
        category: SanitizeCategory,
    ) {
        let names = self
            .objects
            .get(&root)
            .and_then(Object::as_dictionary)
            .and_then(|dictionary| dictionary.get("Names"))
            .cloned();
        let Some(names) = names else {
            return;
        };
        match names {
            Object::Reference(names_id) => {
                let child = self.objects.get(&names_id).cloned();
                let Some(Object::Dictionary(mut dictionary)) = child else {
                    return;
                };
                let Some(value) = dictionary.remove(inner) else {
                    return;
                };
                self.objects
                    .insert(names_id, Object::Dictionary(dictionary));
                self.changed.insert(names_id);
                self.neutralize_value(&value, category, label);
                self.removed
                    .insert(RemovedSanitizeItem::new(category, label));
            }
            Object::Dictionary(_) => {
                let value = self
                    .objects
                    .get_mut(&root)
                    .and_then(dictionary_mut)
                    .and_then(|catalog| catalog.get_mut("Names"))
                    .and_then(|names| match names {
                        Object::Dictionary(dictionary) => dictionary.remove(inner),
                        _ => None,
                    });
                if let Some(value) = value {
                    self.changed.insert(root);
                    self.neutralize_value(&value, category, label);
                    self.removed
                        .insert(RemovedSanitizeItem::new(category, label));
                }
            }
            _ => {}
        }
    }

    fn remove_key(
        &mut self,
        owner: ObjectId,
        key: &str,
        label: impl Into<String>,
        category: SanitizeCategory,
    ) -> bool {
        let value = self
            .objects
            .get(&owner)
            .and_then(Object::as_dictionary)
            .and_then(|dictionary| dictionary.get(key))
            .cloned();
        let Some(value) = value else {
            return false;
        };
        let removed = self
            .objects
            .get_mut(&owner)
            .and_then(dictionary_mut)
            .is_some_and(|dictionary| dictionary.remove(key).is_some());
        if !removed {
            return false;
        }
        self.changed.insert(owner);
        let label = label.into();
        self.neutralize_value(&value, category, &label);
        self.removed
            .insert(RemovedSanitizeItem::new(category, label));
        true
    }

    fn neutralize_value(&mut self, value: &Object, category: SanitizeCategory, _label: &str) {
        if let Some(id) = value.as_reference() {
            self.neutralize_tree(id, category, format!("Object/{id}"));
        }
    }

    fn neutralize_tree(&mut self, id: ObjectId, category: SanitizeCategory, label: String) {
        if !self.neutralized.insert(id) {
            return;
        }
        let Some(object) = self.objects.get(&id).cloned() else {
            return;
        };
        if category == SanitizeCategory::Attachments
            && let Some(dictionary) = object.as_dictionary()
        {
            self.record_attachment_dictionary(dictionary);
        }
        for child in referenced_ids(&object) {
            self.neutralize_tree(child, category, format!("Object/{child}"));
        }
        self.objects.insert(id, Object::Null);
        self.changed.insert(id);
        self.removed
            .insert(RemovedSanitizeItem::new(category, label));
    }

    fn record_attachment_name(&mut self, name: &str) {
        if !is_safe_attachment_name(name) {
            self.unsafe_names.insert(name.to_owned());
        }
    }

    fn record_attachment_dictionary(&mut self, dictionary: &Dictionary) {
        if let Some(name) = filespec_name(dictionary) {
            self.record_attachment_name(&name);
        }
        if let Some(entries) = dictionary.get("Names").and_then(Object::as_array) {
            for item in entries.iter().step_by(2) {
                if let Object::String(bytes) = item {
                    self.record_attachment_name(&decode_text_string(bytes));
                }
            }
        }
    }

    fn write_back(&self, document: &mut Document) -> Result<()> {
        for id in &self.changed {
            let Some(object) = self.objects.get(id) else {
                continue;
            };
            document.set_object(*id, object.clone()).map_err(core)?;
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Independent verification
// ---------------------------------------------------------------------------

/// The surface that produced a verification survivor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum SanitizeSurvivorKind {
    /// Page text extraction still contains a supplied secret.
    ExtractedText,
    /// The saved file's raw bytes contain a supplied secret as UTF-8.
    RawBytes,
    /// A stream's stored or decompressed bytes contain a supplied secret.
    StreamBytes,
    /// A text-candidate stream could not be decoded, so it cannot be cleared.
    UnreadableStream,
    /// A document-information category is still present.
    DocumentInfo,
    /// An XMP metadata category is still present.
    XmpMetadata,
    /// An attachment category is still present.
    Attachment,
    /// A JavaScript category is still present.
    JavaScript,
    /// An optional-content category is still present.
    OptionalContent,
    /// A thumbnail category is still present.
    Thumbnail,
}

impl SanitizeSurvivorKind {
    /// Stable identifier.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::ExtractedText => "extracted_text",
            Self::RawBytes => "raw_bytes",
            Self::StreamBytes => "stream_bytes",
            Self::UnreadableStream => "unreadable_stream",
            Self::DocumentInfo => "document_info",
            Self::XmpMetadata => "xmp_metadata",
            Self::Attachment => "attachment",
            Self::JavaScript => "javascript",
            Self::OptionalContent => "optional_content",
            Self::Thumbnail => "thumbnail",
        }
    }
}

impl fmt::Display for SanitizeSurvivorKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// One reason an independent verification is not clean.
///
/// The struct never carries secret text or document content: a secret is
/// identified by its index in the caller's slice.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeSurvivor {
    /// Surface that produced the finding.
    pub kind: SanitizeSurvivorKind,
    /// Category the finding belongs to, when it is a category check.
    pub category: Option<SanitizeCategory>,
    /// Zero-based page index, for extracted text.
    pub page: Option<usize>,
    /// Object number, for stream findings.
    pub object: Option<ObjectId>,
    /// Index of the supplied secret that survived.
    pub secret_index: Option<usize>,
    /// Stable, content-free description.
    pub detail: String,
}

/// The independent verifier's typed result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SanitizeVerification {
    /// `true` when no survivor was found.
    pub clean: bool,
    /// Every finding, in a deterministic order.
    pub survivors: Vec<SanitizeSurvivor>,
    /// Number of non-empty secrets that were checked.
    pub secrets_checked: usize,
    /// Number of pages whose text was re-extracted.
    pub pages_checked: usize,
}

/// Re-parse and independently verify sanitized bytes.
///
/// Checks, in order:
///
/// 1. every page's extracted text for each secret (case-insensitive);
/// 2. the whole saved file for each secret as UTF-8;
/// 3. every text-candidate stream's stored and Flate-decompressed bytes for
///    each secret in UTF-8 and UTF-16LE;
/// 4. a fresh category inventory for all six categories.
///
/// A text-candidate stream with a filter this build cannot decode is reported
/// as [`SanitizeSurvivorKind::UnreadableStream`], so unreadable candidate
/// streams fail closed.
///
/// # Errors
///
/// Returns a typed error when the bytes cannot be parsed or page text cannot be
/// extracted.
pub fn verify_sanitized(bytes: &[u8], secrets: &[&str]) -> Result<SanitizeVerification> {
    let document = Document::from_bytes(bytes.to_vec()).map_err(core)?;
    let objects = load_graph(&document)?;
    let pages = document.page_count().map_err(core)?;
    let mut survivors = Vec::new();

    for page in 0..pages {
        let text = document.page_text(page).map_err(core)?.to_lowercase();
        for (index, secret) in secrets.iter().enumerate() {
            if !secret.is_empty() && text.contains(&secret.to_lowercase()) {
                survivors.push(SanitizeSurvivor {
                    kind: SanitizeSurvivorKind::ExtractedText,
                    category: None,
                    page: Some(page),
                    object: None,
                    secret_index: Some(index),
                    detail: format!("page {page} text still contains secret #{index}"),
                });
            }
        }
    }

    for (index, secret) in secrets.iter().enumerate() {
        if secret.is_empty() {
            continue;
        }
        if find_subslice(bytes, secret.as_bytes()).is_some() {
            survivors.push(SanitizeSurvivor {
                kind: SanitizeSurvivorKind::RawBytes,
                category: None,
                page: None,
                object: None,
                secret_index: Some(index),
                detail: format!("saved file still contains secret #{index} as UTF-8"),
            });
        }
    }

    for (id, object) in &objects {
        let Object::Stream { dictionary, data } = object else {
            continue;
        };
        if !stream_can_carry_text(dictionary, &objects) {
            continue;
        }
        let filters = filter_names(dictionary, &objects);
        let decoded = decoded_stream(data, dictionary, &filters, &objects);
        if decoded.is_err() {
            survivors.push(SanitizeSurvivor {
                kind: SanitizeSurvivorKind::UnreadableStream,
                category: None,
                page: None,
                object: Some(*id),
                secret_index: None,
                detail: format!("stream object {} could not be decoded", id.number),
            });
            continue;
        }
        let decoded = decoded.ok().flatten().unwrap_or_default();
        for (index, secret) in secrets.iter().enumerate() {
            if secret.is_empty() {
                continue;
            }
            let needles = [secret.as_bytes().to_vec(), utf16le(secret)];
            if needles.iter().any(|needle| contains_bytes(data, needle))
                || needles
                    .iter()
                    .any(|needle| contains_bytes(&decoded, needle))
            {
                survivors.push(SanitizeSurvivor {
                    kind: SanitizeSurvivorKind::StreamBytes,
                    category: None,
                    page: None,
                    object: Some(*id),
                    secret_index: Some(index),
                    detail: format!("stream object {} still contains secret #{index}", id.number),
                });
            }
        }
    }

    let inventory = sanitize_inventory(&document)?;
    for category in SanitizeCategory::ALL {
        if !inventory.contains(&category) {
            continue;
        }
        let kind = match category {
            SanitizeCategory::DocumentInfo => SanitizeSurvivorKind::DocumentInfo,
            SanitizeCategory::XmpMetadata => SanitizeSurvivorKind::XmpMetadata,
            SanitizeCategory::Attachments => SanitizeSurvivorKind::Attachment,
            SanitizeCategory::JavaScript => SanitizeSurvivorKind::JavaScript,
            SanitizeCategory::OptionalContent => SanitizeSurvivorKind::OptionalContent,
            SanitizeCategory::Thumbnails => SanitizeSurvivorKind::Thumbnail,
        };
        survivors.push(SanitizeSurvivor {
            kind,
            category: Some(category),
            page: None,
            object: None,
            secret_index: None,
            detail: format!("category still present: {}", category.as_id()),
        });
    }

    Ok(SanitizeVerification {
        clean: survivors.is_empty(),
        survivors,
        secrets_checked: secrets.iter().filter(|secret| !secret.is_empty()).count(),
        pages_checked: pages,
    })
}

// ---------------------------------------------------------------------------
// Object-graph helpers
// ---------------------------------------------------------------------------

/// Load the reachable object graph plus any additional live objects that can be
/// read from the source's classic object headers.
///
/// Reachable objects are loaded strictly: a broken reference is an error.
/// Header-discovered objects are best-effort, so a byte sequence that only
/// looks like an object header inside a stream cannot fail or extend the graph.
fn load_graph(document: &Document) -> Result<BTreeMap<ObjectId, Object>> {
    let root = document.root().map_err(core)?;
    let mut pending = VecDeque::new();
    let mut seen = BTreeSet::new();
    let mut objects = BTreeMap::new();

    for id in std::iter::once(root)
        .chain(trailer_reference(document.bytes(), "Info"))
        .chain(
            document
                .pages()
                .map_err(core)?
                .into_iter()
                .map(|page| page.id),
        )
    {
        if seen.insert(id) {
            pending.push_back(id);
        }
    }

    while let Some(id) = pending.pop_front() {
        let object = document.object(id).map_err(core)?;
        for child in referenced_ids(&object) {
            if seen.insert(child) {
                pending.push_back(child);
            }
        }
        objects.insert(id, object);
    }

    for id in collect_object_ids(document.bytes()) {
        if objects.contains_key(&id) {
            continue;
        }
        if let Ok(object) = document.object(id) {
            objects.insert(id, object);
        }
    }
    Ok(objects)
}

fn referenced_ids(object: &Object) -> Vec<ObjectId> {
    let mut ids = Vec::new();
    collect_references(object, &mut ids);
    ids
}

fn collect_references(object: &Object, ids: &mut Vec<ObjectId>) {
    match object {
        Object::Reference(id) => ids.push(*id),
        Object::Array(items) => {
            for item in items {
                collect_references(item, ids);
            }
        }
        Object::Dictionary(dictionary) => {
            for value in dictionary.values() {
                collect_references(value, ids);
            }
        }
        Object::Stream { dictionary, .. } => {
            for value in dictionary.values() {
                collect_references(value, ids);
            }
        }
        Object::Null
        | Object::Boolean(_)
        | Object::Integer(_)
        | Object::Real(_)
        | Object::Name(_)
        | Object::String(_) => {}
    }
}

fn dictionary_mut(object: &mut Object) -> Option<&mut Dictionary> {
    match object {
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => Some(dictionary),
        _ => None,
    }
}

/// Resolve a value that may be a reference to a dictionary.
fn resolve_dictionary<'a>(
    objects: &'a BTreeMap<ObjectId, Object>,
    value: &'a Object,
) -> Option<&'a Dictionary> {
    match value {
        Object::Reference(id) => objects.get(id).and_then(Object::as_dictionary),
        other => other.as_dictionary(),
    }
}

fn filespec_name(dictionary: &Dictionary) -> Option<String> {
    for key in ["UF", "F", "Unix", "DOS", "Mac"] {
        if let Some(Object::String(bytes)) = dictionary.get(key) {
            return Some(decode_text_string(bytes));
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Saved-byte and stream helpers
// ---------------------------------------------------------------------------

fn is_binary_filter(name: &str) -> bool {
    matches!(
        name,
        "DCTDecode"
            | "JPXDecode"
            | "CCITTFaxDecode"
            | "JBIG2Decode"
            | "RunLengthDecode"
            | "LZWDecode"
    )
}

fn stream_can_carry_text(dictionary: &Dictionary, objects: &BTreeMap<ObjectId, Object>) -> bool {
    let type_name = dictionary.get("Type").and_then(Object::as_name);
    let subtype = dictionary.get("Subtype").and_then(Object::as_name);
    if matches!(
        type_name,
        Some("Image" | "XRef" | "ObjStm" | "Font" | "FontDescriptor" | "EmbeddedFile")
    ) || matches!(
        subtype,
        Some("Image" | "Type1C" | "CIDFontType0C" | "OpenType")
    ) || dictionary.contains_key("Length1")
        || dictionary.contains_key("Length2")
        || dictionary.contains_key("Length3")
    {
        return false;
    }
    !filter_names(dictionary, objects)
        .iter()
        .any(|filter| is_binary_filter(filter))
}

fn filter_names(dictionary: &Dictionary, objects: &BTreeMap<ObjectId, Object>) -> Vec<String> {
    let Some(value) = dictionary.get("Filter").or_else(|| dictionary.get("F")) else {
        return Vec::new();
    };
    let value = resolve_filter_value(objects, value);
    match value {
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(|item| resolve_filter_value(objects, item).and_then(Object::as_name))
            .map(str::to_owned)
            .collect(),
        _ => Vec::new(),
    }
}

fn resolve_filter_value<'a>(
    objects: &'a BTreeMap<ObjectId, Object>,
    value: &'a Object,
) -> Option<&'a Object> {
    match value {
        Object::Reference(id) => objects.get(id),
        other => Some(other),
    }
}

fn flate_params(dictionary: &Dictionary, objects: &BTreeMap<ObjectId, Object>) -> FlateParams {
    let Some(parms) = dictionary
        .get("DecodeParms")
        .or_else(|| dictionary.get("DP"))
    else {
        return FlateParams::default();
    };
    let Some(parms) = resolve_dictionary(objects, parms) else {
        return FlateParams::default();
    };
    let predictor = parms
        .get("Predictor")
        .and_then(Object::as_i64)
        .and_then(pdfxi_core::Predictor::from_pdf)
        .unwrap_or(pdfxi_core::Predictor::None);
    let colors = parms
        .get("Colors")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    let bits = parms
        .get("BitsPerComponent")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(8);
    let columns = parms
        .get("Columns")
        .and_then(Object::as_i64)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(1);
    FlateParams::new(predictor, colors, bits, columns)
}

/// Decode a candidate stream. `Ok(None)` means the stored bytes are the content.
/// `Err(())` means the stream declares a filter chain this verifier cannot read.
fn decoded_stream(
    data: &[u8],
    dictionary: &Dictionary,
    filters: &[String],
    objects: &BTreeMap<ObjectId, Object>,
) -> std::result::Result<Option<Vec<u8>>, ()> {
    if filters.is_empty() {
        return Ok(None);
    }
    if filters.len() == 1 && filters[0] == "FlateDecode" {
        let params = flate_params(dictionary, objects);
        return decode_stream(data, &[Filter::FlateDecode(params)])
            .map(Some)
            .map_err(|_| ());
    }
    Err(())
}

fn utf16le(text: &str) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(text.len() * 2);
    for unit in text.encode_utf16() {
        bytes.extend_from_slice(&unit.to_le_bytes());
    }
    bytes
}

fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
    !needle.is_empty() && find_subslice(haystack, needle).is_some()
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}
