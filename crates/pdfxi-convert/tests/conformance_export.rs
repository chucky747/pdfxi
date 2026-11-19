//! Integration tests for T-0093: profile-targeted normalized PDFs must remain
//! parseable by `pdfxi-core` and must preserve page content byte-for-byte at
//! the stream level. These tests never claim conformance; they check structure.

use pdfxi_convert::{
    ConformanceRequest, NormalizationOptions, create_from_text, export_conformance_pdf,
    looks_like_normalized_pdf,
};
use pdfxi_core::Document;

fn source_pdf() -> Vec<u8> {
    create_from_text(
        "Conformance export round trip.\nSecond line.",
        &pdfxi_convert::CreateOptions::new(),
    )
    .expect("test: create a source PDF")
}

#[test]
fn t0093_round_trip_a2b_through_core_preserves_pages_and_text() {
    let source = source_pdf();
    let result = export_conformance_pdf(
        &source,
        &ConformanceRequest::new("PDF/A-2b"),
        &NormalizationOptions::all(),
    )
    .expect("test: export PDF/A-2b");

    assert!(looks_like_normalized_pdf(&result.bytes));
    assert_eq!(result.report.target().label(), "PDF/A-2b");
    assert_eq!(result.report.output_version(), "1.7");
    assert!(!result.report.certification_claimed());

    let document = Document::from_bytes(result.bytes).expect("test: core parses normalized PDF");
    assert_eq!(document.page_count().expect("test: page count"), 1);
    assert_eq!(document.version(), "1.7");
    let text = document
        .page_text(0)
        .expect("test: extract first page text");
    assert!(text.contains("Conformance export round trip."));
    assert!(text.contains("Second line."));
}

#[test]
fn t0093_round_trip_x4_through_core_keeps_content_stream_bytes() {
    let source = source_pdf();
    let result = export_conformance_pdf(
        &source,
        &ConformanceRequest::new("PDF/X-4"),
        &NormalizationOptions::all(),
    )
    .expect("test: export PDF/X-4");

    let output = String::from_utf8_lossy(&result.bytes);
    assert!(output.contains("/S /GTS_PDFX"));
    assert!(output.contains("/Type /OutputIntent"));
    let document = Document::from_bytes(result.bytes).expect("test: core parses normalized PDF");
    assert_eq!(document.page_count().expect("test: page count"), 1);
    let text = document
        .page_text(0)
        .expect("test: extract first page text");
    assert!(text.contains("Conformance export round trip."));
}

#[test]
fn t0093_round_trip_e1_does_not_invent_output_intents() {
    let source = source_pdf();
    let result = export_conformance_pdf(
        &source,
        &ConformanceRequest::new("PDF/E-1"),
        &NormalizationOptions::all(),
    )
    .expect("test: export PDF/E-1");

    let output = String::from_utf8_lossy(&result.bytes);
    assert!(!output.contains("/OutputIntents"));
    let document = Document::from_bytes(result.bytes).expect("test: core parses normalized PDF");
    assert_eq!(document.page_count().expect("test: page count"), 1);
    assert_eq!(document.version(), "1.6");
}
