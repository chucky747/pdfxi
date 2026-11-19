//! Integration tests for the optional native PDFium backend.
//!
//! These tests are compiled only with `--features pdfium`; default builds keep
//! the crate free of native dependencies. They use the checked-in
//! `fixtures/minimal.pdf` and require a PDFium library discoverable through
//! `PDFIUM_PATH` or the standard per-user install location.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{CancellationToken, PageIndex, RenderError, Renderer};

/// Absolute path to a file in the workspace `fixtures/` directory.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn renders_minimal_pdf_page_zero_at_scale_one() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");
    assert_eq!(renderer.page_count(), Ok(1));

    let mut reports = Vec::new();
    let page = renderer
        .render_page(
            PageIndex::new(0),
            1.0,
            &CancellationToken::new(),
            &mut |progress| reports.push(progress),
        )
        .expect("render page 0");

    assert_eq!(page.page(), PageIndex::new(0));
    assert!(page.width() > 0, "width must be non-zero");
    assert!(page.height() > 0, "height must be non-zero");
    assert!(!page.is_empty());

    let expected_len = page.width() as usize * page.height() as usize * 4;
    assert_eq!(
        page.pixels().len(),
        expected_len,
        "RGBA buffer must be width * height * 4"
    );

    let first = page.pixels()[0];
    assert!(
        page.pixels().iter().any(|byte| *byte != first),
        "raster must contain more than one colour"
    );
    assert!(!reports.is_empty(), "progress must be reported");
    assert!(
        reports.last().is_some_and(|report| report.is_complete()),
        "the final progress report must be complete"
    );
}

#[test]
fn invalid_document_returns_structured_error() {
    let result = PdfiumRenderer::from_bytes(b"this is not a PDF document".to_vec());
    assert!(
        matches!(result, Err(RenderError::Backend(_))),
        "a malformed document must produce a Backend error, not a panic"
    );
}

#[test]
fn missing_file_returns_structured_error() {
    let result = PdfiumRenderer::from_file(fixture("this-file-does-not-exist.pdf"));
    assert!(matches!(result, Err(RenderError::Backend(_))));
}

#[test]
fn invalid_inputs_are_rejected_without_panicking() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");

    for scale in [0.0, -1.0, f32::INFINITY, f32::NAN] {
        let result = renderer.render_page(
            PageIndex::new(0),
            scale,
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert!(
            matches!(result, Err(RenderError::InvalidScale(_))),
            "scale {scale} must be rejected"
        );
    }

    let result = renderer.render_page(
        PageIndex::new(99),
        1.0,
        &CancellationToken::new(),
        &mut |_| {},
    );
    assert_eq!(result, Err(RenderError::PageOutOfRange(99)));

    let token = CancellationToken::new();
    token.cancel();
    let result = renderer.render_page(PageIndex::new(0), 1.0, &token, &mut |_| {});
    assert_eq!(result, Err(RenderError::Cancelled));
}
