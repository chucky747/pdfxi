//! Integration tests for PDFium-backed text extraction.
//!
//! Compiled only with `--features pdfium`. Requires a PDFium library
//! discoverable through `PDFIUM_PATH` or the standard per-user install
//! location. The fixtures are the checked-in `fixtures/text.pdf` ("PDFXI
//! golden fixture text") and `fixtures/minimal.pdf` ("Hello, PDFXI!").

#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_text::{CancellationToken, Progress, TextError, TextExtractor};

/// Absolute path to a file in the workspace `fixtures/` directory.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

/// Build a structurally valid one-page PDF whose content stream draws nothing.
///
/// Cross-reference offsets are computed from the emitted bytes so PDFium
/// accepts the document. This gives us a deterministic "page with no text"
/// without depending on another crate's fixture builder.
fn blank_pdf_bytes() -> Vec<u8> {
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R \
         /Resources << /Font << /F1 5 0 R >> >> >>"
            .to_owned(),
        "<< /Length 0 >>\nstream\n\nendstream".to_owned(),
        "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
    ];

    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in offsets {
        out.push_str(&format!("{:010} 00000 n \n", offset));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objects.len() + 1,
        xref
    ));
    out.into_bytes()
}

/// Assert every metric on a span is finite and geometrically well-formed.
fn assert_finite_span(span: &pdfxi_text::TextSpan) {
    assert!(span.x.is_finite(), "span x must be finite");
    assert!(span.y.is_finite(), "span y (baseline) must be finite");
    assert!(span.width.is_finite(), "span width must be finite");
    assert!(span.height.is_finite(), "span height must be finite");
    assert!(span.font_size.is_finite(), "span font size must be finite");
    assert!(
        span.direction_degrees.is_finite(),
        "span direction must be finite"
    );
    assert!(span.width >= 0.0, "span width must be non-negative");
    assert!(span.height >= 0.0, "span height must be non-negative");
}

#[test]
fn extracts_golden_marker_from_text_fixture() {
    let renderer = PdfiumRenderer::from_file(fixture("text.pdf")).expect("open text fixture");
    assert_eq!(TextExtractor::page_count(&renderer), Ok(1));

    let mut reports = Vec::new();
    let page = renderer
        .extract_page(0, &CancellationToken::new(), &mut |p| reports.push(p))
        .expect("extract page 0");

    assert_eq!(page.page(), 0);
    assert!(page.span_count() > 0, "a text page must have spans");
    assert!(!page.is_empty());
    assert!(
        page.text().contains("PDFXI golden fixture text"),
        "expected marker missing from {:?}",
        page.text()
    );
    for span in page.spans() {
        assert!(!span.is_empty(), "spans must carry text");
        assert_finite_span(span);
    }
    assert_eq!(
        page.spans()
            .iter()
            .map(|s| s.text.as_str())
            .collect::<String>(),
        page.text(),
        "page text is the verbatim concatenation of its spans"
    );
    assert_eq!(
        reports,
        vec![
            Progress::new(0, 3),
            Progress::new(1, 3),
            Progress::new(2, 3),
            Progress::new(3, 3),
        ],
        "extraction reports its stages"
    );
}

#[test]
fn extracts_text_from_minimal_fixture() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open minimal fixture");
    let page = renderer
        .extract_page(0, &CancellationToken::new(), &mut |_| {})
        .expect("extract page 0");

    assert!(page.span_count() > 0);
    assert!(
        page.text().contains("Hello, PDFXI!"),
        "expected minimal marker missing from {:?}",
        page.text()
    );
    for span in page.spans() {
        assert_finite_span(span);
    }
}

#[test]
fn page_without_text_returns_an_empty_page() {
    let renderer = PdfiumRenderer::from_bytes(blank_pdf_bytes()).expect("open blank PDF");
    assert_eq!(TextExtractor::page_count(&renderer), Ok(1));

    let page = renderer
        .extract_page(0, &CancellationToken::new(), &mut |_| {})
        .expect("extract blank page");

    assert!(page.is_empty(), "a blank page must be empty");
    assert_eq!(page.span_count(), 0);
    assert_eq!(page.text(), "");
}

#[test]
fn out_of_range_page_returns_structured_error() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");
    let result = renderer.extract_page(99, &CancellationToken::new(), &mut |_| {});
    assert_eq!(result, Err(TextError::PageOutOfRange(99)));
}

#[test]
fn cancellation_before_work_stops_extraction() {
    let renderer = PdfiumRenderer::from_file(fixture("text.pdf")).expect("open fixture");
    let token = CancellationToken::new();
    token.cancel();

    let mut reports = Vec::new();
    let result = renderer.extract_page(0, &token, &mut |p| reports.push(p));
    assert_eq!(result, Err(TextError::Cancelled));
    assert!(
        reports.is_empty(),
        "a cancelled extraction must not report progress"
    );
}

#[test]
fn malformed_document_returns_structured_error() {
    let renderer = PdfiumRenderer::from_bytes(b"not a PDF".to_vec()).expect_err("invalid bytes");
    // `from_bytes` already rejects malformed documents, so this renderer cannot
    // be constructed. The assertion documents that no panic escaped.
    let _ = renderer;
}
