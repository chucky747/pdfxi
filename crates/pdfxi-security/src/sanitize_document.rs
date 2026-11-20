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
fn trailer_reference(bytes: &[u8], key: &str) -> Option<ObjectId> {
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
