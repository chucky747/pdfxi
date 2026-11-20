//! Comments and markup on a real-world document through the public library API.
//!
//! Every test opens `fixtures/realworld/microsoft-excel-2019-sample.pdf` (a
//! hybrid cross-reference/object-stream file produced by Excel 2019), mutates it
//! through `pdfxi-edit`, saves, and reopens the result with `pdfxi-core` before
//! asserting. The first test also writes the annotated file to
//! `target/lanes/author/t0230-out/annotated.pdf` for an independent PDFium
//! check.

use std::path::{Path, PathBuf};

use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use pdfxi_edit::{
    AnnotationEditError, AnnotationState, AnnotationSubtype, Point, Rect, add_highlight,
    add_text_note, delete_annotation, export_xfdf, import_xfdf, list_annotations, reply_to,
    set_state, update_contents,
};

/// Read the real-world fixture.
fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::new(x0, y0, x1, y1).expect("test rectangle")
}

/// Return the indirect annotation references from one page's `/Annots` entry.
fn annotation_ids(document: &Document, page_index: usize) -> Vec<ObjectId> {
    let page = document.page(page_index).expect("page metadata");
    let page_object = document.object(page.id).expect("page object");
    let dictionary = page_object.as_dictionary().expect("page dictionary");
    let items = match dictionary.get("Annots") {
        None => return Vec::new(),
        Some(Object::Array(items)) => items.clone(),
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("annotation array object")
            .as_array()
            .expect("annotation array")
            .to_vec(),
        Some(other) => panic!("unexpected /Annots value: {other:?}"),
    };
    items
        .into_iter()
        .map(|item| item.as_reference().expect("annotation indirect reference"))
        .collect()
}

fn dictionary(document: &Document, id: ObjectId) -> Dictionary {
    document
        .object(id)
        .expect("annotation object")
        .as_dictionary()
        .expect("annotation dictionary")
        .clone()
}

fn annotation_by_id(document: &Document, page_index: usize, id: &str) -> (ObjectId, Dictionary) {
    for object in annotation_ids(document, page_index) {
        let dictionary = dictionary(document, object);
        if string_field(&dictionary, "NM").as_deref() == Some(id) {
            return (object, dictionary);
        }
    }
    panic!("annotation {id:?} not found on page {page_index}");
}

fn string_field(dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Some(pdfxi_core::decode_text_string(bytes)),
        _ => None,
    }
}

fn name_field<'a>(dictionary: &'a Dictionary, key: &str) -> Option<&'a str> {
    dictionary.get(key).and_then(Object::as_name)
}

fn number_field(dictionary: &Dictionary, key: &str) -> Option<f64> {
    dictionary.get(key).and_then(Object::as_f64)
}

/// Load an annotation's `/AP /N` stream dictionary and content bytes.
fn appearance(document: &Document, dictionary: &Dictionary) -> (Dictionary, Vec<u8>) {
    let reference = dictionary
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|appearance| appearance.get("N"))
        .and_then(Object::as_reference)
        .expect("AP /N reference");
    match document.object(reference).expect("appearance object") {
        Object::Stream { dictionary, data } => (dictionary, data),
        other => panic!("appearance is not a stream: {other:?}"),
    }
}

fn numbers(object: Option<&Object>) -> Vec<f64> {
    object
        .and_then(Object::as_array)
        .expect("numeric array")
        .iter()
        .map(|item| item.as_f64().expect("number"))
        .collect()
}

/// Where the orchestrator can pick up the annotated file for a PDFium check.
fn output_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("target/lanes/author/t0230-out/annotated.pdf")
}

#[test]
fn note_highlight_reply_and_resolve_survive_a_full_rewrite() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let original_text = document.page_text(0).expect("page 1 text before editing");

    let note = add_text_note(
        &mut document,
        0,
        Point::new(72.0, 700.0),
        "Please review <this> & that.",
        "Alice",
        "note-1",
    )
    .expect("add text note");
    let highlight = add_highlight(
        &mut document,
        0,
        &[
            rect(72.0, 600.0, 300.0, 616.0),
            rect(72.0, 618.0, 200.0, 630.0),
        ],
        "Key result",
        "Bob",
    )
    .expect("add highlight");
    let reply =
        reply_to(&mut document, 0, "note-1", "Looking now.", "Carol").expect("reply to note");
    set_state(&mut document, 0, "note-1", AnnotationState::Accepted).expect("resolve note");

    assert_eq!(note.subtype, AnnotationSubtype::Text);
    assert_eq!(highlight.subtype, AnnotationSubtype::Highlight);
    assert_ne!(reply.object, note.object);

    let views = list_annotations(&document, 0).expect("list annotations");
    assert_eq!(views.len(), 3);
    let note_view = views
        .iter()
        .find(|view| view.id == "note-1")
        .expect("note view");
    assert_eq!(note_view.contents, "Please review <this> & that.");
    assert_eq!(note_view.author, "Alice");
    assert_eq!(note_view.state.as_deref(), Some("Accepted"));
    let reply_view = views
        .iter()
        .find(|view| view.object == reply.object)
        .expect("reply view");
    assert_eq!(reply_view.parent_id.as_deref(), Some("note-1"));

    let saved = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(saved.clone()).expect("saved document parses");

    assert_eq!(
        reopened.page_text(0).expect("page 1 text after editing"),
        original_text,
        "annotations must not change page text"
    );
    assert_eq!(annotation_ids(&reopened, 0).len(), 3);

    let (note_object, note_dictionary) = annotation_by_id(&reopened, 0, "note-1");
    assert_eq!(name_field(&note_dictionary, "Type"), Some("Annot"));
    assert_eq!(name_field(&note_dictionary, "Subtype"), Some("Text"));
    assert_eq!(
        string_field(&note_dictionary, "Contents").as_deref(),
        Some("Please review <this> & that.")
    );
    assert_eq!(
        string_field(&note_dictionary, "T").as_deref(),
        Some("Alice")
    );
    assert_eq!(name_field(&note_dictionary, "State"), Some("Accepted"));
    assert_eq!(name_field(&note_dictionary, "StateModel"), Some("Review"));
    assert_eq!(
        note_dictionary.get("F").and_then(Object::as_i64),
        Some(4),
        "annotations must carry the print flag"
    );
    let note_rect = numbers(note_dictionary.get("Rect"));
    assert_eq!(note_rect, vec![72.0, 700.0, 96.0, 724.0]);
    let color = numbers(note_dictionary.get("C"));
    assert_eq!(color.len(), 3);
    assert_eq!(number_field(&note_dictionary, "CA"), Some(1.0));

    let (note_appearance, note_content) = appearance(&reopened, &note_dictionary);
    assert_eq!(name_field(&note_appearance, "Type"), Some("XObject"));
    assert_eq!(name_field(&note_appearance, "Subtype"), Some("Form"));
    assert_eq!(
        numbers(note_appearance.get("BBox")),
        vec![0.0, 0.0, 24.0, 24.0]
    );
    assert!(matches!(
        note_appearance.get("Resources"),
        Some(Object::Dictionary(_))
    ));
    let note_drawing = String::from_utf8_lossy(&note_content);
    assert!(
        note_drawing.contains("re f") && note_drawing.contains(" l S"),
        "note appearance must draw the icon: {note_drawing:?}"
    );

    let (_, highlight_dictionary) = annotation_by_id(&reopened, 0, &highlight.id);
    assert_eq!(
        name_field(&highlight_dictionary, "Subtype"),
        Some("Highlight")
    );
    assert_eq!(
        string_field(&highlight_dictionary, "Contents").as_deref(),
        Some("Key result")
    );
    assert_eq!(
        string_field(&highlight_dictionary, "T").as_deref(),
        Some("Bob")
    );
    assert_eq!(
        numbers(highlight_dictionary.get("Rect")),
        vec![72.0, 600.0, 300.0, 630.0]
    );
    assert_eq!(
        numbers(highlight_dictionary.get("QuadPoints")),
        vec![
            72.0, 616.0, 300.0, 616.0, 72.0, 600.0, 300.0, 600.0, 72.0, 630.0, 200.0, 630.0, 72.0,
            618.0, 200.0, 618.0,
        ]
    );
    assert_eq!(number_field(&highlight_dictionary, "CA"), Some(0.5));
    let (highlight_appearance, highlight_content) = appearance(&reopened, &highlight_dictionary);
    assert_eq!(name_field(&highlight_appearance, "Subtype"), Some("Form"));
    assert_eq!(
        numbers(highlight_appearance.get("BBox")),
        vec![0.0, 0.0, 228.0, 30.0]
    );
    let translucent = highlight_appearance
        .get("Resources")
        .and_then(Object::as_dictionary)
        .and_then(|resources| resources.get("ExtGState"))
        .and_then(Object::as_dictionary)
        .and_then(|states| states.get("GS0"))
        .and_then(Object::as_dictionary)
        .expect("highlight ExtGState resource");
    assert_eq!(translucent.get("ca").and_then(Object::as_f64), Some(0.5));
    let highlight_drawing = String::from_utf8_lossy(&highlight_content);
    assert_eq!(
        highlight_drawing.matches(" re f").count(),
        2,
        "one fill per quad: {highlight_drawing:?}"
    );

    let (_, reply_dictionary) = annotation_by_id(&reopened, 0, &reply.id);
    assert_eq!(
        reply_dictionary.get("IRT").and_then(Object::as_reference),
        Some(note_object),
        "/IRT must point at the parent annotation object"
    );
    assert_eq!(name_field(&reply_dictionary, "RT"), Some("R"));
    assert!(
        reply_dictionary
            .get("AP")
            .and_then(Object::as_dictionary)
            .is_some()
    );

    let output = output_path();
    std::fs::create_dir_all(output.parent().expect("output directory")).expect("create output dir");
    std::fs::write(&output, &saved).expect("write annotated PDF");
    assert!(output.is_file());
}

#[test]
fn xfdf_export_import_round_trips_on_a_real_document() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_note(
        &mut document,
        0,
        Point::new(40.0, 500.0),
        "Use <b> & \"quotes\"",
        "A & B",
        "n-1",
    )
    .expect("add text note");
    add_highlight(
        &mut document,
        0,
        &[rect(40.0, 400.0, 240.0, 416.0)],
        "Highlight & <markup>",
        "Reviewer",
    )
    .expect("add highlight");

    let exported = export_xfdf(&document).expect("export XFDF");
    assert!(exported.contains("<annot "));
    assert!(exported.contains("subtype=\"Text\""));
    assert!(exported.contains("subtype=\"Highlight\""));
    assert!(exported.contains("&amp;"));
    assert!(!exported.contains(" & "));

    let mut target = Document::from_bytes(fixture()).expect("fresh fixture parses");
    let imported = import_xfdf(&mut target, &exported, "fallback").expect("import XFDF");
    assert_eq!(imported, 2);

    let views = list_annotations(&target, 0).expect("list imported annotations");
    assert_eq!(views.len(), 2);
    let note = views
        .iter()
        .find(|view| view.subtype == AnnotationSubtype::Text)
        .expect("imported note");
    assert_eq!(note.id, "n-1");
    assert_eq!(note.contents, "Use <b> & \"quotes\"");
    assert_eq!(note.author, "A & B");

    let reexported = export_xfdf(&target).expect("re-export XFDF");
    assert_eq!(reexported, exported, "XFDF fields must round-trip exactly");
}

#[test]
fn update_and_delete_remove_content_from_a_saved_document() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_note(
        &mut document,
        0,
        Point::new(100.0, 300.0),
        "Original text",
        "Alice",
        "keep-1",
    )
    .expect("add first note");
    let removed = add_text_note(
        &mut document,
        0,
        Point::new(100.0, 200.0),
        "Delete me",
        "Alice",
        "remove-2",
    )
    .expect("add second note");

    update_contents(&mut document, 0, "keep-1", "Revised text").expect("update contents");
    assert!(matches!(
        add_text_note(
            &mut document,
            0,
            Point::new(10.0, 10.0),
            "duplicate",
            "Alice",
            "keep-1"
        ),
        Err(AnnotationEditError::DuplicateId { .. })
    ));
    delete_annotation(&mut document, 0, "remove-2").expect("delete annotation");

    assert_eq!(
        list_annotations(&document, 0)
            .expect("list after delete")
            .len(),
        1
    );

    let saved = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(saved).expect("saved document parses");
    assert_eq!(annotation_ids(&reopened, 0).len(), 1);
    let (_, kept) = annotation_by_id(&reopened, 0, "keep-1");
    assert_eq!(
        string_field(&kept, "Contents").as_deref(),
        Some("Revised text")
    );
    assert_eq!(
        reopened.object(removed.object).expect("removed object"),
        Object::Null,
        "a deleted annotation object must be overwritten"
    );
}

#[test]
fn annotations_survive_the_incremental_save_the_session_uses() {
    let bytes = fixture();
    let mut document = Document::from_bytes(bytes.clone()).expect("fixture parses");
    add_text_note(
        &mut document,
        0,
        Point::new(20.0, 20.0),
        "Incremental note",
        "Alice",
        "inc-1",
    )
    .expect("add note");
    add_highlight(
        &mut document,
        0,
        &[rect(20.0, 40.0, 200.0, 56.0)],
        "Incremental highlight",
        "Bob",
    )
    .expect("add highlight");

    let appended = document.save_incremental().expect("incremental save");
    assert!(
        appended.starts_with(&bytes),
        "an incremental save must preserve the source prefix"
    );
    let reopened = Document::from_bytes(appended).expect("appended revision parses");
    let views = list_annotations(&reopened, 0).expect("list annotations");
    assert_eq!(views.len(), 2);
}

#[test]
fn empty_identifier_is_rejected() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    assert!(matches!(
        add_text_note(
            &mut document,
            0,
            Point::new(10.0, 10.0),
            "note",
            "Alice",
            ""
        ),
        Err(AnnotationEditError::EmptyId)
    ));
}
