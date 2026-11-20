//! Real-world PDF regression tests.
//!
//! The fixture in `fixtures/realworld/` was produced by Microsoft Excel 2019
//! (see its README). Unlike the generated corpus it uses a hybrid
//! cross-reference stream, an object stream, FlateDecode content streams, and
//! an embedded subset TrueType font.
//!
//! Both write paths are supported: `save_incremental()` appends a revision whose
//! cross-reference section matches the source's final section, while `save()`,
//! `optimize()` and `apply_page_plan()` perform a full rewrite that expands the
//! object stream into standalone indirect objects and emits a classic table.

use std::path::PathBuf;

use pdfxi_core::{CancellationToken, Document, OptimizeOptions, PagePlan, XrefSectionKind};

fn fixture() -> Vec<u8> {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
    std::fs::read(&path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()))
}

/// Assert that a rewritten file is a single-revision classic-table PDF.
fn assert_classic_table(bytes: &[u8]) -> Document {
    let text = String::from_utf8_lossy(bytes);
    assert!(
        text.contains("\nxref\n"),
        "the rewrite must emit a classic xref table"
    );
    assert!(
        !text.contains("/Type /XRef"),
        "the old cross-reference stream must not be copied into the output"
    );
    assert!(
        !text.contains("/ObjStm"),
        "the old object stream must not be copied into the output"
    );
    let reparsed = Document::from_bytes(bytes.to_vec()).expect("rewritten PDF parses");
    let report = reparsed.xref_report().expect("rewritten xref report");
    assert_eq!(
        report.sections[0].kind,
        XrefSectionKind::Classic,
        "the first rewritten section must be a classic table"
    );
    assert!(
        !report.uses_xref_stream,
        "no rewritten section may be an xref stream"
    );
    reparsed
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

#[test]
fn full_save_rewrites_hybrid_xref_stream_source_with_classic_table() {
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");
    let saved = original
        .save()
        .expect("a full rewrite must support hybrid xref-stream sources");
    let reopened = assert_classic_table(&saved);
    assert_eq!(reopened.page_count().expect("page count"), 2);
    for index in 0..2 {
        assert_eq!(
            reopened.page_text(index).expect("rewritten page text"),
            original.page_text(index).expect("original page text"),
            "page {index} text changed across the full save"
        );
    }
}

#[test]
fn full_save_page_plan_can_reverse_hybrid_xref_stream_source() {
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");
    let original_first = original.page_text(0).expect("original page 1 text");
    let original_second = original.page_text(1).expect("original page 2 text");
    let plan = PagePlan::reorder(&[1, 0]).expect("two-page reversal plan");
    let saved = original
        .apply_page_plan(&plan)
        .expect("page plans must support hybrid xref-stream sources");
    let reopened = assert_classic_table(&saved);
    assert_eq!(reopened.page_count().expect("page count"), 2);
    assert_eq!(
        reopened.page_text(0).expect("new page 1 text"),
        original_second,
        "new page 1 must carry the original page 2 content"
    );
    assert_eq!(
        reopened.page_text(1).expect("new page 2 text"),
        original_first,
        "new page 2 must carry the original page 1 content"
    );
}

#[test]
fn full_save_page_plan_can_delete_first_hybrid_page() {
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");
    let original_second = original.page_text(1).expect("original page 2 text");
    let plan = PagePlan::extract(&[1]).expect("single-page selection");
    let saved = original
        .apply_page_plan(&plan)
        .expect("page deletion must support hybrid xref-stream sources");
    let reopened = assert_classic_table(&saved);
    assert_eq!(reopened.page_count().expect("page count"), 1);
    assert_eq!(
        reopened.page_text(0).expect("remaining page text"),
        original_second,
        "the remaining page must carry the original page 2 content"
    );
}

#[test]
fn optimize_rewrites_hybrid_xref_stream_source() {
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");
    let optimized = original
        .optimize(&OptimizeOptions::default(), &CancellationToken::new())
        .expect("optimize must support hybrid xref-stream sources");
    let reopened = assert_classic_table(&optimized.bytes);
    assert_eq!(reopened.page_count().expect("page count"), 2);
    for index in 0..2 {
        assert_eq!(
            reopened.page_text(index).expect("optimized page text"),
            original.page_text(index).expect("original page text"),
            "page {index} text changed across optimization"
        );
    }
    assert!(
        optimized.report.objects_before >= 2,
        "the optimizer report must count the source objects"
    );
}

#[test]
fn full_save_rewrites_pure_xref_stream_and_object_stream_source() {
    // `compressed_pdf` writes a pure (non-hybrid) `/Type /XRef` stream whose
    // page and content objects live in an `/ObjStm`.
    let source = pdfxi_testkit::hostile::compressed_pdf("2", "16");
    let original = Document::from_bytes(source).expect("pure xref-stream fixture parses");
    assert!(
        original
            .xref_report()
            .expect("source xref report")
            .uses_xref_stream,
        "the fixture must be a pure xref-stream source"
    );
    assert_eq!(
        original.page_text(0).expect("original page text"),
        "From ObjStm"
    );
    let saved = original
        .save()
        .expect("full rewrite of a pure stream source");
    let reopened = assert_classic_table(&saved);
    assert_eq!(reopened.page_count().expect("page count"), 1);
    assert_eq!(
        reopened.page_text(0).expect("rewritten page text"),
        "From ObjStm",
        "an /ObjStm member must survive the full rewrite"
    );
}

#[test]
#[ignore = "writes rewritten outputs for an external PDFium check"]
fn write_pdfium_check_outputs() {
    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/core/t0213-out");
    std::fs::create_dir_all(&directory).expect("create output directory");
    let original = Document::from_bytes(fixture()).expect("real-world fixture parses");

    let saved = original.save().expect("full save");
    std::fs::write(directory.join("save.pdf"), saved).expect("write save.pdf");

    let reversed = original
        .apply_page_plan(&PagePlan::reorder(&[1, 0]).expect("reversal plan"))
        .expect("reversed page plan");
    std::fs::write(directory.join("reversed.pdf"), reversed).expect("write reversed.pdf");

    let deleted = original
        .apply_page_plan(&PagePlan::extract(&[1]).expect("deletion plan"))
        .expect("deleted page plan");
    std::fs::write(directory.join("deleted.pdf"), deleted).expect("write deleted.pdf");
}
