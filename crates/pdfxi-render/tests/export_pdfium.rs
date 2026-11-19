//! Integration test for page-image export with the native PDFium backend.
//!
//! Compiled only with `--features pdfium`. It uses the checked-in
//! `fixtures/minimal.pdf` and requires a PDFium library discoverable through
//! `PDFIUM_PATH` or the standard per-user install location.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{
    AlphaPolicy, CancellationToken, ColorMode, ExportOptions, ImageFormat, Renderer, export_pages,
};

/// Absolute path to a file in the workspace `fixtures/` directory.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn export_minimal_pdf_to_png_jpeg_and_tiff() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");
    assert_eq!(renderer.page_count(), Ok(1));

    // PNG: verify the file signature and a non-trivial byte length.
    let png = export_pages(
        &renderer,
        &ExportOptions {
            format: ImageFormat::Png,
            dpi: 72,
            ..ExportOptions::default()
        },
        &CancellationToken::new(),
        &mut |_| {},
    )
    .expect("PNG export");
    assert_eq!(png.len(), 1);
    assert_eq!(&png[0].bytes[..4], &[0x89, b'P', b'N', b'G']);
    assert!(png[0].width > 0 && png[0].height > 0);
    assert!(png[0].bytes.len() > 64);

    // JPEG: verify SOI/EOI markers (alpha must be flattened).
    let jpeg = export_pages(
        &renderer,
        &ExportOptions {
            format: ImageFormat::Jpeg,
            color: ColorMode::Rgb,
            alpha: AlphaPolicy::Flatten {
                background: [255, 255, 255],
            },
            dpi: 72,
            quality: 75,
            ..ExportOptions::default()
        },
        &CancellationToken::new(),
        &mut |_| {},
    )
    .expect("JPEG export");
    assert_eq!(&jpeg[0].bytes[..2], &[0xFF, 0xD8]);
    assert_eq!(&jpeg[0].bytes[jpeg[0].bytes.len() - 2..], &[0xFF, 0xD9]);

    // TIFF: verify the little-endian header and a multi-page-capable batch.
    let tiff = export_pages(
        &renderer,
        &ExportOptions {
            format: ImageFormat::Tiff,
            dpi: 72,
            ..ExportOptions::default()
        },
        &CancellationToken::new(),
        &mut |_| {},
    )
    .expect("TIFF export");
    assert_eq!(&tiff[0].bytes[..4], &[b'I', b'I', 42, 0]);
    assert_eq!(tiff[0].page_count, 1);
}
