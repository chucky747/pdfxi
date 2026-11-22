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
    AnnotationEditError, AnnotationKind, AnnotationState, AnnotationSubtype, MAX_INK_PATHS,
    MAX_INK_POINTS, MAX_MARKUP_QUADS, MAX_POLYGON_VERTICES, MAX_STAMP_TEXT_CHARS, Point, Rect,
    ShapeStyle, add_callout, add_circle, add_cloud, add_file_attachment, add_free_text,
    add_highlight, add_ink, add_line, add_polygon, add_polygon_styled, add_square,
    add_square_styled, add_stamp, add_strikeout, add_text_note, add_underline, delete_annotation,
    export_xfdf, import_xfdf, list_annotations, reply_to, set_state, update_contents,
};
use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{CancellationToken, PageIndex, RenderedPage, Renderer};

/// Page 1 of the fixture, in default user space points.
const PAGE_WIDTH: f32 = 612.0;
/// Page 1 of the fixture, in default user space points.
const PAGE_HEIGHT: f32 = 792.0;

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

/// Where the T-0247 artifact (underline, strikeout and ink) is written.
fn markup_output_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("target/lanes/author7/t0247-out/markup.pdf")
}

/// Where the T-0247 Round B artifact (shapes and stamp) is written.
fn shapes_output_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("target/lanes/author9/t0247-out/shapes.pdf")
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

/// The underline, strikeout and ink geometry used by the round-A tests.
fn markup_quads() -> [Rect; 3] {
    [
        rect(72.0, 600.0, 300.0, 616.0),
        rect(72.0, 620.0, 240.0, 632.0),
        rect(72.0, 560.0, 300.0, 576.0),
    ]
}

/// Two deterministic ink strokes inside page 1.
fn ink_paths() -> Vec<Vec<Point>> {
    vec![
        vec![
            Point::new(80.0, 500.0),
            Point::new(120.0, 520.0),
            Point::new(160.0, 500.0),
            Point::new(200.0, 520.0),
        ],
        vec![Point::new(220.0, 500.0), Point::new(260.0, 540.0)],
    ]
}

#[test]
fn underline_strikeout_and_ink_survive_a_full_rewrite() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let original_text = document.page_text(0).expect("page 1 text before editing");

    add_text_note(
        &mut document,
        0,
        Point::new(40.0, 700.0),
        "Existing note",
        "Reader",
        "existing-note",
    )
    .expect("add an annotation that must be preserved");

    let quads = markup_quads();
    let underline = add_underline(&mut document, 0, &quads[..2], "Underlined phrase", "Alice")
        .expect("add underline");
    let strikeout = add_strikeout(&mut document, 0, &quads[2..], "Rejected phrase", "Bob")
        .expect("add strikeout");
    let paths = ink_paths();
    let ink = add_ink(&mut document, 0, &paths, 2.0, "Freehand note", "Carol").expect("add ink");

    assert_eq!(underline.subtype.as_pdf_name(), "Underline");
    assert_eq!(strikeout.subtype.as_pdf_name(), "StrikeOut");
    assert_eq!(ink.subtype.as_pdf_name(), "Ink");
    assert_ne!(underline.object, strikeout.object);

    let views = list_annotations(&document, 0).expect("list annotations");
    assert_eq!(views.len(), 4, "existing note plus three new annotations");
    assert_eq!(
        views
            .iter()
            .find(|view| view.id == "existing-note")
            .expect("existing note view")
            .contents,
        "Existing note"
    );

    let underline_view = views
        .iter()
        .find(|view| view.id == underline.id)
        .expect("underline view");
    assert_eq!(underline_view.kind, Some(AnnotationKind::Underline));
    assert_eq!(underline_view.author, "Alice");
    assert_eq!(underline_view.quads, quads[..2].to_vec());
    assert_eq!(underline_view.rect, rect(72.0, 600.0, 300.0, 632.0));
    assert_eq!(underline_view.color, Some(vec![0.0, 0.0, 0.0]));
    assert_eq!(underline_view.stroke_width, Some(1.0));
    assert_eq!(underline_view.contents, "Underlined phrase");

    let strikeout_view = views
        .iter()
        .find(|view| view.id == strikeout.id)
        .expect("strikeout view");
    assert_eq!(strikeout_view.kind, Some(AnnotationKind::Strikeout));
    assert_eq!(strikeout_view.quads, quads[2..].to_vec());
    assert_eq!(strikeout_view.stroke_width, Some(1.0));

    let ink_view = views
        .iter()
        .find(|view| view.id == ink.id)
        .expect("ink view");
    assert_eq!(ink_view.kind, Some(AnnotationKind::Pencil));
    assert_eq!(ink_view.author, "Carol");
    assert_eq!(ink_view.ink_paths, paths);
    assert!(ink_view.quads.is_empty());
    assert_eq!(ink_view.stroke_width, Some(2.0));
    assert_eq!(
        ink_view.rect,
        rect(78.5, 498.5, 261.5, 541.5),
        "the ink rect expands the point bounds by half the stroke plus slack"
    );

    let saved = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(saved.clone()).expect("saved document parses");
    assert_eq!(
        reopened.page_text(0).expect("page 1 text after editing"),
        original_text,
        "annotations must not change page text"
    );
    assert_eq!(annotation_ids(&reopened, 0).len(), 4);

    let (_, note_dictionary) = annotation_by_id(&reopened, 0, "existing-note");
    assert_eq!(
        string_field(&note_dictionary, "Contents").as_deref(),
        Some("Existing note"),
        "the pre-existing annotation must be preserved"
    );

    let (_, underline_dictionary) = annotation_by_id(&reopened, 0, &underline.id);
    assert_eq!(name_field(&underline_dictionary, "Type"), Some("Annot"));
    assert_eq!(
        name_field(&underline_dictionary, "Subtype"),
        Some("Underline")
    );
    assert_eq!(
        string_field(&underline_dictionary, "T").as_deref(),
        Some("Alice")
    );
    assert!(
        string_field(&underline_dictionary, "M")
            .as_deref()
            .is_some_and(|date| date.starts_with("D:")),
        "markup must carry a PDF date"
    );
    assert_eq!(
        numbers(underline_dictionary.get("Rect")),
        vec![72.0, 600.0, 300.0, 632.0]
    );
    assert_eq!(
        numbers(underline_dictionary.get("QuadPoints")),
        vec![
            72.0, 616.0, 300.0, 616.0, 72.0, 600.0, 300.0, 600.0, 72.0, 632.0, 240.0, 632.0, 72.0,
            620.0, 240.0, 620.0,
        ]
    );
    assert_eq!(numbers(underline_dictionary.get("C")), vec![0.0, 0.0, 0.0]);
    assert_eq!(
        underline_dictionary
            .get("BS")
            .and_then(Object::as_dictionary)
            .and_then(|border| border.get("W"))
            .and_then(Object::as_f64),
        Some(1.0)
    );
    let (underline_appearance, underline_content) = appearance(&reopened, &underline_dictionary);
    assert_eq!(
        numbers(underline_appearance.get("BBox")),
        vec![0.0, 0.0, 228.0, 32.0]
    );
    let underline_drawing = String::from_utf8_lossy(&underline_content);
    assert_eq!(
        underline_drawing.matches(" l S").count(),
        2,
        "one stroked underline per quad: {underline_drawing:?}"
    );
    assert!(underline_drawing.contains("0.000 0.000 0.000 RG"));

    let (_, strikeout_dictionary) = annotation_by_id(&reopened, 0, &strikeout.id);
    assert_eq!(
        name_field(&strikeout_dictionary, "Subtype"),
        Some("StrikeOut")
    );
    assert_eq!(
        numbers(strikeout_dictionary.get("QuadPoints")),
        vec![72.0, 576.0, 300.0, 576.0, 72.0, 560.0, 300.0, 560.0]
    );
    let (_, strikeout_content) = appearance(&reopened, &strikeout_dictionary);
    assert_eq!(
        String::from_utf8_lossy(&strikeout_content)
            .matches(" l S")
            .count(),
        1
    );

    let (_, ink_dictionary) = annotation_by_id(&reopened, 0, &ink.id);
    assert_eq!(name_field(&ink_dictionary, "Subtype"), Some("Ink"));
    assert_eq!(string_field(&ink_dictionary, "T").as_deref(), Some("Carol"));
    assert_eq!(
        numbers(ink_dictionary.get("Rect")),
        vec![78.5, 498.5, 261.5, 541.5]
    );
    let strokes: Vec<Vec<f64>> = ink_dictionary
        .get("InkList")
        .and_then(Object::as_array)
        .expect("ink list")
        .iter()
        .map(|stroke| numbers(Some(stroke)))
        .collect();
    assert_eq!(strokes.len(), 2);
    assert_eq!(strokes[0].len(), 8, "four points with x and y each");
    assert_eq!(strokes[1], vec![220.0, 500.0, 260.0, 540.0]);
    assert_eq!(
        ink_dictionary
            .get("BS")
            .and_then(Object::as_dictionary)
            .and_then(|border| border.get("W"))
            .and_then(Object::as_f64),
        Some(2.0)
    );
    assert_eq!(numbers(ink_dictionary.get("C")), vec![0.0, 0.0, 0.0]);
    let (ink_appearance, ink_content) = appearance(&reopened, &ink_dictionary);
    assert_eq!(
        numbers(ink_appearance.get("BBox")),
        vec![0.0, 0.0, 183.0, 43.0]
    );
    let ink_drawing = String::from_utf8_lossy(&ink_content);
    assert_eq!(
        ink_drawing.matches(" m\n").count(),
        2,
        "one move for each stroke: {ink_drawing:?}"
    );
    assert_eq!(ink_drawing.matches(" l\n").count(), 4);
    assert!(ink_drawing.contains("2.000 w"));
    assert!(ink_drawing.contains("1 J 1 j"));

    let output = markup_output_path();
    std::fs::create_dir_all(output.parent().expect("output directory")).expect("create output dir");
    std::fs::write(&output, &saved).expect("write markup PDF");
    assert!(output.is_file());

    // The session path appends; an incremental revision must read back too.
    let bytes = fixture();
    let mut session_document = Document::from_bytes(bytes.clone()).expect("fixture parses");
    add_ink(
        &mut session_document,
        0,
        &ink_paths(),
        2.0,
        "Incremental ink",
        "Carol",
    )
    .expect("add ink to the session document");
    let appended = session_document
        .save_incremental()
        .expect("incremental save");
    assert!(
        appended.starts_with(&bytes),
        "an incremental save must preserve the source prefix"
    );
    let session_reopened = Document::from_bytes(appended).expect("appended revision parses");
    let session_views = list_annotations(&session_reopened, 0).expect("list incremental views");
    assert_eq!(session_views.len(), 1);
    assert_eq!(session_views[0].kind, Some(AnnotationKind::Pencil));
    assert_eq!(session_views[0].ink_paths, ink_paths());
}

#[test]
fn invalid_markup_input_is_rejected_without_mutation() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_note(
        &mut document,
        0,
        Point::new(40.0, 700.0),
        "Existing note",
        "Reader",
        "existing-note",
    )
    .expect("add an annotation that must be preserved");
    let annotations_before = annotation_ids(&document, 0);
    let next_object_before = document.allocate_object_number().expect("object number");

    assert!(matches!(
        add_underline(&mut document, 0, &[], "x", "Alice"),
        Err(AnnotationEditError::EmptyQuads)
    ));
    assert!(matches!(
        add_underline(
            &mut document,
            0,
            &[Rect::saturating(10.0, 10.0, 10.0, 20.0)],
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::EmptyQuads)
    ));
    assert!(matches!(
        add_strikeout(
            &mut document,
            0,
            &[rect(700.0, 700.0, 760.0, 760.0)],
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));
    let too_many_quads = vec![rect(10.0, 10.0, 20.0, 20.0); MAX_MARKUP_QUADS + 1];
    assert!(matches!(
        add_underline(&mut document, 0, &too_many_quads, "x", "Alice"),
        Err(AnnotationEditError::TooManyQuads {
            count,
            limit: MAX_MARKUP_QUADS
        }) if count == MAX_MARKUP_QUADS + 1
    ));

    assert!(matches!(
        add_ink(&mut document, 0, &[], 1.0, "x", "Alice"),
        Err(AnnotationEditError::EmptyInk)
    ));
    assert!(matches!(
        add_ink(&mut document, 0, &[Vec::new()], 1.0, "x", "Alice"),
        Err(AnnotationEditError::EmptyInk)
    ));
    assert!(matches!(
        add_ink(
            &mut document,
            0,
            &[vec![Point::new(f32::NAN, 10.0)]],
            1.0,
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::Geometry(_))
    ));
    assert!(matches!(
        add_ink(
            &mut document,
            0,
            &[vec![Point::new(5000.0, 10.0)]],
            1.0,
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));
    for width in [0.0, f32::NAN, 100.0] {
        assert!(matches!(
            add_ink(
                &mut document,
                0,
                &[vec![Point::new(10.0, 10.0), Point::new(20.0, 20.0)]],
                width,
                "x",
                "Alice"
            ),
            Err(AnnotationEditError::InvalidStrokeWidth { .. })
        ));
    }
    let too_many_paths = vec![Vec::new(); MAX_INK_PATHS + 1];
    assert!(matches!(
        add_ink(&mut document, 0, &too_many_paths, 1.0, "x", "Alice"),
        Err(AnnotationEditError::TooManyInkPaths {
            count,
            limit: MAX_INK_PATHS
        }) if count == MAX_INK_PATHS + 1
    ));
    let too_many_points = vec![vec![Point::new(10.0, 10.0); MAX_INK_POINTS + 1]];
    assert!(matches!(
        add_ink(&mut document, 0, &too_many_points, 1.0, "x", "Alice"),
        Err(AnnotationEditError::TooManyInkPoints {
            count,
            limit: MAX_INK_POINTS
        }) if count == MAX_INK_POINTS + 1
    ));

    assert_eq!(
        annotation_ids(&document, 0),
        annotations_before,
        "a rejected annotation must not change the /Annots array"
    );
    assert_eq!(
        document.allocate_object_number().expect("object number"),
        next_object_before,
        "a rejected annotation must not consume an object number"
    );
    let views = list_annotations(&document, 0).expect("list annotations");
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].id, "existing-note");
    assert_eq!(views[0].contents, "Existing note");
}

#[test]
fn new_markup_types_render_inside_their_rect() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let page = document.page(1).expect("page 1");
    assert_eq!(
        (page.media_box.width(), page.media_box.height()),
        (f64::from(PAGE_WIDTH), f64::from(PAGE_HEIGHT)),
        "the render helpers assume page 1 is US Letter"
    );

    let underline_quad = rect(72.0, 660.0, 300.0, 676.0);
    let strikeout_quad = rect(72.0, 620.0, 300.0, 636.0);
    let ink_stroke = vec![
        Point::new(72.0, 540.0),
        Point::new(110.0, 600.0),
        Point::new(150.0, 545.0),
        Point::new(200.0, 600.0),
        Point::new(250.0, 545.0),
        Point::new(300.0, 590.0),
    ];

    let underline = add_underline(
        &mut document,
        1,
        &[underline_quad],
        "Rendered underline",
        "Alice",
    )
    .expect("add underline");
    let strikeout = add_strikeout(
        &mut document,
        1,
        &[strikeout_quad],
        "Rendered strikeout",
        "Bob",
    )
    .expect("add strikeout");
    let ink = add_ink(
        &mut document,
        1,
        &[ink_stroke],
        2.0,
        "Rendered ink",
        "Carol",
    )
    .expect("add ink");

    let views = list_annotations(&document, 1).expect("list annotations");
    let rects = vec![
        views
            .iter()
            .find(|view| view.id == underline.id)
            .expect("underline view")
            .rect,
        views
            .iter()
            .find(|view| view.id == strikeout.id)
            .expect("strikeout view")
            .rect,
        views
            .iter()
            .find(|view| view.id == ink.id)
            .expect("ink view")
            .rect,
    ];

    let saved = document.save().expect("save edited page");
    let before = render_page(&renderer_for(&fixture()), 1);
    let after = render_page(&renderer_for(&saved), 1);
    assert_eq!(
        (before.width(), before.height()),
        (after.width(), after.height()),
        "the render must keep the page size"
    );

    for (name, rect, minimum) in [
        ("underline", rects[0], 100usize),
        ("strikeout", rects[1], 100),
        ("ink", rects[2], 40),
    ] {
        let changed = changed_pixels_inside(&before, &after, rect, 2);
        assert!(
            changed > minimum,
            "{name} must paint inside its annotation rect, changed {changed} pixels"
        );
        println!("{name} rect {rect:?} changed {changed} pixels");
    }
    let outside = changed_pixels_outside(&before, &after, &rects, 2);
    assert_eq!(
        outside, 0,
        "the new markup must not paint outside its annotation rects"
    );
}

#[test]
fn line_square_circle_polygon_and_stamp_survive_a_full_rewrite() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let original_text = document.page_text(0).expect("page 1 text before editing");

    add_text_note(
        &mut document,
        0,
        Point::new(40.0, 700.0),
        "Existing note",
        "Reader",
        "existing-note",
    )
    .expect("add an annotation that must be preserved");

    let line_from = Point::new(90.0, 520.0);
    let line_to = Point::new(240.0, 560.0);
    let line = add_line(&mut document, 0, line_from, line_to, "Leader", "Alice").expect("add line");
    let square_rect = rect(90.0, 430.0, 210.0, 480.0);
    let square_style = ShapeStyle {
        stroke_color: [0.1, 0.2, 0.6],
        fill_color: Some([1.0, 0.9, 0.2]),
        stroke_width: 2.0,
        opacity: 0.8,
    };
    let square = add_square_styled(
        &mut document,
        0,
        square_rect,
        square_style,
        "Key box",
        "Bob",
    )
    .expect("add square");
    let circle_rect = rect(240.0, 430.0, 340.0, 480.0);
    let circle = add_circle(&mut document, 0, circle_rect, "Ellipse", "Carol").expect("add circle");
    let vertices = [
        Point::new(90.0, 350.0),
        Point::new(160.0, 410.0),
        Point::new(230.0, 350.0),
        Point::new(160.0, 330.0),
    ];
    let polygon_style = ShapeStyle {
        fill_color: Some([0.9, 0.9, 0.9]),
        ..ShapeStyle::default()
    };
    let polygon = add_polygon_styled(&mut document, 0, &vertices, polygon_style, "Region", "Dan")
        .expect("add polygon");
    let stamp_rect = rect(300.0, 330.0, 450.0, 380.0);
    let stamp =
        add_stamp(&mut document, 0, stamp_rect, "approved", "APPROVED", "Eve").expect("add stamp");

    assert_eq!(line.subtype.as_pdf_name(), "Line");
    assert_eq!(square.subtype.as_pdf_name(), "Square");
    assert_eq!(circle.subtype.as_pdf_name(), "Circle");
    assert_eq!(polygon.subtype.as_pdf_name(), "Polygon");
    assert_eq!(stamp.subtype.as_pdf_name(), "Stamp");

    let views = list_annotations(&document, 0).expect("list annotations");
    assert_eq!(views.len(), 6, "existing note plus five new annotations");

    let line_view = views
        .iter()
        .find(|view| view.id == line.id)
        .expect("line view");
    assert_eq!(line_view.kind, Some(AnnotationKind::Line));
    assert_eq!(line_view.line, Some((line_from, line_to)));
    assert_eq!(line_view.rect, rect(81.5, 511.5, 248.5, 568.5));
    assert_eq!(line_view.contents, "Leader");
    assert_eq!(line_view.author, "Alice");
    assert_eq!(line_view.stroke_width, Some(1.0));

    let square_view = views
        .iter()
        .find(|view| view.id == square.id)
        .expect("square view");
    assert_eq!(square_view.kind, Some(AnnotationKind::Rectangle));
    assert_eq!(square_view.rect, rect(89.0, 429.0, 211.0, 481.0));
    assert_eq!(square_view.color, Some(vec![0.1, 0.2, 0.6]));
    assert_eq!(square_view.stroke_width, Some(2.0));
    assert_eq!(square_view.contents, "Key box");

    let circle_view = views
        .iter()
        .find(|view| view.id == circle.id)
        .expect("circle view");
    assert_eq!(circle_view.kind, Some(AnnotationKind::Oval));
    assert_eq!(circle_view.rect, rect(239.5, 429.5, 340.5, 480.5));

    let polygon_view = views
        .iter()
        .find(|view| view.id == polygon.id)
        .expect("polygon view");
    assert_eq!(polygon_view.kind, Some(AnnotationKind::Polygon));
    assert_eq!(polygon_view.vertices, vertices.to_vec());
    assert_eq!(polygon_view.rect, rect(89.5, 329.5, 230.5, 410.5));

    let stamp_view = views
        .iter()
        .find(|view| view.id == stamp.id)
        .expect("stamp view");
    assert_eq!(stamp_view.kind, Some(AnnotationKind::Stamp));
    assert_eq!(stamp_view.stamp_name.as_deref(), Some("Approved"));
    assert_eq!(stamp_view.rect, stamp_rect);
    assert_eq!(stamp_view.contents, "APPROVED");
    assert_eq!(stamp_view.author, "Eve");

    assert_eq!(
        views
            .iter()
            .find(|view| view.id == "existing-note")
            .expect("existing note view")
            .kind,
        Some(AnnotationKind::Note)
    );

    let saved = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(saved.clone()).expect("saved document parses");
    assert_eq!(
        reopened.page_text(0).expect("page 1 text after editing"),
        original_text,
        "annotations must not change page text"
    );
    assert_eq!(annotation_ids(&reopened, 0).len(), 6);

    let (_, note_dictionary) = annotation_by_id(&reopened, 0, "existing-note");
    assert_eq!(
        string_field(&note_dictionary, "Contents").as_deref(),
        Some("Existing note"),
        "the pre-existing annotation must be preserved"
    );

    let (_, line_dictionary) = annotation_by_id(&reopened, 0, &line.id);
    assert_eq!(name_field(&line_dictionary, "Subtype"), Some("Line"));
    assert_eq!(
        numbers(line_dictionary.get("L")),
        vec![90.0, 520.0, 240.0, 560.0]
    );
    let line_endings: Vec<&str> = line_dictionary
        .get("LE")
        .and_then(Object::as_array)
        .expect("line endings")
        .iter()
        .map(|item| item.as_name().expect("line ending name"))
        .collect();
    assert_eq!(line_endings, vec!["None", "OpenArrow"]);
    assert_eq!(
        numbers(line_dictionary.get("Rect")),
        vec![81.5, 511.5, 248.5, 568.5]
    );
    assert_eq!(numbers(line_dictionary.get("C")), vec![0.0, 0.0, 0.0]);
    assert_eq!(
        line_dictionary
            .get("BS")
            .and_then(Object::as_dictionary)
            .and_then(|border| border.get("W"))
            .and_then(Object::as_f64),
        Some(1.0)
    );
    let (line_appearance, line_content) = appearance(&reopened, &line_dictionary);
    assert_eq!(name_field(&line_appearance, "Type"), Some("XObject"));
    assert_eq!(name_field(&line_appearance, "Subtype"), Some("Form"));
    assert_eq!(
        numbers(line_appearance.get("BBox")),
        vec![0.0, 0.0, 167.0, 57.0]
    );
    let line_drawing = String::from_utf8_lossy(&line_content);
    assert_eq!(
        line_drawing.matches(" l S").count(),
        3,
        "the line plus two open-arrow legs: {line_drawing:?}"
    );
    assert!(line_drawing.contains("1 J 1 j"));

    let (_, square_dictionary) = annotation_by_id(&reopened, 0, &square.id);
    assert_eq!(name_field(&square_dictionary, "Subtype"), Some("Square"));
    assert_eq!(
        numbers(square_dictionary.get("Rect")),
        vec![89.0, 429.0, 211.0, 481.0]
    );
    assert_eq!(numbers(square_dictionary.get("IC")), vec![1.0, 0.9, 0.2]);
    assert_eq!(numbers(square_dictionary.get("C")), vec![0.1, 0.2, 0.6]);
    assert_eq!(number_field(&square_dictionary, "CA"), Some(0.8));
    let (square_appearance, square_content) = appearance(&reopened, &square_dictionary);
    assert_eq!(
        numbers(square_appearance.get("BBox")),
        vec![0.0, 0.0, 122.0, 52.0]
    );
    let square_drawing = String::from_utf8_lossy(&square_content);
    assert!(
        square_drawing.contains(" re B"),
        "a filled square strokes and fills: {square_drawing:?}"
    );
    assert!(square_drawing.contains("1.000 0.900 0.200 rg"));
    assert!(square_drawing.contains("0.100 0.200 0.600 RG"));
    assert!(square_drawing.contains("2.000 w"));

    let (_, circle_dictionary) = annotation_by_id(&reopened, 0, &circle.id);
    assert_eq!(name_field(&circle_dictionary, "Subtype"), Some("Circle"));
    assert_eq!(
        numbers(circle_dictionary.get("Rect")),
        vec![239.5, 429.5, 340.5, 480.5]
    );
    assert!(
        !circle_dictionary.contains_key("IC"),
        "an unfilled ellipse must not write /IC"
    );
    let (circle_appearance, circle_content) = appearance(&reopened, &circle_dictionary);
    assert_eq!(
        numbers(circle_appearance.get("BBox")),
        vec![0.0, 0.0, 101.0, 51.0]
    );
    let circle_drawing = String::from_utf8_lossy(&circle_content);
    assert_eq!(
        circle_drawing.matches(" c\n").count(),
        4,
        "four Bezier segments make the ellipse: {circle_drawing:?}"
    );

    let (_, polygon_dictionary) = annotation_by_id(&reopened, 0, &polygon.id);
    assert_eq!(name_field(&polygon_dictionary, "Subtype"), Some("Polygon"));
    assert_eq!(
        numbers(polygon_dictionary.get("Vertices")),
        vec![90.0, 350.0, 160.0, 410.0, 230.0, 350.0, 160.0, 330.0]
    );
    assert_eq!(numbers(polygon_dictionary.get("IC")), vec![0.9, 0.9, 0.9]);
    let (_, polygon_content) = appearance(&reopened, &polygon_dictionary);
    let polygon_drawing = String::from_utf8_lossy(&polygon_content);
    assert_eq!(polygon_drawing.matches(" l\n").count(), 3);
    assert!(polygon_drawing.contains("h B"));

    let (_, stamp_dictionary) = annotation_by_id(&reopened, 0, &stamp.id);
    assert_eq!(name_field(&stamp_dictionary, "Type"), Some("Annot"));
    assert_eq!(name_field(&stamp_dictionary, "Subtype"), Some("Stamp"));
    assert_eq!(name_field(&stamp_dictionary, "Name"), Some("Approved"));
    assert_eq!(
        numbers(stamp_dictionary.get("Rect")),
        vec![300.0, 330.0, 450.0, 380.0]
    );
    assert_eq!(
        string_field(&stamp_dictionary, "Contents").as_deref(),
        Some("APPROVED")
    );
    assert_eq!(string_field(&stamp_dictionary, "T").as_deref(), Some("Eve"));
    let (stamp_appearance, stamp_content) = appearance(&reopened, &stamp_dictionary);
    assert_eq!(
        numbers(stamp_appearance.get("BBox")),
        vec![0.0, 0.0, 150.0, 50.0]
    );
    let font = stamp_appearance
        .get("Resources")
        .and_then(Object::as_dictionary)
        .and_then(|resources| resources.get("Font"))
        .and_then(Object::as_dictionary)
        .and_then(|fonts| fonts.get("F1"))
        .and_then(Object::as_dictionary)
        .expect("stamp font resource");
    assert_eq!(name_field(font, "BaseFont"), Some("Helvetica"));
    let stamp_drawing = String::from_utf8_lossy(&stamp_content);
    assert!(stamp_drawing.contains(" re S"), "bordered label");
    assert!(stamp_drawing.contains("BT"));
    assert!(stamp_drawing.contains("/F1"));
    assert!(stamp_drawing.contains("(APPROVED) Tj"));

    let output = shapes_output_path();
    std::fs::create_dir_all(output.parent().expect("output directory")).expect("create output dir");
    std::fs::write(&output, &saved).expect("write shapes PDF");
    assert!(output.is_file());
}

#[test]
fn invalid_shape_and_stamp_input_is_rejected_without_mutation() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let annotations_before = annotation_ids(&document, 0);
    let next_object_before = document.allocate_object_number().expect("object number");

    assert!(matches!(
        add_square(
            &mut document,
            0,
            Rect::saturating(10.0, 10.0, 10.0, 20.0),
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::EmptyShapeRect)
    ));
    assert!(matches!(
        add_circle(
            &mut document,
            0,
            rect(700.0, 700.0, 760.0, 760.0),
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));
    assert!(matches!(
        add_line(
            &mut document,
            0,
            Point::new(10.0, 10.0),
            Point::new(10.0, 10.0),
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::DegenerateLine)
    ));
    assert!(matches!(
        add_line(
            &mut document,
            0,
            Point::new(f32::NAN, 10.0),
            Point::new(20.0, 20.0),
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::Geometry(_))
    ));
    assert!(matches!(
        add_line(
            &mut document,
            0,
            Point::new(-10.0, 10.0),
            Point::new(20.0, 20.0),
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));

    assert!(matches!(
        add_polygon(
            &mut document,
            0,
            &[Point::new(10.0, 10.0), Point::new(20.0, 20.0)],
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::DegeneratePolygon)
    ));
    assert!(matches!(
        add_polygon(
            &mut document,
            0,
            &[
                Point::new(100.0, 100.0),
                Point::new(120.0, 120.0),
                Point::new(140.0, 140.0)
            ],
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::DegeneratePolygon)
    ));
    let too_many_vertices = vec![Point::new(10.0, 10.0); MAX_POLYGON_VERTICES + 1];
    assert!(matches!(
        add_polygon(
            &mut document,
            0,
            &too_many_vertices,
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::TooManyVertices {
            count,
            limit: MAX_POLYGON_VERTICES
        }) if count == MAX_POLYGON_VERTICES + 1
    ));
    assert!(matches!(
        add_polygon(
            &mut document,
            0,
            &[
                Point::new(5000.0, 10.0),
                Point::new(20.0, 20.0),
                Point::new(30.0, 10.0)
            ],
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));

    let shape_rect = rect(100.0, 100.0, 200.0, 160.0);
    for style in [
        ShapeStyle {
            stroke_color: [0.0, f64::NAN, 0.0],
            ..ShapeStyle::default()
        },
        ShapeStyle {
            fill_color: Some([0.0, 0.0, 2.0]),
            ..ShapeStyle::default()
        },
    ] {
        assert!(matches!(
            add_square_styled(&mut document, 0, shape_rect, style, "x", "Alice"),
            Err(AnnotationEditError::InvalidColorComponent { .. })
        ));
    }
    assert!(matches!(
        add_square_styled(
            &mut document,
            0,
            shape_rect,
            ShapeStyle {
                opacity: 1.5,
                ..ShapeStyle::default()
            },
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::InvalidOpacity { .. })
    ));
    for width in [0.0, f32::NAN, 100.0] {
        assert!(matches!(
            add_square_styled(
                &mut document,
                0,
                shape_rect,
                ShapeStyle {
                    stroke_width: width,
                    ..ShapeStyle::default()
                },
                "x",
                "Alice"
            ),
            Err(AnnotationEditError::InvalidStrokeWidth { .. })
        ));
    }

    let stamp_rect = rect(300.0, 300.0, 450.0, 350.0);
    assert!(matches!(
        add_stamp(&mut document, 0, stamp_rect, "NotAStamp", "x", "Alice"),
        Err(AnnotationEditError::InvalidStampName(name)) if name == "NotAStamp"
    ));
    let too_long = "x".repeat(MAX_STAMP_TEXT_CHARS + 1);
    assert!(matches!(
        add_stamp(&mut document, 0, stamp_rect, "Approved", &too_long, "Alice"),
        Err(AnnotationEditError::StampTextTooLong {
            chars,
            limit: MAX_STAMP_TEXT_CHARS
        }) if chars == MAX_STAMP_TEXT_CHARS + 1
    ));
    assert!(matches!(
        add_stamp(
            &mut document,
            0,
            rect(700.0, 700.0, 800.0, 800.0),
            "Approved",
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::CoordinateOutsidePage { page: 0, .. })
    ));
    assert!(matches!(
        add_stamp(
            &mut document,
            0,
            Rect::saturating(10.0, 10.0, 10.0, 20.0),
            "Approved",
            "x",
            "Alice"
        ),
        Err(AnnotationEditError::EmptyShapeRect)
    ));

    assert_eq!(
        annotation_ids(&document, 0),
        annotations_before,
        "a rejected shape or stamp must not change the /Annots array"
    );
    assert_eq!(
        document.allocate_object_number().expect("object number"),
        next_object_before,
        "a rejected shape or stamp must not consume an object number"
    );
    assert!(
        list_annotations(&document, 0)
            .expect("list annotations")
            .is_empty()
    );
}

#[test]
fn shapes_and_stamp_render_inside_their_rects() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let page = document.page(1).expect("page 1");
    assert_eq!(
        (page.media_box.width(), page.media_box.height()),
        (f64::from(PAGE_WIDTH), f64::from(PAGE_HEIGHT)),
        "the render helpers assume page 1 is US Letter"
    );

    let line = add_line(
        &mut document,
        1,
        Point::new(72.0, 700.0),
        Point::new(220.0, 740.0),
        "Rendered line",
        "Alice",
    )
    .expect("add line");
    let square = add_square_styled(
        &mut document,
        1,
        rect(72.0, 600.0, 200.0, 660.0),
        ShapeStyle {
            fill_color: Some([0.95, 0.85, 0.2]),
            stroke_width: 2.0,
            ..ShapeStyle::default()
        },
        "Rendered square",
        "Bob",
    )
    .expect("add square");
    let circle = add_circle(
        &mut document,
        1,
        rect(240.0, 600.0, 360.0, 660.0),
        "Rendered circle",
        "Carol",
    )
    .expect("add circle");
    let polygon = add_polygon(
        &mut document,
        1,
        &[
            Point::new(72.0, 500.0),
            Point::new(150.0, 560.0),
            Point::new(230.0, 500.0),
        ],
        "Rendered polygon",
        "Dan",
    )
    .expect("add polygon");
    let stamp = add_stamp(
        &mut document,
        1,
        rect(300.0, 500.0, 460.0, 560.0),
        "Final",
        "FINAL",
        "Eve",
    )
    .expect("add stamp");

    let views = list_annotations(&document, 1).expect("list annotations");
    let rect_of = |id: &str| {
        views
            .iter()
            .find(|view| view.id == id)
            .expect("annotation view")
            .rect
    };
    let rects = [
        rect_of(&line.id),
        rect_of(&square.id),
        rect_of(&circle.id),
        rect_of(&polygon.id),
        rect_of(&stamp.id),
    ];

    let saved = document.save().expect("save edited page");
    let before = render_page(&renderer_for(&fixture()), 1);
    let after = render_page(&renderer_for(&saved), 1);
    assert_eq!(
        (before.width(), before.height()),
        (after.width(), after.height()),
        "the render must keep the page size"
    );

    for (name, rect, minimum) in [
        ("line", rects[0], 40usize),
        ("square", rects[1], 200),
        ("circle", rects[2], 200),
        ("polygon", rects[3], 200),
        ("stamp", rects[4], 200),
    ] {
        let changed = changed_pixels_inside(&before, &after, rect, 2);
        assert!(
            changed > minimum,
            "{name} must paint inside its annotation rect, changed {changed} pixels"
        );
        println!("{name} rect {rect:?} changed {changed} pixels");
    }
    let outside = changed_pixels_outside(&before, &after, &rects, 2);
    assert_eq!(
        outside, 0,
        "shapes and stamps must not paint outside their annotation rects"
    );
}

#[test]
fn hostile_stamp_text_is_pdf_and_xml_safe() {
    let hostile = "Evil ) Tj ET /Name /Approved ( & < > \" ' \\ end";
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_stamp(
        &mut document,
        0,
        rect(300.0, 600.0, 480.0, 660.0),
        "Confidential",
        hostile,
        "Mallory",
    )
    .expect("add stamp with hostile text");

    let views = list_annotations(&document, 0).expect("list annotations");
    assert_eq!(views.len(), 1);
    assert_eq!(views[0].kind, Some(AnnotationKind::Stamp));
    assert_eq!(views[0].stamp_name.as_deref(), Some("Confidential"));
    assert_eq!(views[0].contents, hostile);

    let saved = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(saved).expect("saved document parses");
    let (_, stamp_dictionary) = annotation_by_id(&reopened, 0, &views[0].id);
    assert_eq!(
        string_field(&stamp_dictionary, "Contents").as_deref(),
        Some(hostile),
        "the exact label survives the PDF text string"
    );
    let (_, content) = appearance(&reopened, &stamp_dictionary);
    let drawing = String::from_utf8(content).expect("the appearance stream is ASCII");
    assert!(
        drawing.contains("Evil \\) Tj ET"),
        "the injected close parenthesis is escaped: {drawing:?}"
    );
    assert!(
        drawing.contains("\\( & < >"),
        "the injected open parenthesis is escaped: {drawing:?}"
    );
    assert_eq!(
        drawing.matches("Tj\nET").count(),
        1,
        "only the real text operator ends the literal: {drawing:?}"
    );

    let exported = export_xfdf(&reopened).expect("export XFDF");
    assert!(exported.contains("subtype=\"Stamp\""));
    for entity in ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"] {
        assert!(
            exported.contains(entity),
            "XFDF must escape hostile text as {entity}: {exported:?}"
        );
    }
}

#[test]
fn old_annotations_keep_their_view_but_have_no_markup_geometry() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_note(
        &mut document,
        0,
        Point::new(20.0, 20.0),
        "Plain note",
        "Alice",
        "plain-note",
    )
    .expect("add note");
    let views = list_annotations(&document, 0).expect("list annotations");
    let note = views
        .iter()
        .find(|view| view.id == "plain-note")
        .expect("note view");
    assert_eq!(note.kind, Some(AnnotationKind::Note));
    assert!(note.quads.is_empty());
    assert!(note.ink_paths.is_empty());
    assert!(note.stroke_width.is_none());
    assert_eq!(note.color.as_deref(), Some([1.0, 0.82, 0.1].as_slice()));
}

// ---------------------------------------------------------------------------
// PDFium raster helpers
// ---------------------------------------------------------------------------

fn renderer_for(bytes: &[u8]) -> PdfiumRenderer {
    PdfiumRenderer::from_bytes(bytes.to_vec()).expect("PDFium must open the document")
}

fn render_page(renderer: &PdfiumRenderer, index: usize) -> RenderedPage {
    renderer
        .render_page(
            PageIndex::new(index),
            1.0,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("PDFium must rasterize the page")
}

/// Pixel bounds of `rect` in `page`, expanded by `margin` pixels, clamped.
fn pixel_bounds(page: &RenderedPage, rect: Rect, margin: usize) -> (usize, usize, usize, usize) {
    let width = page.width() as usize;
    let height = page.height() as usize;
    let scale_x = page.width() as f32 / PAGE_WIDTH;
    let scale_y = page.height() as f32 / PAGE_HEIGHT;
    let left = ((rect.x0().max(0.0) * scale_x) as usize)
        .saturating_sub(margin)
        .min(width);
    let right = (((rect.x1().clamp(0.0, PAGE_WIDTH) * scale_x) as usize) + margin).min(width);
    let top = ((((PAGE_HEIGHT - rect.y1().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize)
        .saturating_sub(margin))
    .min(height);
    let bottom = ((((PAGE_HEIGHT - rect.y0().clamp(0.0, PAGE_HEIGHT)) * scale_y) as usize)
        + margin)
        .min(height);
    (left, right, top, bottom)
}

fn pixel_changed(before: &RenderedPage, after: &RenderedPage, index: usize) -> bool {
    let before = before.pixels().get(index..index + 4);
    let after = after.pixels().get(index..index + 4);
    match (before, after) {
        (Some(before), Some(after)) => before != after,
        _ => false,
    }
}

/// Count pixels that differ between the two renders inside `rect`.
fn changed_pixels_inside(
    before: &RenderedPage,
    after: &RenderedPage,
    rect: Rect,
    margin: usize,
) -> usize {
    let width = after.width() as usize;
    let (left, right, top, bottom) = pixel_bounds(after, rect, margin);
    let mut changed = 0usize;
    for y in top..bottom {
        for x in left..right {
            if pixel_changed(before, after, (y * width + x) * 4) {
                changed += 1;
            }
        }
    }
    changed
}

/// Count pixels that differ between the two renders outside every `rect`.
fn changed_pixels_outside(
    before: &RenderedPage,
    after: &RenderedPage,
    rects: &[Rect],
    margin: usize,
) -> usize {
    let width = after.width() as usize;
    let height = after.height() as usize;
    let boxes: Vec<(usize, usize, usize, usize)> = rects
        .iter()
        .map(|rect| pixel_bounds(after, *rect, margin))
        .collect();
    let mut changed = 0usize;
    for y in 0..height {
        for x in 0..width {
            let inside = boxes.iter().any(|(left, right, top, bottom)| {
                x >= *left && x < *right && y >= *top && y < *bottom
            });
            if !inside && pixel_changed(before, after, (y * width + x) * 4) {
                changed += 1;
            }
        }
    }
    changed
}

#[test]
fn free_text_callout_and_cloud_survive_a_full_rewrite() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let original_text = document.page_text(0).expect("page 1 text before editing");

    let note = add_free_text(
        &mut document,
        0,
        rect(72.0, 600.0, 320.0, 660.0),
        "Free text note with several words to wrap",
        11.0,
        "Alice",
    )
    .expect("add free text");
    let callout = add_callout(
        &mut document,
        0,
        rect(72.0, 500.0, 320.0, 560.0),
        Point::new(60.0, 470.0),
        "Callout text",
        12.0,
        "Bob",
    )
    .expect("add callout");
    let cloud =
        add_cloud(&mut document, 0, rect(72.0, 380.0, 320.0, 460.0), "Carol").expect("add cloud");

    let bytes = document.save().expect("save annotated document");
    let reopened = Document::from_bytes(bytes).expect("annotated output reparses");
    assert_eq!(
        reopened.page_text(0).expect("page text after editing"),
        original_text,
        "annotations must not change the page text"
    );

    let views = list_annotations(&reopened, 0).expect("list annotations");
    let free_text = views
        .iter()
        .find(|view| view.id == note.id)
        .expect("free text listed");
    assert_eq!(free_text.author, "Alice");
    assert!(free_text.contents.contains("Free text note"));

    let callout_view = views
        .iter()
        .find(|view| view.id == callout.id)
        .expect("callout listed");
    assert_eq!(callout_view.author, "Bob");

    let cloud_view = views
        .iter()
        .find(|view| view.id == cloud.id)
        .expect("cloud listed");
    assert_eq!(cloud_view.vertices.len(), 4, "cloud carries four vertices");

    // Each annotation points at a non-empty appearance stream.
    for id in [note.object, callout.object, cloud.object] {
        let annotation = reopened.object(id).expect("annotation object");
        let dictionary = annotation.as_dictionary().expect("annotation dictionary");
        assert_eq!(
            dictionary.get("Subtype").and_then(Object::as_name),
            Some(if id == cloud.object {
                "Polygon"
            } else {
                "FreeText"
            })
        );
        let appearance = dictionary
            .get("AP")
            .and_then(|ap| ap.as_dictionary())
            .and_then(|ap| ap.get("N"))
            .and_then(Object::as_reference)
            .expect("appearance reference");
        let stream = reopened.object(appearance).expect("appearance object");
        let Object::Stream { data, .. } = stream else {
            panic!("appearance must be a stream");
        };
        assert!(!data.is_empty());
    }

    let callout_dictionary = reopened
        .object(callout.object)
        .expect("callout object")
        .as_dictionary()
        .expect("callout dictionary")
        .clone();
    assert_eq!(
        callout_dictionary.get("IT").and_then(Object::as_name),
        Some("FreeTextCallout")
    );
    assert!(callout_dictionary.contains_key("CL"), "callout leader line");
}

#[test]
fn free_text_rejects_bad_sizes_text_and_geometry() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let empty = rect(72.0, 600.0, 72.0, 660.0);
    assert!(matches!(
        add_free_text(&mut document, 0, empty, "x", 11.0, "A"),
        Err(AnnotationEditError::EmptyShapeRect)
    ));
    assert!(matches!(
        add_free_text(
            &mut document,
            0,
            rect(72.0, 600.0, 320.0, 660.0),
            "x",
            100.0,
            "A"
        ),
        Err(AnnotationEditError::InvalidFontSize(_))
    ));
    assert!(matches!(
        add_free_text(
            &mut document,
            0,
            rect(72.0, 600.0, 320.0, 660.0),
            "  ",
            11.0,
            "A"
        ),
        Err(AnnotationEditError::EmptyText)
    ));
    assert!(matches!(
        add_callout(
            &mut document,
            0,
            rect(72.0, 500.0, 320.0, 560.0),
            Point::new(f32::NAN, 470.0),
            "x",
            11.0,
            "A"
        ),
        Err(AnnotationEditError::InvalidAnchor)
    ));
}

#[test]
fn file_attachment_survives_a_save_and_is_discoverable_as_an_attachment() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let payload = b"attachment payload with binary \x00 bytes";
    let added = add_file_attachment(
        &mut document,
        0,
        rect(72.0, 700.0, 96.0, 724.0),
        "evidence.bin",
        payload,
        "Alice",
    )
    .expect("add file attachment");

    let bytes = document.save().expect("save annotated document");
    let reopened = Document::from_bytes(bytes).expect("annotated output reparses");

    let views = list_annotations(&reopened, 0).expect("list annotations");
    let view = views
        .iter()
        .find(|view| view.id == added.id)
        .expect("attachment annotation listed");
    assert_eq!(view.subtype.as_pdf_name(), "FileAttachment");
    assert_eq!(view.author, "Alice");

    // The annotation is also a document attachment via core's attachment reader.
    let attachments = reopened.attachments().expect("read attachments");
    assert!(
        attachments
            .iter()
            .any(|attachment| attachment.name == "evidence.bin"),
        "{attachments:?}"
    );
    assert_eq!(
        reopened
            .attachment_bytes("evidence.bin")
            .expect("read attachment bytes"),
        payload
    );
}

#[test]
fn file_attachment_rejects_bad_names_sizes_and_rectangles() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    assert!(matches!(
        add_file_attachment(
            &mut document,
            0,
            rect(72.0, 700.0, 96.0, 724.0),
            "",
            b"x",
            "A"
        ),
        Err(AnnotationEditError::InvalidAttachmentName(_))
    ));
    assert!(matches!(
        add_file_attachment(
            &mut document,
            0,
            rect(72.0, 700.0, 96.0, 724.0),
            "a/b.bin",
            b"x",
            "A"
        ),
        Err(AnnotationEditError::InvalidAttachmentName(_))
    ));
    assert!(matches!(
        add_file_attachment(
            &mut document,
            0,
            rect(72.0, 700.0, 96.0, 724.0),
            "a\\b.bin",
            b"x",
            "A"
        ),
        Err(AnnotationEditError::InvalidAttachmentName(_))
    ));
    assert!(matches!(
        add_file_attachment(
            &mut document,
            0,
            rect(72.0, 700.0, 72.0, 724.0),
            "ok.bin",
            b"x",
            "A"
        ),
        Err(AnnotationEditError::EmptyShapeRect)
    ));
}
