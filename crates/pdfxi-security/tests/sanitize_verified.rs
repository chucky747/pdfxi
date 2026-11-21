//! T-0257: document sanitize and independent verification.
//!
//! The real-world test wraps `fixtures/realworld/microsoft-excel-2019-sample.pdf`
//! with identifying `/Info`, sanitizes it, saves a full rewrite, reopens it, and
//! proves unrelated text survives. Synthetic documents add attachments, XMP,
//! JavaScript actions, optional content, thumbnails and traversal names.

use std::collections::BTreeMap;
use std::path::PathBuf;

use pdfxi_core::{Document, Object, ObjectId};
use pdfxi_security::{
    SanitizeCategory, SanitizeRequest, SanitizeSurvivorKind, SecurityError,
    is_safe_attachment_name, sanitize_document_full, sanitize_inventory, verify_sanitized,
};

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .to_path_buf()
}

fn excel_fixture() -> Vec<u8> {
    let path = workspace_root()
        .join("fixtures")
        .join("realworld")
        .join("microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

fn artifact_dir() -> PathBuf {
    let directory = workspace_root()
        .join("target")
        .join("lanes")
        .join("security6")
        .join("t0257-out");
    std::fs::create_dir_all(&directory).expect("create artifact directory");
    directory
}

/// Assemble a classic-cross-reference PDF from object bodies.
fn build_pdf(objects: &[Vec<u8>], root: u32, info: u32) -> Vec<u8> {
    let mut out = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
        out.extend_from_slice(body);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref = out.len();
    out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {} /Root {} 0 R /Info {} 0 R /ID [<00112233445566778899AABBCCDDEEFF> <00112233445566778899AABBCCDDEEFF>] >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1,
            root,
            info
        )
        .as_bytes(),
    );
    out
}

fn stream(dictionary: &str, data: &[u8]) -> Vec<u8> {
    let mut out = format!("<< {dictionary} /Length {} >>\nstream\n", data.len()).into_bytes();
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream");
    out
}

/// Object numbers (1-based) deliberately map to the documentation below.
fn hidden_fixture(optional_content: &str, content: &[u8]) -> Vec<u8> {
    build_pdf(
        &[
            format!(
                "<< /Type /Catalog /Pages 2 0 R /Names 3 0 R /Metadata 4 0 R /OpenAction 5 0 R /OCProperties {optional_content} /AA << /WC 5 0 R >> /Collection << /View /H >> >>"
            )
            .into_bytes(),
            b"<< /Type /Pages /Kids [7 0 R] /Count 1 >>".to_vec(),
            b"<< /EmbeddedFiles 8 0 R /JavaScript 9 0 R >>".to_vec(),
            stream(
                "/Type /Metadata /Subtype /XML",
                b"<x:xmpmeta><rdf:RDF><rdf:Description>SECRET-XMP</rdf:Description></rdf:RDF></x:xmpmeta>",
            ),
            b"<< /S /JavaScript /JS (app.alert\\('SECRET-JS'\\)) >>".to_vec(),
            b"<< /S /JavaScript /JS (app.beep\\(\\)) >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 18 0 R >> >> /Contents 11 0 R /Thumb 12 0 R /AA << /O 5 0 R >> /Annots [14 0 R] >>".to_vec(),
            b"<< /Names [(../../etc/passwd) 13 0 R] >>".to_vec(),
            b"<< /Names [(init) 5 0 R] >>".to_vec(),
            b"<< /OCGs [15 0 R] /D << /Order [15 0 R] >> >>".to_vec(),
            stream("", content),
            stream(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8",
                &[0],
            ),
            b"<< /Type /Filespec /F (../../etc/passwd) /UF (../../etc/passwd) /EF << /F 16 0 R >> >>".to_vec(),
            b"<< /Type /Annot /Subtype /Text /Rect [72 700 92 720] /Contents (note) /A 6 0 R /AA << /E 6 0 R >> >>".to_vec(),
            b"<< /Type /OCG /Name (Layer) >>".to_vec(),
            stream("/Type /EmbeddedFile", b"SECRET-ATTACHMENT-BYTES"),
            b"<< /Title (SECRET-INFO-TITLE) /Author (SECRET-INFO-AUTHOR) /CustomKey (SECRET-INFO-CUSTOM) >>"
                .to_vec(),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        ],
        1,
        17,
    )
}

fn visible_fixture() -> Vec<u8> {
    hidden_fixture(
        "10 0 R",
        b"BT /F1 12 Tf 72 720 Td (Visible unrelated text) Tj ET",
    )
}

fn hidden_layer_fixture() -> Vec<u8> {
    // Replace the visible `/D` dictionary with a default-off layer set.
    build_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R /OCProperties 3 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [4 0 R] /Count 1 >>".to_vec(),
            b"<< /OCGs [5 0 R] /D << /BaseState /OFF /Order [5 0 R] >> >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 6 0 R >>".to_vec(),
            b"<< /Type /OCG /Name (Hidden) >>".to_vec(),
            stream("", b"BT (Hidden layer text) Tj ET"),
            b"<< >>".to_vec(),
        ],
        1,
        7,
    )
}

/// A small XMP packet whose `dc:title` carries a caller secret as literal
/// UTF-8, so an unremoved packet is visible to the raw-byte, stream-byte and
/// category checks of the independent verifier.
fn xmp_with_title(secret: &str) -> Vec<u8> {
    format!(
        "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\
         <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\
         <rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\
         <dc:title>{secret}</dc:title>\
         </rdf:Description></rdf:RDF></x:xmpmeta>"
    )
    .into_bytes()
}

/// Classic document whose only XMP lives in a page and an image XObject, never
/// in the catalog. Object numbers: 1 catalog, 2 pages, 3 page, 4 content,
/// 5 image XObject, 6 font, 7 page XMP, 8 image XMP, 9 Info.
fn non_catalog_xmp_fixture() -> Vec<u8> {
    build_pdf(
        &[
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << /Font << /F1 6 0 R >> /XObject << /Im0 5 0 R >> >> /Contents 4 0 R /Metadata 7 0 R >>".to_vec(),
            stream("", b"BT /F1 12 Tf 72 720 Td (Visible unrelated text) Tj ET"),
            stream(
                "/Type /XObject /Subtype /Image /Width 1 /Height 1 /ColorSpace /DeviceGray /BitsPerComponent 8 /Metadata 8 0 R",
                &[0],
            ),
            b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
            stream("/Type /Metadata /Subtype /XML", &xmp_with_title("SECRET-XMP-PAGE")),
            stream("/Type /Metadata /Subtype /XML", &xmp_with_title("SECRET-XMP-IMAGE")),
            b"<< /Title (SECRET-INFO-TITLE) /Author (SECRET-INFO-AUTHOR) >>".to_vec(),
        ],
        1,
        9,
    )
}

/// Stage a page-level `/Metadata` XMP stream carrying `secret` on a real
/// `Document` and return its object number. The next free number is just past
/// the trailer `/Size`, so the classic xref table stays compact.
fn stage_page_xmp(document: &mut Document, secret: &str) -> ObjectId {
    let page_id = document.pages().expect("pages").first().expect("a page").id;
    let mut number = document.trailer_size().ok().flatten().unwrap_or(1).max(2);
    while document.object(ObjectId::new(number, 0)).is_ok() {
        number += 1;
    }
    let xmp_id = ObjectId::new(number, 0);

    let mut page = document.object(page_id).expect("load page object");
    match &mut page {
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            dictionary.insert("Metadata".to_owned(), Object::Reference(xmp_id));
        }
        other => panic!("page object is not a dictionary: {other:?}"),
    }
    document
        .set_object(page_id, page)
        .expect("stage page /Metadata");

    let stream = Object::Stream {
        dictionary: BTreeMap::from([
            ("Type".to_owned(), Object::Name("Metadata".to_owned())),
            ("Subtype".to_owned(), Object::Name("XML".to_owned())),
        ]),
        data: xmp_with_title(secret),
    };
    document
        .set_object(xmp_id, stream)
        .expect("stage XMP stream");
    xmp_id
}

fn survival_probe(text: &str) -> String {
    text.split_whitespace()
        .find(|word| {
            word.len() >= 4
                && word.chars().all(|character| character.is_alphanumeric())
                && !word.to_ascii_lowercase().contains("secret")
        })
        .unwrap_or("Report")
        .to_owned()
}

#[test]
fn real_fixture_is_sanitized_saved_and_reopens_with_text() {
    let mut source = Document::from_bytes(excel_fixture()).expect("Excel fixture parses");
    let source_text = source.page_text(0).expect("source text");
    let probe = survival_probe(&source_text);

    let mut info = source.info().expect("read info").unwrap_or_default();
    info.title = Some("SECRET-INFO-FIXTURE".to_owned());
    info.author = Some("SECRET-AUTHOR-FIXTURE".to_owned());
    source.set_info(&info).expect("stage identifying info");
    // The real fixture already carries catalog XMP; add an XMP packet whose only
    // reference is from a page dictionary, with a secret title in literal UTF-8.
    stage_page_xmp(&mut source, "SECRET-XMP-TITLE-FIXTURE");

    let before = sanitize_inventory(&source).expect("source inventory");
    assert!(before.contains(&SanitizeCategory::DocumentInfo));
    assert!(before.contains(&SanitizeCategory::XmpMetadata));

    let sanitized = sanitize_document_full(&source, &SanitizeRequest::all())
        .expect("sanitize the real fixture");
    let report = sanitized.report();
    assert!(report.is_clean(), "residuals: {:?}", report.residuals());
    assert!(report.before.contains(&SanitizeCategory::DocumentInfo));
    assert!(report.before.contains(&SanitizeCategory::XmpMetadata));
    assert!(!report.after.contains(&SanitizeCategory::DocumentInfo));
    assert!(!report.after.contains(&SanitizeCategory::XmpMetadata));

    let artifact = artifact_dir().join("sanitized.pdf");
    std::fs::write(&artifact, sanitized.bytes()).expect("write sanitized artifact");

    let reopened = Document::from_bytes(sanitized.bytes().to_vec()).expect("sanitized file parses");
    let reopened_info = reopened.info().expect("reopened info");
    assert!(
        reopened_info.is_none_or(|info| {
            info.title.is_none()
                && info.author.is_none()
                && info.subject.is_none()
                && info.keywords.is_none()
                && info.creator.is_none()
                && info.producer.is_none()
                && info.creation_date.is_none()
                && info.modification_date.is_none()
        }),
        "identifying Info survived"
    );
    let after = sanitize_inventory(&reopened).expect("reopened inventory");
    assert!(!after.contains(&SanitizeCategory::XmpMetadata));
    assert!(!after.contains(&SanitizeCategory::DocumentInfo));

    let text = reopened.page_text(0).expect("reopened text");
    assert!(
        text.contains(&probe),
        "unrelated text {probe:?} was lost: {text}"
    );
    assert!(
        !text.to_ascii_lowercase().contains("secret-info-fixture"),
        "identifying info leaked into page text"
    );

    let verification = verify_sanitized(
        sanitized.bytes(),
        &[
            "SECRET-INFO-FIXTURE",
            "SECRET-AUTHOR-FIXTURE",
            "SECRET-XMP-TITLE-FIXTURE",
        ],
    )
    .expect("independent verification");
    assert!(
        verification.clean,
        "verification survivors: {:?}",
        verification.survivors
    );
    assert_eq!(verification.secrets_checked, 3);
    assert_eq!(
        verification.pages_checked,
        reopened.page_count().expect("count")
    );
    for secret in [
        "SECRET-INFO-FIXTURE",
        "SECRET-AUTHOR-FIXTURE",
        "SECRET-XMP-TITLE-FIXTURE",
    ] {
        assert!(
            !sanitized
                .bytes()
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "removed secret survived in saved bytes: {secret}"
        );
    }
    assert!(artifact.exists());
}

#[test]
fn synthetic_document_loses_all_categories_and_verifies_clean() {
    let source = Document::from_bytes(visible_fixture()).expect("synthetic fixture parses");
    let before = sanitize_inventory(&source).expect("source inventory");
    for category in SanitizeCategory::ALL {
        assert!(before.contains(&category), "missing before: {category}");
    }

    let sanitized =
        sanitize_document_full(&source, &SanitizeRequest::all()).expect("sanitize synthetic");
    let report = sanitized.report();
    assert!(report.is_clean(), "residuals: {:?}", report.residuals());
    assert!(
        report.unsafe_attachment_names >= 1,
        "the traversal attachment name must be counted"
    );
    let removed: Vec<SanitizeCategory> =
        report.removed.iter().map(|item| item.category()).collect();
    for category in SanitizeCategory::ALL {
        assert!(
            removed.contains(&category),
            "category was not removed: {category}"
        );
    }

    let reopened = Document::from_bytes(sanitized.bytes().to_vec()).expect("output parses");
    let after = sanitize_inventory(&reopened).expect("output inventory");
    assert!(after.is_empty(), "categories still present: {after:?}");
    assert_eq!(
        reopened.page_text(0).expect("page text"),
        "Visible unrelated text"
    );

    let secrets = [
        "SECRET-XMP",
        "SECRET-JS",
        "SECRET-ATTACHMENT-BYTES",
        "SECRET-INFO-TITLE",
        "SECRET-INFO-AUTHOR",
        "SECRET-INFO-CUSTOM",
    ];
    for secret in secrets {
        assert!(
            !sanitized
                .bytes()
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "removed bytes survived for {secret}"
        );
    }
    let verification =
        verify_sanitized(sanitized.bytes(), &secrets).expect("independent verification");
    assert!(
        verification.clean,
        "verification survivors: {:?}",
        verification.survivors
    );
}

#[test]
fn xmp_in_image_xobject_and_page_is_inventoried_and_removed() {
    let source = Document::from_bytes(non_catalog_xmp_fixture()).expect("fixture parses");
    let before = sanitize_inventory(&source).expect("source inventory");
    // The catalog has no /Metadata. Without cross-dictionary detection this
    // assertion fails: XMP can only be found on the page and image XObject.
    assert!(
        before.contains(&SanitizeCategory::XmpMetadata),
        "non-catalog XMP was not detected: {before:?}"
    );

    let sanitized = sanitize_document_full(&source, &SanitizeRequest::all()).expect("sanitize");
    let report = sanitized.report();
    assert!(report.is_clean(), "residuals: {:?}", report.residuals());
    assert!(report.before.contains(&SanitizeCategory::XmpMetadata));
    assert!(!report.after.contains(&SanitizeCategory::XmpMetadata));
    let xmp_removals = report
        .removed
        .iter()
        .filter(|item| item.category() == SanitizeCategory::XmpMetadata)
        .count();
    assert!(
        xmp_removals >= 2,
        "expected page and image XObject removals, got {xmp_removals}"
    );

    let reopened = Document::from_bytes(sanitized.bytes().to_vec()).expect("output parses");
    let after = sanitize_inventory(&reopened).expect("output inventory");
    assert!(
        !after.contains(&SanitizeCategory::XmpMetadata),
        "XMP remained after sanitize: {after:?}"
    );
    assert_eq!(
        reopened.page_text(0).expect("page text"),
        "Visible unrelated text",
        "unrelated text must survive"
    );

    let secrets = ["SECRET-XMP-PAGE", "SECRET-XMP-IMAGE", "SECRET-INFO-TITLE"];
    for secret in secrets {
        assert!(
            !sanitized
                .bytes()
                .windows(secret.len())
                .any(|window| window == secret.as_bytes()),
            "removed secret survived in saved bytes: {secret}"
        );
    }
    let verification = verify_sanitized(sanitized.bytes(), &secrets).expect("verification");
    assert!(
        verification.clean,
        "verification survivors: {:?}",
        verification.survivors
    );
}

#[test]
fn xmp_outside_selected_categories_keeps_inventory_and_verification_dirty() {
    let source = Document::from_bytes(non_catalog_xmp_fixture()).expect("fixture parses");
    // Leave XMP unselected: a caller that only clears /Info must still see the
    // unremoved image/page XMP reported as present, not as a false clean.
    let request = SanitizeRequest::new([SanitizeCategory::DocumentInfo]);
    let sanitized = sanitize_document_full(&source, &request).expect("sanitize");
    assert!(
        sanitized.report().is_clean(),
        "residuals are scoped to the request"
    );

    let reopened = sanitized.document().expect("reparse sanitized bytes");
    let after = sanitize_inventory(&reopened).expect("output inventory");
    assert!(
        after.contains(&SanitizeCategory::XmpMetadata),
        "unselected XMP must remain detectable: {after:?}"
    );

    let verification =
        verify_sanitized(sanitized.bytes(), &["SECRET-XMP-PAGE", "SECRET-XMP-IMAGE"])
            .expect("verification");
    assert!(
        !verification.clean,
        "the verifier must not be always-true for unremoved XMP"
    );
    assert!(
        verification
            .survivors
            .iter()
            .any(|survivor| survivor.kind == SanitizeSurvivorKind::XmpMetadata),
        "expected an XmpMetadata survivor, got {:?}",
        verification.survivors
    );
}

#[test]
fn secret_outside_the_sanitized_categories_makes_verification_dirty() {
    let source = Document::from_bytes(hidden_fixture(
        "10 0 R",
        b"BT /F1 12 Tf 72 720 Td (Visible text SECRET-OUTSIDE-77) Tj ET",
    ))
    .expect("fixture parses");
    let sanitized = sanitize_document_full(&source, &SanitizeRequest::all()).expect("sanitize");

    let verification = verify_sanitized(sanitized.bytes(), &["SECRET-OUTSIDE-77"])
        .expect("independent verification");
    assert!(
        !verification.clean,
        "the verifier must not always report clean"
    );
    assert!(verification.survivors.iter().any(|survivor| {
        survivor.kind == SanitizeSurvivorKind::ExtractedText
            && survivor.page == Some(0)
            && survivor.secret_index == Some(0)
    }));
}

#[test]
fn default_hidden_optional_content_fails_closed() {
    let source = Document::from_bytes(hidden_layer_fixture()).expect("fixture parses");
    let error = sanitize_document_full(&source, &SanitizeRequest::all())
        .expect_err("hidden layers must not be revealed");
    assert_eq!(error, SecurityError::UnsafeOptionalContent);
}

#[test]
fn attachment_name_validator_rejects_traversal_and_absolute_paths() {
    for safe in ["report.pdf", "budget 2026.xlsx", "résumé.txt"] {
        assert!(is_safe_attachment_name(safe), "safe name rejected: {safe}");
    }
    for unsafe_name in [
        "",
        ".",
        "..",
        "../secret.txt",
        "..\\secret.txt",
        "/etc/passwd",
        "C:\\Windows\\win.ini",
        "file.txt:stream",
        " padded.txt",
        "tab\tname",
    ] {
        assert!(
            !is_safe_attachment_name(unsafe_name),
            "unsafe name accepted: {unsafe_name:?}"
        );
    }
}

#[test]
fn category_identifiers_round_trip() {
    for category in SanitizeCategory::ALL {
        assert_eq!(SanitizeCategory::from_id(category.as_id()), Some(category));
    }
    assert_eq!(SanitizeCategory::from_id("unknown"), None);
    assert_eq!(
        SanitizeCategory::OptionalContent.to_string(),
        "optional_content"
    );
    let request = SanitizeRequest::new([SanitizeCategory::Thumbnails]);
    assert_eq!(request.len(), 1);
    assert!(request.contains(SanitizeCategory::Thumbnails));
    assert!(!request.contains(SanitizeCategory::Attachments));
}
