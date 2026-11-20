//! Real-world PDF regression test.
//!
//! The fixture in `fixtures/realworld/` was produced by Microsoft Excel 2019
//! (see its README). Unlike the generated corpus it uses a hybrid
//! cross-reference stream, an object stream, FlateDecode content streams, and
//! an embedded subset TrueType font.
//!
//! The read and incremental-save paths are supported: `save_incremental()`
//! appends a revision whose cross-reference section matches the source's final
//! section (classic here, because the Excel hybrid file ends in a classic table
//! with `/XRefStm`), leaving the original bytes as an exact prefix.

use std::path::PathBuf;

use pdfxi_core::{Document, XrefSectionKind};

fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn real_world_pdf_text_is_extractable() {
    let doc = Document::from_bytes(fixture()).expect("real-world fixture parses");

    assert_eq!(doc.page_count().expect("page count"), 2);
    assert!(
        doc.xref_report().expect("xref report").uses_xref_stream,
        "the fixture must keep its cross-reference stream"
    );

    let first = doc.page_text(0).expect("page 1 text");
    for word in [
        "PDFXI",
        "Real-World",
        "orchestra",
        "quarantine",
        "lighthouse",
        "cinnamon",
    ] {
        assert!(
            first.contains(word),
            "page 1 is missing {word:?}: {first:?}"
        );
    }
    let second = doc.page_text(1).expect("page 2 text");
    for word in ["Second page marker", "meadow", "harmonic"] {
        assert!(
            second.contains(word),
            "page 2 is missing {word:?}: {second:?}"
        );
    }
}

#[test]
fn real_world_pdf_incremental_save_preserves_pages_and_text() {
    let original_bytes = fixture();
    let original = Document::from_bytes(original_bytes.clone()).expect("real-world fixture parses");
    let saved = original
        .save_incremental()
        .expect("incremental save must support xref-stream sources");
    assert!(
        saved.starts_with(&original_bytes),
        "an incremental save must preserve the original bytes as an exact prefix"
    );

    let reopened = Document::from_bytes(saved.clone()).expect("incremental revision parses");
    let report = reopened.xref_report().expect("saved xref report");
    assert_eq!(
        report.sections[0].kind,
        XrefSectionKind::Classic,
        "a hybrid source whose final section is a classic table must keep the classic representation"
    );
    let appended = &saved[original_bytes.len()..];
    assert!(
        !appended.windows(7).any(|window| window == b"/XRefStm"),
        "the appended classic section must not advertise a hybrid companion"
    );
    assert_eq!(reopened.page_count().expect("page count"), 2);
    for index in 0..2 {
        assert_eq!(
            reopened.page_text(index).expect("reopened page text"),
            original.page_text(index).expect("original page text"),
            "page {index} text changed across the incremental save"
        );
    }
}
