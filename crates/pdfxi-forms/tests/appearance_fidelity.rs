//! End-to-end appearance fidelity: the field's own font and transformed widgets.
//!
//! Every case starts from the committed self-produced AcroForm fixture
//! (`fixtures/realworld/pdfxi-forms-selfmade.pdf`), stages an embedded font,
//! a rotated widget or both through `Document::set_object`, saves, reopens, then
//! fills through the public API. Derivatives are written to
//! `target/lanes/forms/t0248-out/` for an independent PDFium check.
//!
//! The embedded TrueType program is the repository's own OFL-licensed
//! `pdfxi-edit` test asset when it is present; otherwise a structurally valid
//! sfnt header is staged so the object graph is still exercised. Either way the
//! fixture derivative is self-produced.

use std::path::{Path, PathBuf};

use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use pdfxi_forms::document_form::{
    AppearanceFont, FallbackReason, FormRect, fallback_reason_name, flatten, read_acroform,
    set_checkbox_reported, set_text_reported,
};
use pdfxi_forms::{FieldValue, Matrix};

const FIXTURE: &str = concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../fixtures/realworld/pdfxi-forms-selfmade.pdf"
);

fn page_id() -> ObjectId {
    ObjectId::new(3, 0)
}

fn acroform_id() -> ObjectId {
    ObjectId::new(6, 0)
}

fn checkbox_widget_id() -> ObjectId {
    ObjectId::new(8, 0)
}

/// Object numbers reserved by this test, above the fixture's own `1..=9`.
const EMBEDDED_FONT_ID: u32 = 20;
const EMBEDDED_DESCRIPTOR_ID: u32 = 21;
const EMBEDDED_FILE_ID: u32 = 22;
const EMBEDDED_FIELD_ID: u32 = 25;
const ROTATED_FIELD_ID: u32 = 30;
const SUBSET_FONT_ID: u32 = 40;
const SUBSET_CID_FONT_ID: u32 = 41;
const SUBSET_DESCRIPTOR_ID: u32 = 42;
const SUBSET_FILE_ID: u32 = 43;
const SUBSET_TO_UNICODE_ID: u32 = 44;
const SUBSET_FIELD_ID: u32 = 45;

fn load() -> Document {
    let bytes = std::fs::read(FIXTURE).expect("fixture bytes");
    Document::from_bytes(bytes).expect("fixture parses")
}

fn save_and_reopen(doc: &Document) -> Document {
    let bytes = doc.save().expect("full save");
    Document::from_bytes(bytes).expect("saved bytes parse")
}

fn dictionary(doc: &Document, id: ObjectId) -> Dictionary {
    doc.object(id)
        .expect("object loads")
        .as_dictionary()
        .cloned()
        .expect("object is a dictionary")
}

fn stream(doc: &Document, id: ObjectId) -> (Dictionary, Vec<u8>) {
    match doc.object(id).expect("stream object loads") {
        Object::Stream { dictionary, data } => (dictionary, data),
        other => panic!("expected a stream, found {other:?}"),
    }
}

fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

fn numbers(object: &Object) -> Vec<f64> {
    object
        .as_array()
        .expect("array")
        .iter()
        .map(|value| value.as_f64().expect("numeric"))
        .collect()
}

fn normal_stream_id(doc: &Document, widget: ObjectId) -> ObjectId {
    let widget = dictionary(doc, widget);
    widget
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|ap| ap.get("N"))
        .and_then(Object::as_reference)
        .expect("AP/N stream reference")
}

fn output_directory() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/forms/t0248-out")
}

fn write_artifact(name: &str, doc: &Document) {
    let directory = output_directory();
    std::fs::create_dir_all(&directory).expect("create output directory");
    std::fs::write(
        directory.join(name),
        doc.save().expect("serialize artifact"),
    )
    .expect("write artifact");
}

/// Reads the repository's OFL Courier Prime program, or a synthetic sfnt header.
fn font_program() -> Vec<u8> {
    let path =
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../pdfxi-edit/assets/CourierPrime-Regular.ttf");
    std::fs::read(path).unwrap_or_else(|_| {
        let mut bytes = vec![0x00, 0x01, 0x00, 0x00];
        bytes.extend_from_slice(b"pdfxi-synthetic-sfnt-program");
        bytes
    })
}

/// Stages a simple TrueType font with an embedded program and WinAnsiEncoding.
fn add_embedded_truetype(doc: &mut Document) {
    let program = font_program();
    let mut descriptor = Dictionary::new();
    descriptor.insert("Type".to_owned(), Object::Name("FontDescriptor".to_owned()));
    descriptor.insert(
        "FontName".to_owned(),
        Object::Name("CourierPrime".to_owned()),
    );
    descriptor.insert("Flags".to_owned(), Object::Integer(32));
    descriptor.insert(
        "FontBBox".to_owned(),
        Object::Array(vec![
            Object::Integer(-50),
            Object::Integer(-250),
            Object::Integer(650),
            Object::Integer(800),
        ]),
    );
    descriptor.insert("ItalicAngle".to_owned(), Object::Integer(0));
    descriptor.insert("Ascent".to_owned(), Object::Integer(800));
    descriptor.insert("Descent".to_owned(), Object::Integer(-250));
    descriptor.insert("CapHeight".to_owned(), Object::Integer(600));
    descriptor.insert("StemV".to_owned(), Object::Integer(80));
    descriptor.insert(
        "FontFile2".to_owned(),
        Object::Reference(ObjectId::new(EMBEDDED_FILE_ID, 0)),
    );
    doc.set_object(
        ObjectId::new(EMBEDDED_DESCRIPTOR_ID, 0),
        Object::Dictionary(descriptor),
    )
    .expect("stage descriptor");
    doc.set_object(
        ObjectId::new(EMBEDDED_FILE_ID, 0),
        Object::Stream {
            dictionary: Dictionary::new(),
            data: program,
        },
    )
    .expect("stage font program");

    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), Object::Name("TrueType".to_owned()));
    font.insert(
        "BaseFont".to_owned(),
        Object::Name("CourierPrime".to_owned()),
    );
    font.insert("FirstChar".to_owned(), Object::Integer(32));
    font.insert("LastChar".to_owned(), Object::Integer(255));
    font.insert(
        "Widths".to_owned(),
        Object::Array((32..=255).map(|_| Object::Integer(600)).collect()),
    );
    font.insert(
        "Encoding".to_owned(),
        Object::Name("WinAnsiEncoding".to_owned()),
    );
    font.insert(
        "FontDescriptor".to_owned(),
        Object::Reference(ObjectId::new(EMBEDDED_DESCRIPTOR_ID, 0)),
    );
    doc.set_object(ObjectId::new(EMBEDDED_FONT_ID, 0), Object::Dictionary(font))
        .expect("stage font");

    let mut fonts = Dictionary::new();
    fonts.insert(
        "FT1".to_owned(),
        Object::Reference(ObjectId::new(EMBEDDED_FONT_ID, 0)),
    );
    let mut default_resources = Dictionary::new();
    default_resources.insert("Font".to_owned(), Object::Dictionary(fonts));
    let mut acroform = dictionary(doc, acroform_id());
    acroform.insert("DR".to_owned(), Object::Dictionary(default_resources));
    doc.set_object(acroform_id(), Object::Dictionary(acroform))
        .expect("stage AcroForm DR");
}

/// Stages a Type0 Identity-H font whose ToUnicode covers `A`, `B`, `1`, `2`.
fn add_subset_type0(doc: &mut Document) {
    doc.set_object(
        ObjectId::new(SUBSET_FILE_ID, 0),
        Object::Stream {
            dictionary: Dictionary::new(),
            data: font_program(),
        },
    )
    .expect("stage subset program");

    let mut descriptor = Dictionary::new();
    descriptor.insert("Type".to_owned(), Object::Name("FontDescriptor".to_owned()));
    descriptor.insert(
        "FontName".to_owned(),
        Object::Name("ABCDEF+SubsetTest".to_owned()),
    );
    descriptor.insert("Flags".to_owned(), Object::Integer(32));
    descriptor.insert(
        "FontBBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(-200),
            Object::Integer(1000),
            Object::Integer(900),
        ]),
    );
    descriptor.insert("ItalicAngle".to_owned(), Object::Integer(0));
    descriptor.insert("Ascent".to_owned(), Object::Integer(800));
    descriptor.insert("Descent".to_owned(), Object::Integer(-200));
    descriptor.insert("CapHeight".to_owned(), Object::Integer(700));
    descriptor.insert("StemV".to_owned(), Object::Integer(80));
    descriptor.insert(
        "FontFile2".to_owned(),
        Object::Reference(ObjectId::new(SUBSET_FILE_ID, 0)),
    );
    doc.set_object(
        ObjectId::new(SUBSET_DESCRIPTOR_ID, 0),
        Object::Dictionary(descriptor),
    )
    .expect("stage subset descriptor");

    let mut system_info = Dictionary::new();
    system_info.insert("Registry".to_owned(), Object::String(b"Adobe".to_vec()));
    system_info.insert("Ordering".to_owned(), Object::String(b"Identity".to_vec()));
    system_info.insert("Supplement".to_owned(), Object::Integer(0));
    let mut cid_font = Dictionary::new();
    cid_font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    cid_font.insert(
        "Subtype".to_owned(),
        Object::Name("CIDFontType2".to_owned()),
    );
    cid_font.insert(
        "BaseFont".to_owned(),
        Object::Name("ABCDEF+SubsetTest".to_owned()),
    );
    cid_font.insert("CIDSystemInfo".to_owned(), Object::Dictionary(system_info));
    cid_font.insert(
        "FontDescriptor".to_owned(),
        Object::Reference(ObjectId::new(SUBSET_DESCRIPTOR_ID, 0)),
    );
    cid_font.insert("DW".to_owned(), Object::Integer(600));
    doc.set_object(
        ObjectId::new(SUBSET_CID_FONT_ID, 0),
        Object::Dictionary(cid_font),
    )
    .expect("stage CID font");

    let cmap = "begincmap\n\
        1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange\n\
        4 beginbfchar\n<0001> <0041>\n<0002> <0042>\n<0003> <0031>\n<0004> <0032>\nendbfchar\n\
        endcmap\n";
    doc.set_object(
        ObjectId::new(SUBSET_TO_UNICODE_ID, 0),
        Object::Stream {
            dictionary: Dictionary::new(),
            data: cmap.as_bytes().to_vec(),
        },
    )
    .expect("stage ToUnicode");

    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), Object::Name("Type0".to_owned()));
    font.insert(
        "BaseFont".to_owned(),
        Object::Name("ABCDEF+SubsetTest".to_owned()),
    );
    font.insert("Encoding".to_owned(), Object::Name("Identity-H".to_owned()));
    font.insert(
        "DescendantFonts".to_owned(),
        Object::Array(vec![Object::Reference(ObjectId::new(
            SUBSET_CID_FONT_ID,
            0,
        ))]),
    );
    font.insert(
        "ToUnicode".to_owned(),
        Object::Reference(ObjectId::new(SUBSET_TO_UNICODE_ID, 0)),
    );
    doc.set_object(ObjectId::new(SUBSET_FONT_ID, 0), Object::Dictionary(font))
        .expect("stage Type0 font");

    let mut fonts = Dictionary::new();
    fonts.insert(
        "FT2".to_owned(),
        Object::Reference(ObjectId::new(SUBSET_FONT_ID, 0)),
    );
    let mut acroform = dictionary(doc, acroform_id());
    let mut existing = acroform
        .get("DR")
        .and_then(Object::as_dictionary)
        .and_then(|dr| dr.get("Font"))
        .and_then(Object::as_dictionary)
        .cloned()
        .unwrap_or_default();
    for (name, object) in fonts {
        existing.insert(name, object);
    }
    let mut dr = acroform
        .get("DR")
        .and_then(Object::as_dictionary)
        .cloned()
        .unwrap_or_default();
    dr.insert("Font".to_owned(), Object::Dictionary(existing));
    acroform.insert("DR".to_owned(), Object::Dictionary(dr));
    doc.set_object(acroform_id(), Object::Dictionary(acroform))
        .expect("stage AcroForm DR");
}

/// Stages a text field/widget with the given `/DA` and optional `/MK /R`.
fn add_text_field(
    doc: &mut Document,
    id: u32,
    name: &str,
    default_appearance: &str,
    rect: [i64; 4],
    rotation: Option<i64>,
) -> ObjectId {
    let identifier = ObjectId::new(id, 0);
    let mut widget = Dictionary::new();
    widget.insert("Type".to_owned(), Object::Name("Annot".to_owned()));
    widget.insert("Subtype".to_owned(), Object::Name("Widget".to_owned()));
    widget.insert("FT".to_owned(), Object::Name("Tx".to_owned()));
    widget.insert("T".to_owned(), Object::String(name.as_bytes().to_vec()));
    widget.insert(
        "DA".to_owned(),
        Object::String(default_appearance.as_bytes().to_vec()),
    );
    widget.insert(
        "Rect".to_owned(),
        Object::Array(rect.into_iter().map(Object::Integer).collect()),
    );
    widget.insert("P".to_owned(), Object::Reference(page_id()));
    widget.insert("F".to_owned(), Object::Integer(4));
    if let Some(degrees) = rotation {
        let mut mk = Dictionary::new();
        mk.insert("R".to_owned(), Object::Integer(degrees));
        widget.insert("MK".to_owned(), Object::Dictionary(mk));
    }
    doc.set_object(identifier, Object::Dictionary(widget))
        .expect("stage text widget");

    let mut page = dictionary(doc, page_id());
    let mut annots = page
        .get("Annots")
        .and_then(Object::as_array)
        .expect("page Annots")
        .to_vec();
    annots.push(Object::Reference(identifier));
    page.insert("Annots".to_owned(), Object::Array(annots));
    doc.set_object(page_id(), Object::Dictionary(page))
        .expect("stage page");

    let mut acroform = dictionary(doc, acroform_id());
    let mut fields = acroform
        .get("Fields")
        .and_then(Object::as_array)
        .expect("AcroForm Fields")
        .to_vec();
    fields.push(Object::Reference(identifier));
    acroform.insert("Fields".to_owned(), Object::Array(fields));
    doc.set_object(acroform_id(), Object::Dictionary(acroform))
        .expect("stage AcroForm");

    identifier
}

/// Builds a fixture copy with an embedded TrueType field and a rotated field.
///
/// `pdfxi-core` keeps staged changes out of `Document::object`, so each staging
/// phase is saved and reopened before the next one reads the catalog. This is
/// the append-then-reload contract of ADR 0010 that T-0234 also uses.
fn fidelity_fixture() -> Document {
    let mut doc = load();
    add_embedded_truetype(&mut doc);
    let mut doc = save_and_reopen(&doc);
    add_subset_type0(&mut doc);
    let mut doc = save_and_reopen(&doc);

    add_text_field(
        &mut doc,
        EMBEDDED_FIELD_ID,
        "font-field",
        "/FT1 10 Tf 0 0 1 rg",
        [72, 500, 272, 520],
        None,
    );
    let mut doc = save_and_reopen(&doc);
    add_text_field(
        &mut doc,
        ROTATED_FIELD_ID,
        "rotated-field",
        "/FT1 10 Tf 0 g",
        [300, 450, 500, 490],
        Some(90),
    );
    let mut doc = save_and_reopen(&doc);
    add_text_field(
        &mut doc,
        SUBSET_FIELD_ID,
        "subset-field",
        "/FT2 10 Tf 0 g",
        [72, 400, 272, 420],
        None,
    );
    save_and_reopen(&doc)
}

#[test]
fn embedded_true_type_appearance_uses_the_field_font() {
    let mut doc = fidelity_fixture();
    let report = set_text_reported(&mut doc, "font-field", "Embedded invoice 42").expect("fill");
    assert_eq!(
        report.font(),
        Some(&AppearanceFont::FieldFont {
            resource: "FT1".to_owned(),
            embedded: true,
        })
    );
    assert!(report.all_field_fonts());
    assert_eq!(report.widgets[0].rotation, 0);
    assert!(report.widgets[0].matrix.is_identity());

    write_artifact("filled-font.pdf", &doc);

    let saved = save_and_reopen(&doc);
    let model = read_acroform(&saved).expect("read model");
    assert_eq!(
        model.field("font-field").expect("field").value,
        FieldValue::Text("Embedded invoice 42".to_owned())
    );
    let stream_id = normal_stream_id(&saved, ObjectId::new(EMBEDDED_FIELD_ID, 0));
    let (stream_dictionary, data) = stream(&saved, stream_id);
    assert_eq!(
        numbers(stream_dictionary.get("BBox").expect("BBox")),
        vec![0.0, 0.0, 200.0, 20.0]
    );
    assert!(
        !stream_dictionary.contains_key("Matrix"),
        "an upright widget must not carry a /Matrix"
    );
    assert!(contains(&data, b"/FT1 10 Tf"), "font operand missing");
    assert!(
        contains(&data, b"(Embedded invoice 42)"),
        "literal text missing"
    );
    assert!(!contains(&data, b"/Helv"), "the fallback must not be used");

    let resources = stream_dictionary
        .get("Resources")
        .and_then(Object::as_dictionary)
        .expect("appearance Resources");
    let font = resources
        .get("Font")
        .and_then(Object::as_dictionary)
        .and_then(|fonts| fonts.get("FT1"))
        .and_then(Object::as_reference)
        .expect("FT1 resource");
    assert_eq!(font, ObjectId::new(EMBEDDED_FONT_ID, 0));
}

#[test]
fn rotated_widget_matrix_places_the_text_inside_the_rect() {
    let mut doc = fidelity_fixture();
    let report = set_text_reported(&mut doc, "rotated-field", "Rotated value").expect("fill");
    assert_eq!(report.widgets[0].rotation, 90);
    assert_eq!(report.widgets[0].bbox, FormRect::new(0.0, 0.0, 40.0, 200.0));
    let expected = Matrix::new(0.0, 1.0, -1.0, 0.0, 200.0, 0.0);
    assert_eq!(report.widgets[0].matrix, expected);

    // The appearance text box, transformed through the generated matrix and the
    // standard BBox-to-Rect placement, must stay inside the widget rectangle.
    let text_rect = report.widgets[0].text_rect.expect("text was laid out");
    let placement = Matrix::fit_bbox(
        report.widgets[0].matrix,
        (0.0, 0.0, 40.0, 200.0),
        300.0,
        450.0,
        200.0,
        40.0,
    )
    .expect("placement");
    let (x0, y0, x1, y1) = placement
        .transform_bbox(
            text_rect.left,
            text_rect.bottom,
            text_rect.right,
            text_rect.top,
        )
        .expect("finite text bounds");
    assert!(
        x0 >= 299.99 && y0 >= 449.99,
        "({x0}, {y0}) escapes the rect"
    );
    assert!(
        x1 <= 500.01 && y1 <= 490.01,
        "({x1}, {y1}) escapes the rect"
    );
    // The text box must be non-degenerate and must map inside the widget quad.
    // For a 90-degree rotation the page-space height carries the text length.
    assert!(x1 - x0 > 1.0, "text must have a visible thickness");
    assert!(y1 - y0 > 20.0, "text must have a visible length");

    write_artifact("rotated.pdf", &doc);

    let saved = save_and_reopen(&doc);
    let stream_id = normal_stream_id(&saved, ObjectId::new(ROTATED_FIELD_ID, 0));
    let (stream_dictionary, data) = stream(&saved, stream_id);
    assert_eq!(
        numbers(stream_dictionary.get("BBox").expect("BBox")),
        vec![0.0, 0.0, 40.0, 200.0]
    );
    let matrix = numbers(stream_dictionary.get("Matrix").expect("Matrix"));
    assert_eq!(matrix, vec![0.0, 1.0, -1.0, 0.0, 200.0, 0.0]);
    assert!(contains(&data, b"/FT1 10 Tf"));

    // Flatten composes the stream /Matrix with the BBox-to-Rect placement, so
    // the emitted operator is the rotation plus the rect origin translation:
    // local (0,0) and (0,200) land on (500,450) and (300,450).
    let mut flattened = saved;
    flatten(&mut flattened).expect("flatten");
    let reopened = save_and_reopen(&flattened);
    let page = reopened.page(0).expect("page");
    let page_dictionary = dictionary(&reopened, page.id);
    let contents = page_dictionary
        .get("Contents")
        .and_then(Object::as_array)
        .expect("contents");
    let paint_id = contents
        .last()
        .and_then(Object::as_reference)
        .expect("paint stream");
    let (_, paint) = stream(&reopened, paint_id);
    let paint = String::from_utf8_lossy(&paint);
    assert!(
        paint.contains("0 1 -1 0 500 450 cm"),
        "expected the composed rotation to be preserved: {paint}"
    );
}

#[test]
fn unencodable_values_report_the_fallback_and_the_character() {
    let mut doc = fidelity_fixture();

    // The subset CMap covers A, B, 1 and 2, so this value is written as CIDs.
    let report = set_text_reported(&mut doc, "subset-field", "AB12").expect("fill subset");
    assert_eq!(
        report.font(),
        Some(&AppearanceFont::FieldFont {
            resource: "FT2".to_owned(),
            embedded: true,
        })
    );
    let saved = save_and_reopen(&doc);
    let stream_id = normal_stream_id(&saved, ObjectId::new(SUBSET_FIELD_ID, 0));
    let (_, data) = stream(&saved, stream_id);
    assert!(contains(&data, b"/FT2 10 Tf"));
    assert!(
        contains(&data, b"<0001000200030004> Tj"),
        "CID codes missing"
    );

    // A euro sign is not in the subset CMap, so the appearance falls back.
    let report = set_text_reported(&mut doc, "subset-field", "\u{20AC}").expect("fill euro");
    assert_eq!(
        report.font(),
        Some(&AppearanceFont::Fallback {
            requested: "FT2".to_owned(),
            reason: FallbackReason::CannotEncode,
        })
    );
    assert_eq!(
        fallback_reason_name(FallbackReason::CannotEncode),
        "cannot_encode"
    );
    let saved = save_and_reopen(&doc);
    let stream_id = normal_stream_id(&saved, ObjectId::new(SUBSET_FIELD_ID, 0));
    let (stream_dictionary, data) = stream(&saved, stream_id);
    // WinAnsi encodes the euro sign as 0x80, written as an octal escape.
    assert!(contains(&data, b"(\\200)"), "WinAnsi euro byte missing");
    let fonts = stream_dictionary
        .get("Resources")
        .and_then(Object::as_dictionary)
        .and_then(|resources| resources.get("Font"))
        .and_then(Object::as_dictionary)
        .expect("font resources");
    // The appearance resource must now point at a Helvetica fallback object,
    // never back at the Type0 font that could not represent the value.
    let fallback_id = fonts
        .get("FT2")
        .and_then(Object::as_reference)
        .expect("FT2 resource");
    assert_ne!(fallback_id, ObjectId::new(SUBSET_FONT_ID, 0));
    let fallback = dictionary(&saved, fallback_id);
    assert_eq!(
        fallback.get("BaseFont").and_then(Object::as_name),
        Some("Helvetica")
    );

    // A CJK value fails the same way for both fonts.
    let report = set_text_reported(&mut doc, "font-field", "\u{4E2D}\u{6587}").expect("fill cjk");
    assert_eq!(
        report.font(),
        Some(&AppearanceFont::Fallback {
            requested: "FT1".to_owned(),
            reason: FallbackReason::CannotEncode,
        })
    );
}

#[test]
fn rotated_checkbox_keeps_its_active_state_and_gains_a_matrix() {
    let mut doc = load();
    let mut widget = dictionary(&doc, checkbox_widget_id());
    let mut mk = Dictionary::new();
    mk.insert("R".to_owned(), Object::Integer(180));
    widget.insert("MK".to_owned(), Object::Dictionary(mk));
    doc.set_object(checkbox_widget_id(), Object::Dictionary(widget))
        .expect("stage checkbox /MK");
    let mut doc = save_and_reopen(&doc);

    let report = set_checkbox_reported(&mut doc, "checkbox-field", true).expect("tick");
    assert_eq!(report.widgets[0].rotation, 180);
    assert_eq!(report.widgets[0].font, None);

    let saved = save_and_reopen(&doc);
    let widget = dictionary(&saved, checkbox_widget_id());
    assert_eq!(
        widget.get("AS").and_then(Object::as_name),
        Some("Yes"),
        "the active state must survive the rotation"
    );
    let states = widget
        .get("AP")
        .and_then(Object::as_dictionary)
        .and_then(|ap| ap.get("N"))
        .and_then(Object::as_dictionary)
        .expect("state dictionary");
    for state in states.values() {
        let id = state.as_reference().expect("state stream");
        let (stream_dictionary, _) = stream(&saved, id);
        assert_eq!(
            numbers(stream_dictionary.get("Matrix").expect("Matrix")),
            vec![-1.0, 0.0, 0.0, -1.0, 20.0, 20.0]
        );
    }
}
