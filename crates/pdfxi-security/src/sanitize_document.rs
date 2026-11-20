//! Applying a [`Sanitizer`] plan to a concrete [`pdfxi_core::Document`].
//!
//! [`sanitize_document`] takes an immutable reference to a real, parsed PDF
//! document, expands a [`Sanitizer`] selection into its deterministic ordered
//! action plan, applies each action to the object graph, and writes a fresh,
//! standalone PDF byte stream. The source document is never mutated
//! (copy-on-write): all work happens on owned copies of the indirect objects.
//!
//! # Scope and honesty
//!
//! This module operates on classic-cross-reference, unencrypted PDFs — the same
//! layout the core writer emits. Xref-stream, linearized, and encrypted inputs
//! are rejected with a typed error rather than silently mishandled.
//!
//! Metadata removal drops the trailer `/Info` key and empties the information
//! object; the `/Metadata` (XMP) stream is detached and nulled. Embedded-file
//! and JavaScript name trees are removed from `/Names`; page `/Annots` arrays
//! are removed; document-level `/OpenAction` JavaScript is removed; and form
//! field values (`/V`, `/DV`, `/AP`, `/AS`) are cleared while the field
//! structures themselves remain.
//!
//! Verification is performed on bytes: the output is re-parsed with the core
//! parser and a fresh [`SanitizationInventory`] is derived from the resulting
//! object graph. A [`SanitizationReport`] then confirms the selected categories
//! are gone, and strict mode converts any residual into
//! [`SecurityError::SanitizationVerificationFailed`].
//!
//! No document contents or secrets are ever logged or embedded in an error.

use std::collections::BTreeSet;
use std::fmt;

use pdfxi_core::{Dictionary, Document, Object, ObjectId, Page};

use crate::encryption_writer::{collect_object_ids, write_pdf};
use crate::error::{Result, SecurityError};
use crate::sanitization::{
    SanitizationAction, SanitizationCategory, SanitizationInventory, SanitizationReport, Sanitizer,
};

fn core_error(error: pdfxi_core::Error) -> SecurityError {
    SecurityError::SanitizationCoreFailed(error.to_string())
}

fn load_objects(document: &Document) -> Result<Vec<(ObjectId, Object)>> {
    collect_object_ids(document.bytes())
        .into_iter()
        .map(|id| {
            let object = document.object(id).map_err(core_error)?;
            Ok((id, object))
        })
        .collect()
}

fn find_object(objects: &[(ObjectId, Object)], id: ObjectId) -> Option<&Object> {
    objects.iter().find(|(oid, _)| *oid == id).map(|(_, o)| o)
}

fn object_index(objects: &[(ObjectId, Object)], id: ObjectId) -> Option<usize> {
    objects.iter().position(|(oid, _)| *oid == id)
}

/// Locate an indirect reference stored under `key` in the trailer section.
pub(crate) fn trailer_reference(bytes: &[u8], key: &str) -> Option<ObjectId> {
    let start = bytes.windows(7).rposition(|window| window == b"trailer")?;
    let section = &bytes[start..];
    let needle = format!("/{key}").into_bytes();
    let position = section
        .windows(needle.len())
        .position(|window| window == needle)?;
    let rest = std::str::from_utf8(&section[position + needle.len()..]).ok()?;
    let mut tokens = rest.split_ascii_whitespace();
    let number = tokens.next()?.parse::<u32>().ok()?;
    let generation = tokens.next()?.parse::<u16>().ok()?;
    if tokens.next() != Some("R") {
        return None;
    }
    Some(ObjectId::new(number, generation))
}

fn decode_hex(bytes: &[u8]) -> Option<Vec<u8>> {
    let mut out = Vec::with_capacity(bytes.len() / 2);
    let mut high = None;
    for &byte in bytes {
        if byte.is_ascii_whitespace() {
            continue;
        }
        let digit = (byte as char).to_digit(16)? as u8;
        match high.take() {
            Some(h) => out.push((h << 4) | digit),
            None => high = Some(digit),
        }
    }
    if high.is_some() {
        return None;
    }
    Some(out)
}

/// Preserve the trailer's `/ID` array when re-serializing, if present.
fn trailer_id(bytes: &[u8]) -> Option<Object> {
    let start = bytes.windows(7).rposition(|window| window == b"trailer")?;
    let section = &bytes[start..];
    let position = section.windows(3).position(|window| window == b"/ID")?;
    let mut rest = &section[position + 3..];
    while rest.first().is_some_and(u8::is_ascii_whitespace) {
        rest = &rest[1..];
    }
    if rest.first() != Some(&b'[') {
        return None;
    }
    let end = rest.iter().position(|byte| *byte == b']')?;
    let inner = &rest[1..end];
    let mut strings = Vec::new();
    let mut index = 0usize;
    while index < inner.len() {
        match inner[index] {
            b'<' => {
                let close = inner[index + 1..].iter().position(|byte| *byte == b'>')? + index + 1;
                strings.push(Object::String(decode_hex(&inner[index + 1..close])?));
                index = close + 1;
            }
            b'(' => {
                let close = inner[index + 1..].iter().position(|byte| *byte == b')')? + index + 1;
                strings.push(Object::String(inner[index + 1..close].to_vec()));
                index = close + 1;
            }
            _ => index += 1,
        }
    }
    if strings.len() == 2 {
        Some(Object::Array(strings))
    } else {
        None
    }
}

fn dictionary_of(object: &Object) -> Option<&Dictionary> {
    object.as_dictionary()
}

fn catalog_has(objects: &[(ObjectId, Object)], root: ObjectId, key: &str) -> bool {
    find_object(objects, root)
        .and_then(dictionary_of)
        .is_some_and(|dictionary| dictionary.contains_key(key))
}

fn nested_has(objects: &[(ObjectId, Object)], root: ObjectId, outer: &str, inner: &str) -> bool {
    let Some(catalog) = find_object(objects, root).and_then(dictionary_of) else {
        return false;
    };
    let child = match catalog.get(outer) {
        Some(Object::Reference(id)) => find_object(objects, *id),
        other => other,
    };
    child
        .and_then(dictionary_of)
        .is_some_and(|dictionary| dictionary.contains_key(inner))
}

fn non_empty_array(dictionary: &Dictionary, key: &str) -> bool {
    dictionary
        .get(key)
        .and_then(Object::as_array)
        .is_some_and(|array| !array.is_empty())
}

/// Derive a presence inventory by inspecting a real parsed object graph.
pub fn inventory_from_document(document: &Document) -> Result<SanitizationInventory> {
    let objects = load_objects(document)?;
    let root = document.root().map_err(core_error)?;
    let mut present = BTreeSet::new();

    // Metadata: a populated /Info object or a catalog /Metadata stream.
    if trailer_reference(document.bytes(), "Info")
        .and_then(|id| find_object(&objects, id))
        .and_then(dictionary_of)
        .is_some_and(|dictionary| !dictionary.is_empty())
    {
        present.insert(SanitizationCategory::Metadata);
    }
    if catalog_has(&objects, root, "Metadata") {
        present.insert(SanitizationCategory::Metadata);
    }

    // Embedded files: a name-tree entry or an embedded-file stream object.
    if nested_has(&objects, root, "Names", "EmbeddedFiles")
        || objects.iter().any(|(_, object)| {
            dictionary_of(object).is_some_and(|dictionary| {
                dictionary.get("Type").and_then(Object::as_name) == Some("Filespec")
                    || dictionary.contains_key("EmbeddedFile")
            })
        })
    {
        present.insert(SanitizationCategory::EmbeddedFiles);
    }

    // Annotations: any page carrying a non-empty /Annots array.
    if document.pages().map_err(core_error)?.iter().any(|page| {
        find_object(&objects, page.id)
            .and_then(dictionary_of)
            .is_some_and(|dictionary| non_empty_array(dictionary, "Annots"))
    }) {
        present.insert(SanitizationCategory::Annotations);
    }

    // JavaScript: name-tree JavaScript, document /OpenAction JS, or a JS action.
    let open_action_has_js = find_object(&objects, root)
        .and_then(dictionary_of)
        .and_then(|dictionary| dictionary.get("OpenAction"))
        .and_then(|value| match value {
            Object::Reference(id) => find_object(&objects, *id),
            other => Some(other),
        })
        .and_then(dictionary_of)
        .is_some_and(|dictionary| {
            dictionary.contains_key("JS")
                || dictionary.get("S").and_then(Object::as_name) == Some("JavaScript")
        });
    if nested_has(&objects, root, "Names", "JavaScript")
        || open_action_has_js
        || objects.iter().any(|(_, object)| {
            dictionary_of(object).is_some_and(|dictionary| {
                dictionary.get("S").and_then(Object::as_name) == Some("JavaScript")
                    || dictionary.get("Type").and_then(Object::as_name) == Some("JavaScript")
            })
        })
    {
        present.insert(SanitizationCategory::JavaScript);
    }

    // Form values: a form field (/FT) that still stores a non-null /V.
    if objects.iter().any(|(_, object)| {
        dictionary_of(object).is_some_and(|dictionary| {
            dictionary.contains_key("FT")
                && dictionary.get("V").is_some_and(|value| {
                    *value != Object::Null
                        && !matches!(value, Object::String(bytes) if bytes.is_empty())
                })
        })
    }) {
        present.insert(SanitizationCategory::FormValues);
    }

    Ok(SanitizationInventory::new(present))
}

fn dict_ref(objects: &[(ObjectId, Object)], id: ObjectId) -> Option<&Dictionary> {
    object_index(objects, id).and_then(|index| dictionary_of(&objects[index].1))
}

fn dict_mut(objects: &mut [(ObjectId, Object)], id: ObjectId) -> Option<&mut Dictionary> {
    let index = object_index(objects, id)?;
    match &mut objects[index].1 {
        Object::Dictionary(dictionary) => Some(dictionary),
        Object::Stream { dictionary, .. } => Some(dictionary),
        _ => None,
    }
}

/// Replace a referenced child object with null before detaching its key, so its
/// bytes cannot remain in the output as an orphan.
fn neutralize_child(objects: &mut [(ObjectId, Object)], owner: ObjectId, key: &str) {
    let child = dict_ref(objects, owner)
        .and_then(|dictionary| dictionary.get(key))
        .and_then(Object::as_reference);
    if let Some(child) = child
        && let Some(index) = object_index(objects, child)
    {
        objects[index].1 = Object::Null;
    }
}

/// Detach a key from a top-level (catalog) dictionary, nulling a referenced
/// child first.
fn detach_catalog_key(objects: &mut [(ObjectId, Object)], root: ObjectId, key: &str) -> bool {
    neutralize_child(objects, root, key);
    dict_mut(objects, root).is_some_and(|dictionary| dictionary.remove(key).is_some())
}

/// Remove `inner` from the catalog's `/Names` dictionary, which may be direct or
/// behind one indirect reference. A referenced child is nulled first.
fn remove_names_key(objects: &mut [(ObjectId, Object)], root: ObjectId, inner: &str) -> bool {
    let names_ref = dict_ref(objects, root)
        .and_then(|dictionary| dictionary.get("Names"))
        .and_then(Object::as_reference);
    if let Some(names_id) = names_ref {
        neutralize_child(objects, names_id, inner);
        return dict_mut(objects, names_id)
            .is_some_and(|dictionary| dictionary.remove(inner).is_some());
    }
    let root_index = match object_index(objects, root) {
        Some(index) => index,
        None => return false,
    };
    match &mut objects[root_index].1 {
        Object::Dictionary(catalog) => match catalog.get_mut("Names") {
            Some(Object::Dictionary(names)) => names.remove(inner).is_some(),
            _ => false,
        },
        _ => false,
    }
}

fn apply_action(
    action: &SanitizationAction,
    objects: &mut [(ObjectId, Object)],
    root: ObjectId,
    pages: &[Page],
    info_id: Option<ObjectId>,
) -> bool {
    match action.target() {
        "Info" => info_id
            .and_then(|id| object_index(objects, id))
            .is_some_and(|index| {
                objects[index].1 = Object::Dictionary(Dictionary::new());
                true
            }),
        "XMP" => detach_catalog_key(objects, root, "Metadata"),
        "Names/EmbeddedFiles" => remove_names_key(objects, root, "EmbeddedFiles"),
        "Page/Annots" => {
            let mut changed = false;
            for page in pages {
                let annot_refs: Vec<ObjectId> = dict_ref(objects, page.id)
                    .and_then(|dictionary| dictionary.get("Annots"))
                    .and_then(Object::as_array)
                    .map(|array| array.iter().filter_map(Object::as_reference).collect())
                    .unwrap_or_default();
                for id in annot_refs {
                    if let Some(index) = object_index(objects, id) {
                        objects[index].1 = Object::Null;
                    }
                }
                if let Some(index) = object_index(objects, page.id)
                    && let Object::Dictionary(dictionary) = &mut objects[index].1
                    && dictionary.remove("Annots").is_some()
                {
                    changed = true;
                }
            }
            changed
        }
        "Names/JavaScript" => remove_names_key(objects, root, "JavaScript"),
        "OpenAction/JS" => detach_catalog_key(objects, root, "OpenAction"),
        "AcroForm/FieldValues" => {
            let mut changed = false;
            for (_, object) in objects.iter_mut() {
                if let Object::Dictionary(dictionary) = object
                    && dictionary.contains_key("FT")
                {
                    for key in ["V", "DV", "AP", "AS"] {
                        if dictionary.remove(key).is_some() {
                            changed = true;
                        }
                    }
                }
            }
            changed
        }
        _ => false,
    }
}

/// The owned result of applying a sanitizer to a document.
#[derive(Debug, Clone)]
pub struct SanitizedDocument {
    bytes: Vec<u8>,
    report: AppliedSanitizationReport,
}

impl SanitizedDocument {
    /// The sanitized, standalone PDF bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// The deterministic report describing what was applied and verified.
    pub fn report(&self) -> &AppliedSanitizationReport {
        &self.report
    }

    /// Re-parse the sanitized bytes with the core parser.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the produced bytes cannot be parsed.
    pub fn document(&self) -> Result<Document> {
        Document::from_bytes(self.bytes.clone()).map_err(core_error)
    }
}

/// A deterministic report of a concrete sanitization run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AppliedSanitizationReport {
    /// The full action plan, in canonical order.
    pub actions: Vec<SanitizationAction>,
    /// The subset of planned actions that changed at least one object.
    pub applied: Vec<SanitizationAction>,
    /// Categories present in the source before sanitization.
    pub before: SanitizationInventory,
    /// Categories present in the output after sanitization.
    pub after: SanitizationInventory,
    /// Verification of the selected categories against the output inventory.
    pub verification: SanitizationReport,
}

impl AppliedSanitizationReport {
    /// Return `true` when no selected category remains in the output.
    pub fn is_clean(&self) -> bool {
        self.verification.is_clean()
    }

    /// Enforce the verification policy selected for the run.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::SanitizationVerificationFailed`] in strict mode
    /// when a selected category is still present.
    pub fn check(&self) -> Result<()> {
        self.verification.check()
    }
}

/// Apply `sanitizer` to `source` and return sanitized bytes plus a report.
///
/// The source document is borrowed immutably and left untouched. Actions are
/// applied in the plan's canonical order and verification is performed on the
/// re-parsed output bytes.
///
/// # Errors
///
/// Returns a typed error for a damaged or unsupported source document, a
/// serialization failure, or (in strict mode) a residual selected category.
/// No document contents are included in any error.
pub fn sanitize_document(
    source: &Document,
    sanitizer: &Sanitizer,
    strict: bool,
) -> Result<SanitizedDocument> {
    let before = inventory_from_document(source)?;
    let actions = sanitizer.plan();
    let root = source.root().map_err(core_error)?;
    let pages = source.pages().map_err(core_error)?;
    let info_id = trailer_reference(source.bytes(), "Info");

    let mut objects = load_objects(source)?;
    let mut applied = Vec::new();
    for action in &actions {
        if apply_action(action, &mut objects, root, &pages, info_id) {
            applied.push(action.clone());
        }
    }

    let mut trailer = Dictionary::new();
    if let Some(id) = trailer_id(source.bytes()) {
        trailer.insert("ID".to_owned(), id);
    }
    let bytes = write_pdf(&objects, root, &trailer)?;

    let output = Document::from_bytes(bytes.clone()).map_err(core_error)?;
    let after = inventory_from_document(&output)?;
    let verification = SanitizationReport::verify(sanitizer, &after, strict);
    verification.check()?;

    Ok(SanitizedDocument {
        bytes,
        report: AppliedSanitizationReport {
            actions,
            applied,
            before,
            after,
            verification,
        },
    })
}

// ---------------------------------------------------------------------------
// SEC-008: extended hidden-information removal
// ---------------------------------------------------------------------------

/// A class of hidden information that can be removed from a concrete PDF.
///
/// Declaration order is canonical for inventories and reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum HiddenInfoClass {
    /// Trailer `/Info` dictionary (author, title, subject, and so on).
    DocumentInfo,
    /// Catalog `/Metadata` XMP stream.
    XmpMetadata,
    /// Embedded-file name tree and its file-spec/stream objects.
    Attachments,
    /// Document- and page-level JavaScript, including name trees and `/AA`.
    JavaScript,
    /// Page annotations (comments, notes, and their appearance streams).
    Annotations,
    /// Page thumbnail images (`/Thumb`).
    Thumbnails,
    /// Private application data blocks (`/PieceInfo`).
    PieceInfo,
    /// Vendor-prefixed private keys and `/LastModified` in catalog, info, and
    /// page dictionaries.
    PrivateAppData,
    /// Interactive form field values (`/V`, `/DV`, `/AP`, `/AS`).
    FormValues,
}

impl HiddenInfoClass {
    /// Every class in canonical order.
    pub const ALL: [Self; 9] = [
        Self::DocumentInfo,
        Self::XmpMetadata,
        Self::Attachments,
        Self::JavaScript,
        Self::Annotations,
        Self::Thumbnails,
        Self::PieceInfo,
        Self::PrivateAppData,
        Self::FormValues,
    ];

    /// Stable string identifier.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::DocumentInfo => "document_info",
            Self::XmpMetadata => "xmp_metadata",
            Self::Attachments => "attachments",
            Self::JavaScript => "javascript",
            Self::Annotations => "annotations",
            Self::Thumbnails => "thumbnails",
            Self::PieceInfo => "piece_info",
            Self::PrivateAppData => "private_app_data",
            Self::FormValues => "form_values",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|class| class.as_id() == id)
    }
}

impl fmt::Display for HiddenInfoClass {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// Default prefix used to recognise vendor-private dictionary keys.
const DEFAULT_PRIVATE_KEY_PREFIX: &str = "Private";

/// A deterministic selection of hidden-information classes to remove.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenInfoSelection {
    classes: BTreeSet<HiddenInfoClass>,
    private_key_prefix: String,
}

impl Default for HiddenInfoSelection {
    fn default() -> Self {
        Self::empty()
    }
}

impl HiddenInfoSelection {
    /// Select every known hidden-information class.
    pub fn all() -> Self {
        Self {
            classes: HiddenInfoClass::ALL.into_iter().collect(),
            private_key_prefix: DEFAULT_PRIVATE_KEY_PREFIX.to_owned(),
        }
    }

    /// Select nothing, with the default private-key prefix.
    pub fn empty() -> Self {
        Self {
            classes: BTreeSet::new(),
            private_key_prefix: DEFAULT_PRIVATE_KEY_PREFIX.to_owned(),
        }
    }

    /// Build a selection from an explicit set of classes.
    pub fn new(classes: impl IntoIterator<Item = HiddenInfoClass>) -> Self {
        Self {
            classes: classes.into_iter().collect(),
            private_key_prefix: DEFAULT_PRIVATE_KEY_PREFIX.to_owned(),
        }
    }

    /// Add a class to the selection.
    pub fn include(&mut self, class: HiddenInfoClass) {
        self.classes.insert(class);
    }

    /// Remove a class from the selection.
    pub fn exclude(&mut self, class: HiddenInfoClass) {
        self.classes.remove(&class);
    }

    /// Whether `class` is selected.
    pub fn contains(&self, class: HiddenInfoClass) -> bool {
        self.classes.contains(&class)
    }

    /// Selected classes in canonical order.
    pub fn classes(&self) -> impl Iterator<Item = HiddenInfoClass> + '_ {
        self.classes.iter().copied()
    }

    /// Number of selected classes.
    pub fn len(&self) -> usize {
        self.classes.len()
    }

    /// Whether nothing is selected.
    pub fn is_empty(&self) -> bool {
        self.classes.is_empty()
    }

    /// Vendor-private key prefix used for detection and removal.
    pub fn private_key_prefix(&self) -> &str {
        &self.private_key_prefix
    }

    /// Set the vendor-private key prefix.
    ///
    /// # Errors
    ///
    /// Rejects an empty prefix or one containing PDF name delimiters with
    /// [`SecurityError::Configuration`].
    pub fn set_private_key_prefix(&mut self, prefix: impl Into<String>) -> Result<()> {
        let prefix = prefix.into();
        if prefix.is_empty()
            || prefix
                .bytes()
                .any(|byte| byte.is_ascii_whitespace() || b"/[]()<>{}%".contains(&byte))
        {
            return Err(SecurityError::Configuration(
                "private key prefix must be a non-empty PDF name fragment".to_owned(),
            ));
        }
        self.private_key_prefix = prefix;
        Ok(())
    }

    /// Expand the selection into a deterministic, ordered action list.
    pub fn plan(&self) -> Vec<HiddenInfoAction> {
        let mut actions = Vec::new();
        for class in &self.classes {
            for target in targets_for(*class) {
                actions.push(HiddenInfoAction::new(*class, target));
            }
        }
        actions.sort();
        actions
    }
}

/// Canonical target keys for a hidden-information class.
fn targets_for(class: HiddenInfoClass) -> &'static [&'static str] {
    match class {
        HiddenInfoClass::DocumentInfo => &["trailer/Info"],
        HiddenInfoClass::XmpMetadata => &["Catalog/Metadata"],
        HiddenInfoClass::Attachments => &["Names/EmbeddedFiles", "Objects/Filespec"],
        HiddenInfoClass::JavaScript => &[
            "Names/JavaScript",
            "Catalog/OpenAction",
            "Catalog/AA",
            "Page/AA",
            "Objects/JavaScript",
        ],
        HiddenInfoClass::Annotations => &["Page/Annots"],
        HiddenInfoClass::Thumbnails => &["Page/Thumb"],
        HiddenInfoClass::PieceInfo => &["Catalog/PieceInfo", "Page/PieceInfo"],
        HiddenInfoClass::PrivateAppData => &["Dictionary/PrivateKeys", "Dictionary/LastModified"],
        HiddenInfoClass::FormValues => &["AcroForm/FieldValues"],
    }
}

/// One deterministic step in an extended sanitization plan.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct HiddenInfoAction {
    class: HiddenInfoClass,
    target: &'static str,
}

impl HiddenInfoAction {
    /// Build an action from a class and stable target identifier.
    pub const fn new(class: HiddenInfoClass, target: &'static str) -> Self {
        Self { class, target }
    }

    /// Class this action belongs to.
    pub const fn class(&self) -> HiddenInfoClass {
        self.class
    }

    /// Stable target identifier.
    pub const fn target(&self) -> &'static str {
        self.target
    }
}

/// A concrete item that a sanitization run removed.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RemovedItem {
    class: HiddenInfoClass,
    target: String,
}

impl RemovedItem {
    /// Build a removed-item record.
    pub fn new(class: HiddenInfoClass, target: impl Into<String>) -> Self {
        Self {
            class,
            target: target.into(),
        }
    }

    /// Class of the removed item.
    pub const fn class(&self) -> HiddenInfoClass {
        self.class
    }

    /// Stable identifier of the removed item.
    pub fn target(&self) -> &str {
        &self.target
    }
}

fn is_private_key(key: &str, prefix: &str) -> bool {
    key == "LastModified" || key.starts_with(prefix)
}

fn dictionary_private_keys(dictionary: &Dictionary, prefix: &str) -> Vec<String> {
    dictionary
        .keys()
        .filter(|key| is_private_key(key, prefix))
        .cloned()
        .collect()
}

/// Derive a hidden-information inventory from a real parsed object graph.
///
/// The private-key prefix comes from `selection`, so detection and removal use
/// the exact same criterion.
pub fn hidden_info_inventory(
    document: &Document,
    selection: &HiddenInfoSelection,
) -> Result<BTreeSet<HiddenInfoClass>> {
    let objects = load_objects(document)?;
    let root = document.root().map_err(core_error)?;
    let mut present = BTreeSet::new();
    let prefix = selection.private_key_prefix();

    if trailer_reference(document.bytes(), "Info")
        .and_then(|id| find_object(&objects, id))
        .and_then(dictionary_of)
        .is_some_and(|dictionary| !dictionary.is_empty())
    {
        present.insert(HiddenInfoClass::DocumentInfo);
    }
    if catalog_has(&objects, root, "Metadata") {
        present.insert(HiddenInfoClass::XmpMetadata);
    }
    if nested_has(&objects, root, "Names", "EmbeddedFiles")
        || objects.iter().any(|(_, object)| {
            dictionary_of(object).is_some_and(|dictionary| {
                dictionary.get("Type").and_then(Object::as_name) == Some("Filespec")
                    || dictionary.contains_key("EmbeddedFile")
                    || dictionary.get("Type").and_then(Object::as_name) == Some("EmbeddedFile")
            })
        })
    {
        present.insert(HiddenInfoClass::Attachments);
    }
    let open_action_has_js = find_object(&objects, root)
        .and_then(dictionary_of)
        .and_then(|dictionary| dictionary.get("OpenAction"))
        .and_then(|value| match value {
            Object::Reference(id) => find_object(&objects, *id),
            other => Some(other),
        })
        .and_then(dictionary_of)
        .is_some_and(|dictionary| {
            dictionary.contains_key("JS")
                || dictionary.get("S").and_then(Object::as_name) == Some("JavaScript")
        });
    if nested_has(&objects, root, "Names", "JavaScript")
        || open_action_has_js
        || catalog_has(&objects, root, "AA")
        || objects.iter().any(|(_, object)| {
            dictionary_of(object).is_some_and(|dictionary| {
                dictionary.get("S").and_then(Object::as_name) == Some("JavaScript")
                    || dictionary.get("Type").and_then(Object::as_name) == Some("JavaScript")
                    || dictionary.contains_key("AA")
            })
        })
    {
        present.insert(HiddenInfoClass::JavaScript);
    }
    let pages = document.pages().map_err(core_error)?;
    if pages.iter().any(|page| {
        find_object(&objects, page.id)
            .and_then(dictionary_of)
            .is_some_and(|dictionary| non_empty_array(dictionary, "Annots"))
    }) {
        present.insert(HiddenInfoClass::Annotations);
    }
    if pages.iter().any(|page| {
        find_object(&objects, page.id)
            .and_then(dictionary_of)
            .is_some_and(|dictionary| dictionary.contains_key("Thumb"))
    }) {
        present.insert(HiddenInfoClass::Thumbnails);
    }
    if catalog_has(&objects, root, "PieceInfo")
        || pages.iter().any(|page| {
            find_object(&objects, page.id)
                .and_then(dictionary_of)
                .is_some_and(|dictionary| dictionary.contains_key("PieceInfo"))
        })
    {
        present.insert(HiddenInfoClass::PieceInfo);
    }
    let mut private_sources: Vec<&Dictionary> = Vec::new();
    if let Some(catalog) = find_object(&objects, root).and_then(dictionary_of) {
        private_sources.push(catalog);
    }
    if let Some(info) = trailer_reference(document.bytes(), "Info")
        .and_then(|id| find_object(&objects, id))
        .and_then(dictionary_of)
    {
        private_sources.push(info);
    }
    for page in &pages {
        if let Some(dictionary) = find_object(&objects, page.id).and_then(dictionary_of) {
            private_sources.push(dictionary);
        }
    }
    if private_sources
        .iter()
        .any(|dictionary| !dictionary_private_keys(dictionary, prefix).is_empty())
    {
        present.insert(HiddenInfoClass::PrivateAppData);
    }
    if objects.iter().any(|(_, object)| {
        dictionary_of(object).is_some_and(|dictionary| {
            dictionary.contains_key("FT")
                && dictionary.get("V").is_some_and(|value| {
                    *value != Object::Null
                        && !matches!(value, Object::String(bytes) if bytes.is_empty())
                })
        })
    }) {
        present.insert(HiddenInfoClass::FormValues);
    }

    Ok(present)
}

/// Remove all keys `owner` holds that are identified as private app data.
fn remove_private_keys(
    objects: &mut [(ObjectId, Object)],
    owner: ObjectId,
    prefix: &str,
    label: &str,
    removed: &mut BTreeSet<RemovedItem>,
    class: HiddenInfoClass,
) {
    let keys = dict_ref(objects, owner)
        .map(|dictionary| dictionary_private_keys(dictionary, prefix))
        .unwrap_or_default();
    for key in keys {
        neutralize_child(objects, owner, &key);
        if dict_mut(objects, owner).is_some_and(|dictionary| dictionary.remove(&key).is_some()) {
            removed.insert(RemovedItem::new(class, format!("{label}/{key}")));
        }
    }
}

fn apply_hidden_action(
    action: &HiddenInfoAction,
    objects: &mut [(ObjectId, Object)],
    root: ObjectId,
    pages: &[Page],
    info_id: Option<ObjectId>,
    selection: &HiddenInfoSelection,
    removed: &mut BTreeSet<RemovedItem>,
) {
    let class = action.class();
    match action.target() {
        "trailer/Info" => {
            if let Some(index) = info_id.and_then(|id| object_index(objects, id)) {
                objects[index].1 = Object::Dictionary(Dictionary::new());
                removed.insert(RemovedItem::new(class, "trailer/Info"));
            }
        }
        "Catalog/Metadata" => {
            if detach_catalog_key(objects, root, "Metadata") {
                removed.insert(RemovedItem::new(class, "Catalog/Metadata"));
            }
        }
        "Names/EmbeddedFiles" => {
            if remove_names_key(objects, root, "EmbeddedFiles") {
                removed.insert(RemovedItem::new(class, "Names/EmbeddedFiles"));
            }
        }
        "Objects/Filespec" => {
            let victims: Vec<ObjectId> = objects
                .iter()
                .filter(|(_, object)| {
                    dictionary_of(object).is_some_and(|dictionary| {
                        matches!(
                            dictionary.get("Type").and_then(Object::as_name),
                            Some("Filespec" | "EmbeddedFile")
                        ) || dictionary.contains_key("EmbeddedFile")
                    })
                })
                .map(|(id, _)| *id)
                .collect();
            for id in victims {
                if let Some(index) = object_index(objects, id) {
                    objects[index].1 = Object::Null;
                    removed.insert(RemovedItem::new(class, format!("Object/{id}")));
                }
            }
        }
        "Names/JavaScript" => {
            if remove_names_key(objects, root, "JavaScript") {
                removed.insert(RemovedItem::new(class, "Names/JavaScript"));
            }
        }
        "Catalog/OpenAction" => {
            if detach_catalog_key(objects, root, "OpenAction") {
                removed.insert(RemovedItem::new(class, "Catalog/OpenAction"));
            }
        }
        "Catalog/AA" => {
            if detach_catalog_key(objects, root, "AA") {
                removed.insert(RemovedItem::new(class, "Catalog/AA"));
            }
        }
        "Page/AA" => {
            for page in pages {
                let id = page.id;
                if let Some(index) = object_index(objects, id) {
                    neutralize_child(objects, id, "AA");
                    if let Object::Dictionary(dictionary) = &mut objects[index].1
                        && dictionary.remove("AA").is_some()
                    {
                        removed.insert(RemovedItem::new(class, format!("Page/{id}/AA")));
                    }
                }
            }
        }
        "Objects/JavaScript" => {
            let victims: Vec<ObjectId> = objects
                .iter()
                .filter(|(_, object)| {
                    dictionary_of(object).is_some_and(|dictionary| {
                        dictionary.get("S").and_then(Object::as_name) == Some("JavaScript")
                            || dictionary.get("Type").and_then(Object::as_name)
                                == Some("JavaScript")
                    })
                })
                .map(|(id, _)| *id)
                .collect();
            for id in victims {
                if let Some(index) = object_index(objects, id) {
                    objects[index].1 = Object::Null;
                    removed.insert(RemovedItem::new(class, format!("Object/{id}")));
                }
            }
        }
        "Page/Annots" => {
            for page in pages {
                let id = page.id;
                let annot_refs: Vec<ObjectId> = dict_ref(objects, id)
                    .and_then(|dictionary| dictionary.get("Annots"))
                    .and_then(Object::as_array)
                    .map(|array| array.iter().filter_map(Object::as_reference).collect())
                    .unwrap_or_default();
                for annot in annot_refs {
                    if let Some(index) = object_index(objects, annot) {
                        objects[index].1 = Object::Null;
                    }
                }
                if let Some(index) = object_index(objects, id)
                    && let Object::Dictionary(dictionary) = &mut objects[index].1
                    && dictionary.remove("Annots").is_some()
                {
                    removed.insert(RemovedItem::new(class, format!("Page/{id}/Annots")));
                }
            }
        }
        "Page/Thumb" => {
            for page in pages {
                let id = page.id;
                if let Some(index) = object_index(objects, id) {
                    neutralize_child(objects, id, "Thumb");
                    if let Object::Dictionary(dictionary) = &mut objects[index].1
                        && dictionary.remove("Thumb").is_some()
                    {
                        removed.insert(RemovedItem::new(class, format!("Page/{id}/Thumb")));
                    }
                }
            }
        }
        "Catalog/PieceInfo" => {
            if detach_catalog_key(objects, root, "PieceInfo") {
                removed.insert(RemovedItem::new(class, "Catalog/PieceInfo"));
            }
        }
        "Page/PieceInfo" => {
            for page in pages {
                let id = page.id;
                if let Some(index) = object_index(objects, id) {
                    neutralize_child(objects, id, "PieceInfo");
                    if let Object::Dictionary(dictionary) = &mut objects[index].1
                        && dictionary.remove("PieceInfo").is_some()
                    {
                        removed.insert(RemovedItem::new(class, format!("Page/{id}/PieceInfo")));
                    }
                }
            }
        }
        "Dictionary/PrivateKeys" => {
            remove_private_keys(
                objects,
                root,
                selection.private_key_prefix(),
                "Catalog",
                removed,
                class,
            );
            if let Some(info) = info_id {
                remove_private_keys(
                    objects,
                    info,
                    selection.private_key_prefix(),
                    "Info",
                    removed,
                    class,
                );
            }
            for page in pages {
                remove_private_keys(
                    objects,
                    page.id,
                    selection.private_key_prefix(),
                    "Page",
                    removed,
                    class,
                );
            }
        }
        "Dictionary/LastModified" => {
            // `LastModified` is already covered by `is_private_key`, but keep a
            // dedicated deterministic action so an explicit selection is clear.
            for owner in std::iter::once(root).chain(pages.iter().map(|page| page.id)) {
                let keys = dict_ref(objects, owner)
                    .map(|dictionary| {
                        dictionary
                            .keys()
                            .filter(|key| key.as_str() == "LastModified")
                            .cloned()
                            .collect::<Vec<_>>()
                    })
                    .unwrap_or_default();
                for key in keys {
                    neutralize_child(objects, owner, &key);
                    if dict_mut(objects, owner)
                        .is_some_and(|dictionary| dictionary.remove(&key).is_some())
                    {
                        removed.insert(RemovedItem::new(
                            class,
                            format!("Object/{owner}/LastModified"),
                        ));
                    }
                }
            }
        }
        "AcroForm/FieldValues" => {
            for (id, object) in objects.iter_mut() {
                if let Object::Dictionary(dictionary) = object
                    && dictionary.contains_key("FT")
                {
                    for key in ["V", "DV", "AP", "AS"] {
                        if dictionary.remove(key).is_some() {
                            removed.insert(RemovedItem::new(class, format!("Object/{id}/{key}")));
                        }
                    }
                }
            }
        }
        _ => {}
    }
}

/// A deterministic report of an extended hidden-information sanitization run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HiddenInfoReport {
    /// The selection that was applied.
    pub selection: HiddenInfoSelection,
    /// The full deterministic action plan.
    pub planned: Vec<HiddenInfoAction>,
    /// Concrete items removed, in canonical order.
    pub removed: Vec<RemovedItem>,
    /// Classes present in the source before the run.
    pub before: BTreeSet<HiddenInfoClass>,
    /// Classes present in the re-parsed output after the run.
    pub after: BTreeSet<HiddenInfoClass>,
}

impl HiddenInfoReport {
    /// Selected classes still present after sanitization, in canonical order.
    pub fn residuals(&self) -> Vec<HiddenInfoClass> {
        self.selection
            .classes()
            .filter(|class| self.after.contains(class))
            .collect()
    }

    /// Whether no selected class remains.
    pub fn is_clean(&self) -> bool {
        self.residuals().is_empty()
    }

    /// Enforce strict verification.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::HiddenInfoResidual`] for the first residual
    /// class when the caller requested strict verification.
    pub fn check(&self, strict: bool) -> Result<()> {
        if strict && let Some(class) = self.residuals().first() {
            return Err(SecurityError::HiddenInfoResidual(class.as_id()));
        }
        Ok(())
    }
}

/// The owned result of an extended hidden-information sanitization.
#[derive(Debug, Clone)]
pub struct HiddenInfoSanitized {
    bytes: Vec<u8>,
    report: HiddenInfoReport,
}

impl HiddenInfoSanitized {
    /// Sanitized, standalone PDF bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Deterministic report of removed items and verification.
    pub fn report(&self) -> &HiddenInfoReport {
        &self.report
    }

    /// Re-parse the sanitized bytes with the core parser.
    ///
    /// # Errors
    ///
    /// Returns a typed error if the produced bytes cannot be parsed.
    pub fn document(&self) -> Result<Document> {
        Document::from_bytes(self.bytes.clone()).map_err(core_error)
    }
}

/// Apply `selection` to `source`, remove hidden information structurally, and
/// verify the re-parsed output.
///
/// This extends [`sanitize_document`] with the classes it did not cover:
/// thumbnails, private application data, and additional JavaScript locations,
/// while accepting a vendor-private key prefix. Referenced child objects are
/// nulled before their keys are detached, so information is removed rather
/// than merely hidden behind an orphaned object.
///
/// # Errors
///
/// Returns a typed error for a damaged source, a serialization failure, or (in
/// strict mode) a residual selected class. Document contents are never
/// embedded in an error.
pub fn sanitize_hidden_info(
    source: &Document,
    selection: &HiddenInfoSelection,
    strict: bool,
) -> Result<HiddenInfoSanitized> {
    let before = hidden_info_inventory(source, selection)?;
    let planned = selection.plan();
    let root = source.root().map_err(core_error)?;
    let pages = source.pages().map_err(core_error)?;
    let info_id = trailer_reference(source.bytes(), "Info");

    let mut objects = load_objects(source)?;
    let mut removed = BTreeSet::new();
    for action in &planned {
        apply_hidden_action(
            action,
            &mut objects,
            root,
            &pages,
            info_id,
            selection,
            &mut removed,
        );
    }

    let mut trailer = Dictionary::new();
    if let Some(id) = trailer_id(source.bytes()) {
        trailer.insert("ID".to_owned(), id);
    }
    // Preserve the /Info reference unless the caller selected its removal:
    // a targeted scrub must not drop structures the caller left untouched.
    if !selection.contains(HiddenInfoClass::DocumentInfo)
        && let Some(info) = info_id
    {
        trailer.insert("Info".to_owned(), Object::Reference(info));
    }
    let bytes = write_pdf(&objects, root, &trailer)?;

    let output = Document::from_bytes(bytes.clone()).map_err(core_error)?;
    let after = hidden_info_inventory(&output, selection)?;
    let report = HiddenInfoReport {
        selection: selection.clone(),
        planned,
        removed: removed.into_iter().collect(),
        before,
        after,
    };
    report.check(strict)?;

    Ok(HiddenInfoSanitized { bytes, report })
}

#[cfg(test)]
mod extended_tests {
    use super::*;

    const ID_HEX: &str = "00112233445566778899AABBCCDDEEFF";

    fn build_pdf(objects: &[&str]) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R /ID [<{ID_HEX}> <{ID_HEX}>] >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// All nine hidden-information classes in one classic-xref document.
    fn full_fixture() -> Vec<u8> {
        build_pdf(&[
            "<< /Type /Catalog /Pages 2 0 R /Names 5 0 R /Metadata 7 0 R /OpenAction 10 0 R /AcroForm << /Fields [11 0 R] >> /PieceInfo 12 0 R /AA << /WC 10 0 R >> /LastModified (D:20200101) /PrivateTag (secret private) >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [9 0 R] /Thumb 13 0 R /PieceInfo 14 0 R /AA << /O 10 0 R >> /PrivateNote (secret page) >>",
            "<< /Length 23 >>\nstream\nBT (Visible text) Tj ET\nendstream",
            "<< /EmbeddedFiles 8 0 R /JavaScript 15 0 R >>",
            "<< /Title (Secret Title) /PrivateTag (secret info) >>",
            "<< /Type /Metadata /Subtype /XML /Length 17 >>\nstream\n<xmp>secret</xmp>\nendstream",
            "<< /Names [(secret.txt) 16 0 R] >>",
            "<< /Type /Annot /Subtype /Text /Contents (hidden note) >>",
            "<< /S /JavaScript /JS (app.alert\\(1\\)) >>",
            "<< /FT /Tx /T (field1) /V (secret value) >>",
            "<< /Private 12 0 R >>",
            "<< /Type /XObject /Subtype /Image /Length 3 >>\nstream\nJPG\nendstream",
            "<< /Private 14 0 R >>",
            "<< /Names [(JS) 10 0 R] >>",
            "<< /Type /Filespec /F (secret.txt) /EF << /F 17 0 R >> >>",
            "<< /Type /EmbeddedFile /Length 5 >>\nstream\nbytes\nendstream",
        ])
    }

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes.to_vec()).expect("fixture parses")
    }

    #[test]
    fn hidden_info_identifiers_round_trip() {
        for class in HiddenInfoClass::ALL {
            assert_eq!(HiddenInfoClass::from_id(class.as_id()), Some(class));
        }
        assert_eq!(HiddenInfoClass::from_id("unknown"), None);
        assert_eq!(HiddenInfoClass::Thumbnails.to_string(), "thumbnails");
    }

    #[test]
    fn full_selection_removes_every_hidden_class_on_real_bytes() {
        let source = doc(&full_fixture());
        let selection = HiddenInfoSelection::all();
        let before = hidden_info_inventory(&source, &selection).expect("before inventory");
        assert_eq!(before.len(), HiddenInfoClass::ALL.len());

        let result = sanitize_hidden_info(&source, &selection, true).expect("sanitize");
        let report = result.report();
        assert!(report.is_clean());
        assert!(report.check(true).is_ok());
        assert!(report.after.is_empty());
        assert_eq!(report.before.len(), HiddenInfoClass::ALL.len());
        assert!(!report.removed.is_empty());
        assert!(report.removed.windows(2).all(|pair| pair[0] <= pair[1]));
        assert!(report.planned.windows(2).all(|pair| pair[0] <= pair[1]));

        let output = result.document().expect("output parses");
        assert!(
            hidden_info_inventory(&output, &selection)
                .expect("after inventory")
                .is_empty()
        );
        assert_eq!(output.page_text(0).expect("text"), "Visible text");
    }

    #[test]
    fn removal_is_structural_not_cosmetic() {
        let source = doc(&full_fixture());
        let result =
            sanitize_hidden_info(&source, &HiddenInfoSelection::all(), true).expect("sanitize");
        let bytes = result.bytes();
        for needle in [
            &b"Secret Title"[..],
            &b"secret.txt"[..],
            &b"hidden note"[..],
            &b"app.alert"[..],
            &b"secret value"[..],
            &b"secret private"[..],
            &b"secret page"[..],
            &b"secret info"[..],
        ] {
            assert!(
                !bytes.windows(needle.len()).any(|w| w == needle),
                "removed bytes must not survive in the output"
            );
        }
    }

    #[test]
    fn targeted_selection_leaves_unselected_classes() {
        let source = doc(&full_fixture());
        let mut selection = HiddenInfoSelection::empty();
        selection.include(HiddenInfoClass::Thumbnails);
        let result = sanitize_hidden_info(&source, &selection, true).expect("thumbnails only");
        let output = result.document().expect("output parses");
        let after = hidden_info_inventory(&output, &selection).expect("after");
        assert!(!after.contains(&HiddenInfoClass::Thumbnails));
        let full = hidden_info_inventory(&output, &HiddenInfoSelection::all()).expect("full");
        assert!(full.contains(&HiddenInfoClass::DocumentInfo));
        assert!(full.contains(&HiddenInfoClass::Attachments));
        assert!(full.contains(&HiddenInfoClass::FormValues));
    }

    #[test]
    fn report_is_deterministic_and_source_is_unchanged() {
        let bytes = full_fixture();
        let source = doc(&bytes);
        let original = source.bytes().to_vec();
        let selection = HiddenInfoSelection::all();
        let first = sanitize_hidden_info(&source, &selection, true).expect("first");
        let second = sanitize_hidden_info(&source, &selection, true).expect("second");
        assert_eq!(first.bytes(), second.bytes());
        assert_eq!(first.report(), second.report());
        assert_eq!(source.bytes(), original.as_slice());
    }

    #[test]
    fn strict_residual_is_a_typed_error() {
        let report = HiddenInfoReport {
            selection: HiddenInfoSelection::new([HiddenInfoClass::Thumbnails]),
            planned: Vec::new(),
            removed: Vec::new(),
            before: BTreeSet::new(),
            after: BTreeSet::from([HiddenInfoClass::Thumbnails]),
        };
        assert!(!report.is_clean());
        assert_eq!(
            report.check(true),
            Err(SecurityError::HiddenInfoResidual("thumbnails"))
        );
        assert_eq!(report.check(false), Ok(()));
    }

    #[test]
    fn private_key_prefix_is_configurable_and_validated() {
        let mut selection = HiddenInfoSelection::empty();
        assert_eq!(selection.private_key_prefix(), "Private");
        selection
            .set_private_key_prefix("Custom")
            .expect("valid prefix");
        assert_eq!(selection.private_key_prefix(), "Custom");
        assert!(selection.set_private_key_prefix("").is_err());
        assert!(selection.set_private_key_prefix("bad/prefix").is_err());

        let source = doc(&full_fixture());
        let mut custom = HiddenInfoSelection::new([HiddenInfoClass::PrivateAppData]);
        custom
            .set_private_key_prefix("Secret")
            .expect("valid prefix");
        // With prefix "Secret" only /LastModified matches in the fixture.
        let before = hidden_info_inventory(&source, &custom).expect("inventory");
        assert!(before.contains(&HiddenInfoClass::PrivateAppData));
        let result = sanitize_hidden_info(&source, &custom, true).expect("sanitize");
        let output = result.document().expect("output parses");
        assert!(
            !hidden_info_inventory(&output, &custom)
                .expect("after")
                .contains(&HiddenInfoClass::PrivateAppData)
        );
    }

    #[test]
    fn empty_selection_is_a_no_op() {
        let source = doc(&full_fixture());
        let selection = HiddenInfoSelection::empty();
        let result = sanitize_hidden_info(&source, &selection, true).expect("noop");
        assert!(result.report().planned.is_empty());
        assert!(result.report().removed.is_empty());
        assert_eq!(result.report().before.len(), HiddenInfoClass::ALL.len());
        assert_eq!(result.report().after, result.report().before);
        assert!(result.report().is_clean());
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::sanitization::SanitizationCategory;

    const ID_HEX: &str = "00112233445566778899AABBCCDDEEFF";

    /// Assemble a classic-xref PDF with accurate offsets from object bodies.
    fn build_pdf(objects: &[&str], with_id: bool) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let id = if with_id {
            format!(" /ID [<{ID_HEX}> <{ID_HEX}>]")
        } else {
            String::new()
        };
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 6 0 R{id} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A one-page document carrying every sanitizable category.
    /// Object numbers: 1 catalog, 2 pages, 3 page, 4 content, 5 names,
    /// 6 info, 7 xmp, 8 embedded stream, 9 annot, 10 js action, 11 field.
    fn rich_fixture() -> Vec<u8> {
        build_pdf(
            &[
                "<< /Type /Catalog /Pages 2 0 R /Names 5 0 R /Metadata 7 0 R /OpenAction 10 0 R /AcroForm << /Fields [11 0 R] >> >>",
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R /Annots [9 0 R] >>",
                "<< /Length 23 >>\nstream\nBT (Visible text) Tj ET\nendstream",
                "<< /EmbeddedFiles 8 0 R /JavaScript 10 0 R >>",
                "<< /Title (Secret Title) /Author (Secret Author) >>",
                "<< /Type /Metadata /Subtype /XML /Length 17 >>\nstream\n<xmp>secret</xmp>\nendstream",
                "<< /Type /Filespec /F (secret.txt) /EmbeddedFile null >>",
                "<< /Type /Annot /Subtype /Text /Contents (hidden note) >>",
                "<< /S /JavaScript /JS (app.alert\\(1\\)) >>",
                "<< /FT /Tx /T (field1) /V (secret value) >>",
            ],
            true,
        )
    }

    fn doc(bytes: &[u8]) -> Document {
        Document::from_bytes(bytes.to_vec()).expect("fixture parses")
    }

    #[test]
    fn full_sanitization_removes_every_category_on_real_bytes() {
        let source = doc(&rich_fixture());
        let before = inventory_from_document(&source).expect("before inventory");
        assert_eq!(before.len(), 5, "fixture should carry all five categories");

        let result = sanitize_document(&source, &Sanitizer::all(), true).expect("sanitize");
        let report = result.report();
        assert!(report.is_clean());
        assert!(report.check().is_ok());
        assert_eq!(report.actions.len(), 7);
        assert_eq!(report.after.len(), 0);
        for action in &report.actions {
            assert!(report.applied.contains(action));
        }
        assert!(report.actions.windows(2).all(|pair| pair[0] <= pair[1]));

        let output = result.document().expect("output parses");
        assert!(
            inventory_from_document(&output)
                .expect("after inventory")
                .is_empty()
        );
        assert_eq!(output.page_text(0).expect("text"), "Visible text");
    }

    #[test]
    fn targeted_sanitization_leaves_unselected_categories() {
        let source = doc(&rich_fixture());
        let mut sanitizer = Sanitizer::empty();
        sanitizer.include(SanitizationCategory::Metadata);
        let result = sanitize_document(&source, &sanitizer, true).expect("sanitize metadata only");
        let output = result.document().expect("output parses");
        let after = inventory_from_document(&output).expect("after inventory");
        assert!(!after.contains(SanitizationCategory::Metadata));
        assert!(after.contains(SanitizationCategory::Annotations));
        assert!(after.contains(SanitizationCategory::JavaScript));
        assert!(after.contains(SanitizationCategory::FormValues));
        assert!(result.report().is_clean());
    }

    #[test]
    fn source_is_unchanged_after_copy_on_write_sanitize() {
        let bytes = rich_fixture();
        let source = doc(&bytes);
        let original = source.bytes().to_vec();
        let _ = sanitize_document(&source, &Sanitizer::all(), true).expect("sanitize");
        assert_eq!(source.bytes(), original.as_slice());
        let inventory = inventory_from_document(&source).expect("inventory");
        assert_eq!(inventory.len(), 5);
    }

    #[test]
    fn sanitization_is_deterministic() {
        let source = doc(&rich_fixture());
        let a = sanitize_document(&source, &Sanitizer::all(), true).expect("first");
        let b = sanitize_document(&source, &Sanitizer::all(), true).expect("second");
        assert_eq!(a.bytes(), b.bytes());
        assert_eq!(a.report(), b.report());
    }

    #[test]
    fn empty_plan_is_a_no_op_with_empty_applied_list() {
        let source = doc(&rich_fixture());
        let result = sanitize_document(&source, &Sanitizer::empty(), true).expect("noop");
        assert!(result.report().actions.is_empty());
        assert!(result.report().applied.is_empty());
        assert!(result.report().is_clean());
        let output = result.document().expect("output parses");
        assert_eq!(
            inventory_from_document(&output).expect("inventory"),
            inventory_from_document(&source).expect("source inventory")
        );
    }

    #[test]
    fn strict_residual_is_a_typed_error() {
        let report = AppliedSanitizationReport {
            actions: Vec::new(),
            applied: Vec::new(),
            before: SanitizationInventory::empty(),
            after: SanitizationInventory::new([SanitizationCategory::Metadata]),
            verification: SanitizationReport::verify(
                &Sanitizer::all(),
                &SanitizationInventory::new([SanitizationCategory::Metadata]),
                true,
            ),
        };
        assert_eq!(
            report.check(),
            Err(SecurityError::SanitizationVerificationFailed {
                category: SanitizationCategory::Metadata
            })
        );
    }

    #[test]
    fn damaged_source_returns_a_typed_core_error() {
        // A valid header with a deliberately broken xref offset leaves a stored
        // structural error on the handle, which the sanitizer maps to a typed
        // error rather than panicking.
        let mut bytes = rich_fixture();
        let marker = b"startxref\n";
        let position = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("marker");
        let digits_start = position + marker.len();
        if let Some(slot) = bytes.get_mut(digits_start) {
            *slot = b'X';
        }
        let source = doc(&bytes);
        let result = sanitize_document(&source, &Sanitizer::all(), true);
        assert!(matches!(
            result,
            Err(SecurityError::SanitizationCoreFailed(_))
        ));
    }
}
