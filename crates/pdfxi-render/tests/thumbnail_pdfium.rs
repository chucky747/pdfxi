//! Integration test for thumbnail generation with the native PDFium backend.
//!
//! Compiled only with `--features pdfium`. It uses the checked-in
//! `fixtures/minimal.pdf` and requires a PDFium library discoverable through
//! `PDFIUM_PATH` or the standard per-user install location.

#![cfg(feature = "pdfium")]

use std::path::PathBuf;

use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::thumbnail::{ThumbnailGenerator, ThumbnailSpec};
use pdfxi_render::{CancellationToken, PageIndex, Renderer};

/// Absolute path to a file in the workspace `fixtures/` directory.
fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures")
        .join(name)
}

#[test]
fn thumbnail_of_minimal_pdf_is_nonzero_sized_and_non_uniform() {
    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");
    assert_eq!(renderer.page_count(), Ok(1));

    let spec = ThumbnailSpec::new(128, 128);
    let thumbnail = ThumbnailGenerator
        .generate(
            &renderer,
            PageIndex::new(0),
            &spec,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("generate thumbnail");

    assert_eq!(thumbnail.page(), PageIndex::new(0));
    assert!(thumbnail.width() > 0, "width must be non-zero");
    assert!(thumbnail.height() > 0, "height must be non-zero");
    assert!(thumbnail.width() <= 128 && thumbnail.height() <= 128);
    assert!(!thumbnail.is_empty());
    assert_eq!(
        thumbnail.pixels().len(),
        thumbnail.width() as usize * thumbnail.height() as usize * 4
    );

    let first = thumbnail.pixels()[0];
    assert!(
        thumbnail.pixels().iter().any(|byte| *byte != first),
        "thumbnail must contain more than one colour"
    );
}

#[test]
fn cached_pdfium_thumbnail_matches_a_fresh_generation() {
    use pdfxi_render::thumbnail::ThumbnailCache;

    let renderer = PdfiumRenderer::from_file(fixture("minimal.pdf")).expect("open fixture");
    let spec = ThumbnailSpec::new(96, 64).with_scale(2.0);
    let mut cache = ThumbnailCache::unbounded();
    let first = cache
        .generate(
            &renderer,
            PageIndex::new(0),
            &spec,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("first thumbnail");
    let second = cache
        .generate(
            &renderer,
            PageIndex::new(0),
            &spec,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("cached thumbnail");
    assert_eq!(first, second);
    assert_eq!(cache.hits(), 1);
}
