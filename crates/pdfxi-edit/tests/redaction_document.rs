//! Permanent redaction on a real-world document through the public library API.
//!
//! Every test opens `fixtures/realworld/microsoft-excel-2019-sample.pdf`, adds
//! text and annotations with the T-0233/T-0230 APIs, redacts through
//! [`pdfxi_edit::apply_redaction`], saves a full rewrite, and reopens the result
//! before asserting. Test (a) also writes the redacted file to
//! `target/lanes/author3/t0237-out/redacted.pdf` for independent inspection.

use std::io::Cursor;
use std::path::{Path, PathBuf};

use pdfxi_core::{Dictionary, Document, DocumentInfo, Object, ObjectId, XmpMetadata};
use pdfxi_edit::{
    DocumentRedactionError, ImageFormat as PdfImageFormat, Point, Rect, RedactionMark,
    RedactionRequest, TextAlign, add_image, add_text_box, add_text_note, apply_redaction,
    list_annotations, verify_redaction,
};
use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{CancellationToken, PageIndex, RenderedPage, Renderer};

fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn rect(x0: f32, y0: f32, x1: f32, y1: f32) -> Rect {
    Rect::new(x0, y0, x1, y1).expect("test rectangle")
}

fn output_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("target/lanes/author3/t0237-out/redacted.pdf")
}

/// T-0268 form artifacts, in the lane target directory named by the task.
fn form_output_path(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("target/lanes/author5/t0268-out")
        .join(name)
}

/// Return the first original word long enough to be a stable survival probe.
fn survival_probe(text: &str) -> String {
    text.split_whitespace()
        .find(|word| {
            word.len() >= 5
                && word.chars().all(|character| character.is_alphanumeric())
                && !word.to_ascii_lowercase().contains("secret")
        })
        .unwrap_or("Report")
        .to_owned()
}

#[test]
fn redacts_text_annotation_and_metadata_then_verifies_clean() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let original_text = document.page_text(0).expect("page 1 text before editing");
    let probe = survival_probe(&original_text);

    add_text_box(
        &mut document,
        0,
        rect(72.0, 72.0, 400.0, 140.0),
        "The hidden line says SECRET-ALPHA-42 and must vanish.",
        11.0,
        TextAlign::Left,
    )
    .expect("insert secret text box");
    add_text_note(
        &mut document,
        0,
        Point::new(300.0, 100.0),
        "SECRET-ALPHA-42 note",
        "Alice",
        "secret-note",
    )
    .expect("insert note annotation");
    let annotations_before = list_annotations(&document, 0)
        .expect("annotations before")
        .len();

    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(60.0, 60.0, 420.0, 160.0))],
        remove_annotations: true,
        remove_links: true,
        remove_form_fields: true,
        remove_metadata: true,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply redaction");
    assert!(
        report
            .removed_runs
            .iter()
            .any(|run| run.text.contains("SECRET-ALPHA-42")),
        "the inserted run must be reported as removed: {report:?}"
    );
    assert!(
        report.removed_annotations >= 1,
        "note annotation must be removed"
    );

    let bytes = document.save().expect("full rewrite");
    let path = output_path();
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output directory");
    }
    std::fs::write(&path, &bytes).expect("write redacted artifact");

    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let text = reopened.page_text(0).expect("page 1 text after redaction");
    assert!(!text.contains("SECRET-ALPHA-42"), "secret survives: {text}");
    assert!(
        !text.contains("hidden line"),
        "marked sentence survives: {text}"
    );
    assert!(text.contains(&probe), "unrelated text {probe:?} was lost");
    assert!(
        list_annotations(&reopened, 0)
            .expect("annotations after")
            .len()
            < annotations_before,
        "annotation count did not drop"
    );

    let verification = verify_redaction(&reopened, &["SECRET-ALPHA-42", "hidden line"], &report)
        .expect("verify redaction");
    assert!(
        verification.clean,
        "survivors: {:?}",
        verification.survivors
    );
}

#[test]
fn empty_region_redaction_changes_nothing() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_box(
        &mut document,
        0,
        rect(100.0, 600.0, 400.0, 640.0),
        "Visible panel text stays put.",
        11.0,
        TextAlign::Left,
    )
    .expect("insert text box");
    add_text_note(
        &mut document,
        0,
        Point::new(500.0, 600.0),
        "Outside note",
        "Bob",
        "outside-note",
    )
    .expect("insert note");
    let baseline = Document::from_bytes(document.save().expect("baseline save")).expect("baseline");
    let before_text = baseline.page_text(0).expect("text before");
    let before_annotations = list_annotations(&baseline, 0)
        .expect("annotations before")
        .len();

    let request = RedactionRequest {
        // A small corner mark over blank page margin.
        marks: vec![RedactionMark::new(0, rect(2.0, 2.0, 12.0, 12.0))],
        remove_annotations: true,
        remove_links: true,
        remove_form_fields: true,
        remove_metadata: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply redaction");
    assert!(report.removed_runs.is_empty(), "blank mark removed text");
    assert!(
        report.surviving_spans.is_empty(),
        "blank mark left a survivor"
    );

    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    assert_eq!(
        reopened.page_text(0).expect("text after"),
        before_text,
        "text changed for an empty mark"
    );
    assert_eq!(
        list_annotations(&reopened, 0)
            .expect("annotations after")
            .len(),
        before_annotations,
        "annotations changed for an empty mark"
    );
}

#[test]
fn verifier_catches_a_secret_left_outside_the_mark() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_box(
        &mut document,
        0,
        rect(72.0, 72.0, 400.0, 110.0),
        "INSIDE-ONLY-11 is marked.",
        11.0,
        TextAlign::Left,
    )
    .expect("insert inside text");
    add_text_box(
        &mut document,
        0,
        rect(72.0, 300.0, 500.0, 340.0),
        "SECRET-OUTSIDE-77 remains visible.",
        11.0,
        TextAlign::Left,
    )
    .expect("insert outside text");

    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(60.0, 60.0, 420.0, 120.0))],
        remove_annotations: true,
        remove_links: true,
        remove_form_fields: true,
        remove_metadata: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply redaction");
    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let text = reopened.page_text(0).expect("text after");
    assert!(!text.contains("INSIDE-ONLY-11"), "inside secret survived");

    let verification =
        verify_redaction(&reopened, &["INSIDE-ONLY-11", "SECRET-OUTSIDE-77"], &report)
            .expect("verify redaction");
    assert!(!verification.clean, "verifier missed a visible secret");
    assert!(
        verification
            .survivors
            .iter()
            .any(|survivor| survivor.detail.contains("SECRET-OUTSIDE-77")),
        "no survivor names the outside secret: {:?}",
        verification.survivors
    );
    assert!(
        verification
            .survivors
            .iter()
            .any(|survivor| survivor.page == Some(0)),
        "survivor has no page location: {:?}",
        verification.survivors
    );
}

#[test]
fn partial_intersection_removes_only_the_marked_glyphs() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    add_text_box(
        &mut document,
        0,
        rect(72.0, 400.0, 400.0, 440.0),
        "ABCDEFGHIJ",
        12.0,
        TextAlign::Left,
    )
    .expect("insert probe text");

    // The box's first glyph starts at x=72 and each Courier Prime advance is
    // about 7.2pt at 12pt, so this mark covers glyphs D through I.
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(95.0, 400.0, 130.0, 440.0))],
        remove_annotations: true,
        remove_links: true,
        remove_form_fields: true,
        remove_metadata: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply redaction");
    let split = report
        .removed_runs
        .iter()
        .find(|run| run.text.contains("ABCDEFGHIJ"))
        .expect("probe run reported");
    assert!(
        !split.whole && split.kept_glyphs > 0 && split.removed_glyphs > 0,
        "expected a split removal, got {split:?}"
    );

    let bytes = document.save().expect("full rewrite");
    let path = output_path().with_file_name("partial.pdf");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output directory");
    }
    std::fs::write(&path, &bytes).expect("write partial artifact");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let text = reopened.page_text(0).expect("text after");
    assert!(
        !text.contains("ABCDEFGHIJ"),
        "the full run survived: {text}"
    );
    assert!(text.contains("ABCJ"), "kept glyphs were lost: {text}");

    let verification =
        verify_redaction(&reopened, &["ABCDEFGHIJ"], &report).expect("verify redaction");
    assert!(
        verification.clean,
        "survivors: {:?}",
        verification.survivors
    );
}

// ---------------------------------------------------------------------------
// Image pixel redaction
// ---------------------------------------------------------------------------

fn rgb_png(red: u8, green: u8, blue: u8, size: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(size, size, image::Rgb([red, green, blue]));
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, image::ImageFormat::Png)
        .expect("encode PNG");
    cursor.into_inner()
}

fn rgb_jpeg(red: u8, green: u8, blue: u8, size: u32) -> Vec<u8> {
    let image = image::RgbImage::from_pixel(size, size, image::Rgb([red, green, blue]));
    let mut cursor = Cursor::new(Vec::new());
    image
        .write_to(&mut cursor, image::ImageFormat::Jpeg)
        .expect("encode JPEG");
    cursor.into_inner()
}

fn page_dictionary(document: &Document) -> Dictionary {
    let page = document.page(0).expect("page metadata");
    document
        .object(page.id)
        .expect("page object")
        .as_dictionary()
        .expect("page dictionary")
        .clone()
}

fn resolved(document: &Document, value: &Object) -> Object {
    match value {
        Object::Reference(id) => document.object(*id).expect("resolved object"),
        other => other.clone(),
    }
}

fn page_resources(document: &Document) -> Dictionary {
    let page = page_dictionary(document);
    match page.get("Resources") {
        Some(value) => resolved(document, value)
            .as_dictionary()
            .expect("resource dictionary")
            .clone(),
        None => Dictionary::new(),
    }
}

/// Return the image XObject used by a resource name as `(dictionary, data)`.
fn image_xobject(document: &Document, resource: &str) -> (Dictionary, Vec<u8>) {
    let resources = page_resources(document);
    let xobjects = resources
        .get("XObject")
        .map(|value| resolved(document, value))
        .and_then(|value| value.as_dictionary().cloned())
        .expect("XObject dictionary");
    let id = xobjects
        .get(resource)
        .and_then(Object::as_reference)
        .expect("image reference");
    match document.object(id).expect("image object") {
        Object::Stream { dictionary, data } => (dictionary, data),
        other => panic!("image is not a stream: {other:?}"),
    }
}

fn flate_samples(dictionary: &Dictionary, data: &[u8]) -> Vec<u8> {
    let filter = dictionary.get("Filter").and_then(Object::as_name);
    assert_eq!(
        filter,
        Some("FlateDecode"),
        "image must be Flate re-encoded"
    );
    pdfxi_core::decode_stream(data, &[pdfxi_core::Filter::FlateDecode(Default::default())])
        .expect("decode Flate image")
}

fn pixel(samples: &[u8], width: u32, x: u32, y: u32) -> [u8; 3] {
    let offset = ((y as usize * width as usize) + x as usize) * 3;
    [samples[offset], samples[offset + 1], samples[offset + 2]]
}

#[test]
fn image_pixels_under_a_mark_are_black_and_metadata_is_gone() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let info = DocumentInfo {
        title: Some("Secret title".to_owned()),
        author: Some("Secret author".to_owned()),
        ..DocumentInfo::default()
    };
    document.set_info(&info).expect("set info");
    let xmp = XmpMetadata {
        title: Some("Secret title".to_owned()),
        creator: Some("Secret author".to_owned()),
        ..XmpMetadata::default()
    };
    document.set_xmp_metadata(&xmp).expect("set XMP");

    let png = rgb_png(255, 0, 0, 16);
    let placement = add_image(
        &mut document,
        0,
        rect(300.0, 400.0, 340.0, 440.0),
        &png,
        PdfImageFormat::Png,
    )
    .expect("place red PNG");

    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        remove_metadata: true,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply image redaction");
    assert!(
        report.clean,
        "image redaction survivors: {:?}",
        report.surviving_spans
    );

    let bytes = document.save().expect("full rewrite");
    let path = output_path().with_file_name("image-redacted.pdf");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output directory");
    }
    std::fs::write(&path, &bytes).expect("write image artifact");

    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let (dictionary, data) = image_xobject(&reopened, &placement.resource);
    let width = dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .expect("image width") as u32;
    let samples = flate_samples(&dictionary, &data);
    let inside = pixel(&samples, width, 6, 10);
    let outside = pixel(&samples, width, 14, 2);
    assert_eq!(inside, [0, 0, 0], "inside mark is not black");
    assert_eq!(outside, [255, 0, 0], "outside mark changed");

    let info = reopened.info().expect("info read").expect("info present");
    assert_eq!(info.title, None, "Info title survived");
    assert_eq!(info.author, None, "Info author survived");
    assert!(
        reopened.xmp_metadata().expect("xmp read").is_none(),
        "XMP metadata survived"
    );
}

#[test]
fn jpeg_image_under_a_mark_is_decoded_refilled_and_flated() {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let jpeg = rgb_jpeg(255, 0, 0, 16);
    let placement = add_image(
        &mut document,
        0,
        rect(300.0, 400.0, 340.0, 440.0),
        &jpeg,
        PdfImageFormat::Jpeg,
    )
    .expect("place red JPEG");
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        ..RedactionRequest::default()
    };
    apply_redaction(&mut document, &request).expect("apply image redaction");
    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let (dictionary, data) = image_xobject(&reopened, &placement.resource);
    let width = dictionary
        .get("Width")
        .and_then(Object::as_i64)
        .expect("image width") as u32;
    assert_eq!(
        dictionary.get("Filter").and_then(Object::as_name),
        Some("FlateDecode"),
        "JPEG must be re-encoded as Flate"
    );
    let samples = flate_samples(&dictionary, &data);
    let inside = pixel(&samples, width, 6, 10);
    let outside = pixel(&samples, width, 14, 2);
    assert_eq!(inside, [0, 0, 0], "inside mark is not black");
    assert!(
        outside[0] > 200 && outside[1] < 40 && outside[2] < 40,
        "outside mark lost its red JPEG colour: {outside:?}"
    );
}

/// Build a document whose page paints a dummy JPXDecode image under a mark.
fn jpx_document() -> Document {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let page = document.page(0).expect("page");
    let mut page_dictionary = page_dictionary(&document);
    let mut resources = match page_dictionary.get("Resources") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("resources")
            .as_dictionary()
            .expect("resource dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    let mut xobjects = match resources.get("XObject") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("xobjects")
            .as_dictionary()
            .expect("XObject dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    let image_id = document.allocate_object_number().expect("object number");
    let mut image_dictionary = Dictionary::new();
    image_dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    image_dictionary.insert("Subtype".to_owned(), Object::Name("Image".to_owned()));
    image_dictionary.insert("Width".to_owned(), Object::Integer(16));
    image_dictionary.insert("Height".to_owned(), Object::Integer(16));
    image_dictionary.insert(
        "ColorSpace".to_owned(),
        Object::Name("DeviceRGB".to_owned()),
    );
    image_dictionary.insert("BitsPerComponent".to_owned(), Object::Integer(8));
    image_dictionary.insert("Filter".to_owned(), Object::Name("JPXDecode".to_owned()));
    document
        .set_object(
            ObjectId::new(image_id, 0),
            Object::Stream {
                dictionary: image_dictionary,
                data: vec![0u8; 32],
            },
        )
        .expect("stage image");
    xobjects.insert(
        "ImJPX".to_owned(),
        Object::Reference(ObjectId::new(image_id, 0)),
    );
    resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
    page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));

    let content_id = document.allocate_object_number().expect("content number");
    document
        .set_object(
            ObjectId::new(content_id, 0),
            Object::Stream {
                dictionary: Dictionary::new(),
                data: b"q 40 0 0 40 300 400 cm /ImJPX Do Q".to_vec(),
            },
        )
        .expect("stage content");
    let mut contents = page.contents.clone();
    contents.push(ObjectId::new(content_id, 0));
    page_dictionary.insert(
        "Contents".to_owned(),
        Object::Array(contents.into_iter().map(Object::Reference).collect()),
    );
    document
        .set_object(page.id, Object::Dictionary(page_dictionary))
        .expect("stage page");
    document
}

#[test]
fn intersecting_jpx_image_fails_closed_without_saving() {
    let mut document = jpx_document();
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        keep_images: false,
        ..RedactionRequest::default()
    };
    match apply_redaction(&mut document, &request) {
        Err(DocumentRedactionError::UnsupportedImage { detail, .. }) => {
            assert!(detail.contains("JPXDecode"), "unexpected detail: {detail}");
        }
        other => panic!("expected UnsupportedImage, got {other:?}"),
    }
    assert!(
        !output_path().with_file_name("jpx-redacted.pdf").exists(),
        "a failed redaction must not write an artifact"
    );

    let mut opt_out = jpx_document();
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        keep_images: true,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut opt_out, &request).expect("opt-out succeeds");
    assert!(!report.clean, "opt-out did not report a survivor");
    assert!(
        report
            .surviving_spans
            .iter()
            .any(|span| span.text.contains("image XObject")),
        "no image survivor reported: {:?}",
        report.surviving_spans
    );
}

/// Build a fixture document whose page paints `/name Do` over a known rect.
fn document_with_xobject(name: &str, object: Option<Object>) -> Document {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let page = document.page(0).expect("page");
    let mut page_dictionary = page_dictionary(&document);
    let mut resources = match page_dictionary.get("Resources") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("resources")
            .as_dictionary()
            .expect("resource dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    let mut xobjects = match resources.get("XObject") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("xobjects")
            .as_dictionary()
            .expect("XObject dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    if let Some(object) = object {
        let id = document.allocate_object_number().expect("object number");
        document
            .set_object(ObjectId::new(id, 0), object)
            .expect("stage XObject");
        xobjects.insert(name.to_owned(), Object::Reference(ObjectId::new(id, 0)));
    }
    resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
    page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));

    let content_id = document.allocate_object_number().expect("content number");
    document
        .set_object(
            ObjectId::new(content_id, 0),
            Object::Stream {
                dictionary: Dictionary::new(),
                data: format!("q 40 0 0 40 300 400 cm /{name} Do Q").into_bytes(),
            },
        )
        .expect("stage content");
    let mut contents = page.contents.clone();
    contents.push(ObjectId::new(content_id, 0));
    page_dictionary.insert(
        "Contents".to_owned(),
        Object::Array(contents.into_iter().map(Object::Reference).collect()),
    );
    document
        .set_object(page.id, Object::Dictionary(page_dictionary))
        .expect("stage page");
    document
}

/// A Helvetica resource with uniform widths so glyph geometry is predictable.
fn helvetica_font() -> Object {
    let mut font = Dictionary::new();
    font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    font.insert("Subtype".to_owned(), Object::Name("Type1".to_owned()));
    font.insert("BaseFont".to_owned(), Object::Name("Helvetica".to_owned()));
    font.insert("FirstChar".to_owned(), Object::Integer(32));
    font.insert("LastChar".to_owned(), Object::Integer(126));
    font.insert(
        "Widths".to_owned(),
        Object::Array((32..=126).map(|_| Object::Integer(500)).collect()),
    );
    Object::Dictionary(font)
}

/// A form XObject whose content shows `text` inside a 120x40 box.
fn form_with_text(text: &str) -> Object {
    let mut fonts = Dictionary::new();
    fonts.insert("F1".to_owned(), helvetica_font());
    let mut resources = Dictionary::new();
    resources.insert("Font".to_owned(), Object::Dictionary(fonts));
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(120),
            Object::Integer(40),
        ]),
    );
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: format!("BT /F1 12 Tf 5 15 Td ({text}) Tj ET").into_bytes(),
    }
}

/// A form XObject that paints one nested form under `/name`.
fn form_with_child(name: &str, child: ObjectId) -> Object {
    let mut xobjects = Dictionary::new();
    xobjects.insert(name.to_owned(), Object::Reference(child));
    let mut resources = Dictionary::new();
    resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(120),
            Object::Integer(40),
        ]),
    );
    dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));
    Object::Stream {
        dictionary,
        data: format!("q 1 0 0 1 0 0 cm /{name} Do Q").into_bytes(),
    }
}

/// Stage one object and return its identity.
fn stage_object(document: &mut Document, object: Object) -> ObjectId {
    let number = document.allocate_object_number().expect("object number");
    let id = ObjectId::new(number, 0);
    document.set_object(id, object).expect("stage object");
    id
}

/// Build the fixture with one page-level XObject painted at `(72, 30)`.
///
/// The builder receives the document so a test can stage nested form objects
/// first and return the outer form's identity.
fn staged_form_document(content: &str, build: impl FnOnce(&mut Document) -> ObjectId) -> Document {
    let mut document = Document::from_bytes(fixture()).expect("fixture parses");
    let page = document.page(0).expect("page");
    let mut page_dictionary = page_dictionary(&document);
    let mut resources = match page_dictionary.get("Resources") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("resources")
            .as_dictionary()
            .expect("resource dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    let form_id = build(&mut document);
    let mut xobjects = match resources.get("XObject") {
        Some(Object::Reference(id)) => document
            .object(*id)
            .expect("xobjects")
            .as_dictionary()
            .expect("XObject dictionary")
            .clone(),
        Some(Object::Dictionary(dictionary)) => dictionary.clone(),
        _ => Dictionary::new(),
    };
    xobjects.insert("FormDo".to_owned(), Object::Reference(form_id));
    resources.insert("XObject".to_owned(), Object::Dictionary(xobjects));
    page_dictionary.insert("Resources".to_owned(), Object::Dictionary(resources));

    let content_id = document.allocate_object_number().expect("content number");
    document
        .set_object(
            ObjectId::new(content_id, 0),
            Object::Stream {
                dictionary: Dictionary::new(),
                data: content.as_bytes().to_vec(),
            },
        )
        .expect("stage content");
    let mut contents = page.contents.clone();
    contents.push(ObjectId::new(content_id, 0));
    page_dictionary.insert(
        "Contents".to_owned(),
        Object::Array(contents.into_iter().map(Object::Reference).collect()),
    );
    document
        .set_object(page.id, Object::Dictionary(page_dictionary))
        .expect("stage page");
    document
}

/// The placement used by the form tests: the form paints over `(72, 30)-(192, 70)`.
const FORM_PLACEMENT: &str = "q 1 0 0 1 72 30 cm /FormDo Do Q";

/// A mark that covers the whole form placement.
fn form_mark() -> Rect {
    rect(70.0, 25.0, 200.0, 75.0)
}

/// Dark pixel count inside a page-space rectangle, on a 1:1 render.
fn dark_pixels_in(page: &RenderedPage, rect: Rect) -> usize {
    let width = page.width() as usize;
    let height = page.height() as usize;
    let scale_x = page.width() as f32 / 612.0;
    let scale_y = page.height() as f32 / 792.0;
    let left = ((rect.x0().max(0.0) * scale_x) as usize).min(width);
    let right = ((rect.x1().clamp(0.0, 612.0) * scale_x) as usize).min(width);
    let top = (((792.0 - rect.y1().clamp(0.0, 792.0)) * scale_y) as usize).min(height);
    let bottom = (((792.0 - rect.y0().clamp(0.0, 792.0)) * scale_y) as usize).min(height);
    if left >= right || top >= bottom {
        return 0;
    }
    let pixels = page.pixels();
    let mut count = 0usize;
    for y in top..bottom {
        for x in left..right {
            let index = (y * width + x) * 4;
            let Some(pixel) = pixels.get(index..index + 4) else {
                continue;
            };
            let luma =
                (u32::from(pixel[0]) * 299 + u32::from(pixel[1]) * 587 + u32::from(pixel[2]) * 114)
                    / 1000;
            if luma < 128 {
                count += 1;
            }
        }
    }
    count
}

fn render_dark_pixels(bytes: Vec<u8>, rect: Rect) -> usize {
    let renderer = PdfiumRenderer::from_bytes(bytes).expect("PDFium must open the document");
    let page = renderer
        .render_page(
            PageIndex::new(0),
            1.0,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("PDFium must rasterize the page");
    dark_pixels_in(&page, rect)
}

/// Build a zlib stream with stored DEFLATE blocks (test-only encoder).
fn zlib_stored(data: &[u8]) -> Vec<u8> {
    let mut out = vec![0x78, 0x01];
    let mut position = 0usize;
    loop {
        let chunk = (data.len() - position).min(65_535);
        let final_block = position + chunk == data.len();
        out.push(u8::from(final_block));
        let length = chunk as u16;
        out.extend_from_slice(&length.to_le_bytes());
        out.extend_from_slice(&(!length).to_le_bytes());
        out.extend_from_slice(&data[position..position + chunk]);
        position += chunk;
        if final_block {
            break;
        }
    }
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    out.extend_from_slice(&((b << 16) | a).to_be_bytes());
    out
}

#[test]
fn compressed_form_text_under_a_mark_is_removed() {
    let Object::Stream {
        mut dictionary,
        data,
    } = form_with_text("COMPRESSED-SECRET")
    else {
        panic!("form builder must return a stream");
    };
    dictionary.insert("Filter".to_owned(), Object::Name("FlateDecode".to_owned()));
    let compressed = zlib_stored(&data);
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        stage_object(
            document,
            Object::Stream {
                dictionary,
                data: compressed,
            },
        )
    });
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply compressed form redaction");
    assert!(
        report
            .removed_runs
            .iter()
            .any(|run| run.text.contains("COMPRESSED-SECRET")),
        "compressed form removal was not reported: {:?}",
        report.removed_runs
    );
    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    let (dictionary, data) = image_xobject(&reopened, "FormDo");
    assert!(
        !dictionary.contains_key("Filter"),
        "the rewritten form must be stored unfiltered"
    );
    assert!(
        !String::from_utf8_lossy(&data).contains("COMPRESSED-SECRET"),
        "the rewritten form stream still contains the secret"
    );
    let verification = verify_redaction(&reopened, &["COMPRESSED-SECRET"], &report)
        .expect("verify compressed form redaction");
    assert!(
        verification.clean,
        "survivors: {:?}",
        verification.survivors
    );
}

/// Where the form's text is painted: form-space `(5, 15)` at 12 pt.
fn form_text_rect() -> Rect {
    rect(74.0, 39.0, 190.0, 60.0)
}

#[test]
fn form_xobject_text_under_a_mark_is_removed() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        stage_object(document, form_with_text("FORM-SECRET"))
    });
    let before_text = document.page_text(0).expect("text before");
    let probe = survival_probe(&before_text);
    let before_bytes = document.save().expect("save before redaction");
    let source = document.bytes().to_vec();

    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply form redaction");
    assert_eq!(
        document.bytes(),
        source.as_slice(),
        "apply_redaction mutated the source bytes"
    );
    assert!(report.clean, "survivors: {:?}", report.surviving_spans);
    assert!(
        report
            .removed_runs
            .iter()
            .any(|run| run.text.contains("FORM-SECRET") && run.page == 0),
        "form text removal was not reported: {:?}",
        report.removed_runs
    );

    let bytes = document.save().expect("full rewrite");
    let path = form_output_path("form-redacted.pdf");
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent).expect("create output directory");
    }
    std::fs::write(&path, &bytes).expect("write form artifact");

    let reopened = Document::from_bytes(bytes.clone()).expect("redacted file reparses");
    assert!(
        !reopened
            .page_text(0)
            .expect("text after")
            .contains("FORM-SECRET"),
        "form secret still extracts from page text"
    );
    let (dictionary, data) = image_xobject(&reopened, "FormDo");
    assert!(
        !dictionary.contains_key("Filter"),
        "a rewritten form must be stored unfiltered"
    );
    assert!(
        !String::from_utf8_lossy(&data).contains("FORM-SECRET"),
        "the rewritten form stream still contains the secret"
    );
    assert!(
        reopened.page_text(0).expect("text after").contains(&probe),
        "unrelated page text {probe:?} was lost"
    );
    assert_eq!(
        document.page_count().expect("pages before"),
        reopened.page_count().expect("pages after"),
        "page count changed"
    );

    let verification =
        verify_redaction(&reopened, &["FORM-SECRET"], &report).expect("verify form redaction");
    assert!(
        verification.clean,
        "survivors: {:?}",
        verification.survivors
    );

    let before_dark = render_dark_pixels(before_bytes, form_text_rect());
    let after_dark = render_dark_pixels(bytes, form_text_rect());
    assert!(
        before_dark > 10,
        "the form text did not render before redaction: {before_dark} dark pixels"
    );
    assert_eq!(
        after_dark, 0,
        "PDFium still paints form ink after redaction: {before_dark} -> {after_dark}"
    );
}

#[test]
fn form_xobject_opt_out_is_a_survivor() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        stage_object(document, form_with_text("FORM-SECRET"))
    });
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: true,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("opt-out succeeds");
    assert!(!report.clean, "the form was not reported as a survivor");
    assert!(
        report
            .surviving_spans
            .iter()
            .any(|span| span.text.contains("form XObject FormDo")),
        "survivor does not name the form: {:?}",
        report.surviving_spans
    );
}

#[test]
fn nested_form_text_under_a_mark_is_removed() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        let inner = stage_object(document, form_with_text("NESTED-SECRET"));
        stage_object(document, form_with_child("Inner", inner))
    });
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply nested form redaction");
    assert!(
        report
            .removed_runs
            .iter()
            .any(|run| run.text.contains("NESTED-SECRET")),
        "nested form removal was not reported: {:?}",
        report.removed_runs
    );
    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes).expect("redacted file reparses");
    assert!(
        !reopened
            .page_text(0)
            .expect("text after")
            .contains("NESTED-SECRET")
    );
    let verification = verify_redaction(&reopened, &["NESTED-SECRET"], &report)
        .expect("verify nested form redaction");
    assert!(
        verification.clean,
        "survivors: {:?}",
        verification.survivors
    );
}

#[test]
fn deeply_nested_form_fails_closed() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        let mut child = stage_object(document, form_with_text("DEEP-SECRET"));
        for index in 0..20 {
            child = stage_object(document, form_with_child(&format!("Child{index}"), child));
        }
        child
    });
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    match apply_redaction(&mut document, &request) {
        Err(DocumentRedactionError::UnsupportedForm { detail, .. }) => {
            assert!(
                detail.contains("nesting exceeds"),
                "unexpected detail: {detail}"
            );
        }
        other => panic!("expected UnsupportedForm, got {other:?}"),
    }
}

#[test]
fn cyclic_form_xobject_fails_closed() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        let first = ObjectId::new(document.allocate_object_number().expect("number"), 0);
        let second = ObjectId::new(document.allocate_object_number().expect("number"), 0);
        document
            .set_object(first, form_with_child("B", second))
            .expect("stage first form");
        document
            .set_object(second, form_with_child("A", first))
            .expect("stage second form");
        first
    });
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    match apply_redaction(&mut document, &request) {
        Err(DocumentRedactionError::UnsupportedForm { detail, .. }) => {
            assert!(detail.contains("cycle"), "unexpected detail: {detail}");
        }
        other => panic!("expected UnsupportedForm, got {other:?}"),
    }
    assert!(
        !form_output_path("form-cycle.pdf").exists(),
        "a failed redaction must not write an artifact"
    );
}

#[test]
fn form_without_a_mark_is_untouched() {
    let mut document = staged_form_document(FORM_PLACEMENT, |document| {
        stage_object(document, form_with_text("FORM-SECRET"))
    });
    let request = RedactionRequest {
        // A blank corner mark far from the form placement.
        marks: vec![RedactionMark::new(0, rect(2.0, 2.0, 12.0, 12.0))],
        keep_images: false,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut document, &request).expect("apply unmatched redaction");
    assert!(
        report.removed_runs.is_empty(),
        "an unrelated form was edited"
    );
    assert!(report.clean, "survivors: {:?}", report.surviving_spans);
    let bytes = document.save().expect("full rewrite");
    let reopened = Document::from_bytes(bytes.clone()).expect("redacted file reparses");
    let (_, data) = image_xobject(&reopened, "FormDo");
    assert!(
        String::from_utf8_lossy(&data).contains("FORM-SECRET"),
        "an unmarked form was rewritten"
    );
    assert!(
        bytes
            .windows(b"FORM-SECRET".len())
            .any(|window| window == b"FORM-SECRET"),
        "the untouched form stream should keep its original bytes"
    );
}

#[test]
fn unsupported_form_filter_fails_closed() {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Type".to_owned(), Object::Name("XObject".to_owned()));
    dictionary.insert("Subtype".to_owned(), Object::Name("Form".to_owned()));
    dictionary.insert(
        "BBox".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(120),
            Object::Integer(40),
        ]),
    );
    dictionary.insert("Filter".to_owned(), Object::Name("LZWDecode".to_owned()));
    let raw_form = Object::Stream {
        dictionary,
        data: vec![0u8; 8],
    };
    let mut document =
        staged_form_document(FORM_PLACEMENT, |document| stage_object(document, raw_form));
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, form_mark())],
        keep_images: false,
        ..RedactionRequest::default()
    };
    match apply_redaction(&mut document, &request) {
        Err(DocumentRedactionError::UnsupportedForm { detail, .. }) => {
            assert!(detail.contains("LZWDecode"), "unexpected detail: {detail}");
        }
        other => panic!("expected UnsupportedForm, got {other:?}"),
    }
}

#[test]
fn unresolvable_xobject_under_a_mark_fails_closed() {
    let mut document = document_with_xobject("MissingDo", None);
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        keep_images: false,
        ..RedactionRequest::default()
    };
    match apply_redaction(&mut document, &request) {
        Err(DocumentRedactionError::UnsupportedImage { detail, .. }) => {
            assert!(
                detail.contains("not present in the page resources"),
                "unexpected detail: {detail}"
            );
        }
        other => panic!("expected UnsupportedImage, got {other:?}"),
    }

    let mut opt_out = document_with_xobject("MissingDo", None);
    let request = RedactionRequest {
        marks: vec![RedactionMark::new(0, rect(305.0, 405.0, 325.0, 425.0))],
        keep_images: true,
        ..RedactionRequest::default()
    };
    let report = apply_redaction(&mut opt_out, &request).expect("opt-out succeeds");
    assert!(!report.clean, "unresolved XObject was not reported");
    assert!(
        report
            .surviving_spans
            .iter()
            .any(|span| span.text.contains("unresolved XObject MissingDo")),
        "survivor does not name the missing XObject: {:?}",
        report.surviving_spans
    );
}
