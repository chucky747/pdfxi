//! Real-world PDF regression test.
//!
//! The fixture in `fixtures/realworld/` was produced by Microsoft Excel 2019
//! (see its README). Unlike the generated corpus it uses a hybrid
//! cross-reference stream, an object stream, FlateDecode content streams, and
//! an embedded subset TrueType font.
//!
//! The read path is fully supported. `Document::save_incremental()` currently
//! declines cross-reference-stream sources with a typed error, so the
//! non-ignored test records that limitation and the ignored test holds the
//! round-trip assertions we need once the writer supports such sources.

use std::path::PathBuf;

use pdfxi_core::{Document, Error};

fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

#[test]
fn real_world_pdf_text_and_current_save_limitation() {
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

    match doc.save_incremental() {
        Err(Error::UnsupportedState(reason)) => assert_eq!(
            reason, "cross-reference streams are not supported",
            "the current limitation must stay a typed, explicit error"
        ),
        Ok(bytes) => panic!(
            "incremental save unexpectedly wrote {} bytes for an xref-stream source",
            bytes.len()
        ),
        Err(other) => panic!("incremental save returned an unexpected error: {other}"),
    }
}

#[test]
#[ignore = "pdfxi-core save_incremental rejects xref-stream sources (crates/pdfxi-core/src/lib.rs:925): Error::UnsupportedState(\"cross-reference streams are not supported\")"]
fn real_world_pdf_incremental_save_preserves_pages_and_text() {
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");
    let saved = original
        .save_incremental()
        .expect("incremental save must support xref-stream sources");

    let reopened = Document::from_bytes(saved).expect("incremental revision parses");
    assert_eq!(reopened.page_count().expect("page count"), 2);
    for index in 0..2 {
        assert_eq!(
            reopened.page_text(index).expect("reopened page text"),
            original.page_text(index).expect("original page text"),
            "page {index} text changed across the incremental save"
        );
    }
}
