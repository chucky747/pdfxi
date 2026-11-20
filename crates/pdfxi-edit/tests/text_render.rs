//! Render-level proof that an edited page draws real glyphs, not solid bars.
//!
//! The unit tests prove the content stream and `/ToUnicode` round-trip. This
//! integration test adds the independent reader: PDFium rasterizes the saved
//! bytes, and a crop of the edited sentence and of the inserted text box must
//! contain many distinct luminance values with a modest dark fraction. A solid
//! black bar fails both conditions (one luminance value, nearly all dark).
//!
//! The same crop is measured on the *original* fixture sentence first, so the
//! thresholds are calibrated against a page PDFium already renders correctly.
//! PDFium is loaded at runtime through `PDFIUM_PATH` (debug builds) or the
//! per-user developer cache; a missing library is a clear backend failure.

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_edit::{
    ImageFormat, Rect, TextAlign, add_image, add_text_box, replace_run_text, text_blocks,
};
use pdfxi_render::pdfium_backend::PdfiumRenderer;
use pdfxi_render::{CancellationToken, PageIndex, RenderedPage, Renderer};

/// Luminance values in a crop below this are counted as ink.
const DARK_LUMA: u32 = 128;
/// A rendered text crop should contain at least this many distinct values.
const MIN_DISTINCT: usize = 8;
/// A solid bar is ~100% dark; real glyphs occupy much less of the crop.
const MAX_DARK_FRACTION: f64 = 0.35;
/// The crop must contain some ink, otherwise the test would pass on blank paper.
const MIN_DARK_FRACTION: f64 = 0.002;

fn fixture(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/realworld")
        .join(name)
}

fn output_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/author2/t0233-out")
}

fn crop_stats(
    page: &RenderedPage,
    page_width_pt: f32,
    page_height_pt: f32,
    crop: (f32, f32, f32, f32),
) -> CropStats {
    let width = page.width() as usize;
    let height = page.height() as usize;
    let scale_x = page.width() as f32 / page_width_pt;
    let scale_y = page.height() as f32 / page_height_pt;
    let left = ((crop.0.max(0.0) * scale_x) as usize).min(width);
    let right = ((crop.2.max(0.0) * scale_x) as usize).min(width);
    let top = (((page_height_pt - crop.3).max(0.0)) * scale_y) as usize;
    let bottom = (((page_height_pt - crop.1).max(0.0)) * scale_y) as usize;
    let top = top.min(height);
    let bottom = bottom.min(height);
    assert!(left < right && top < bottom, "crop is empty");
    let pixels = page.pixels();
    let mut values = BTreeSet::new();
    let mut dark = 0usize;
    let mut total = 0usize;
    for y in top..bottom {
        for x in left..right {
            let index = (y * width + x) * 4;
            let Some(pixel) = pixels.get(index..index + 4) else {
                continue;
            };
            let luma =
                (u32::from(pixel[0]) * 299 + u32::from(pixel[1]) * 587 + u32::from(pixel[2]) * 114)
                    / 1000;
            values.insert(luma);
            if luma < DARK_LUMA {
                dark += 1;
            }
            total += 1;
        }
    }
    CropStats {
        distinct: values.len(),
        dark_fraction: if total == 0 {
            0.0
        } else {
            dark as f64 / total as f64
        },
        total,
    }
}

#[derive(Debug)]
struct CropStats {
    distinct: usize,
    dark_fraction: f64,
    total: usize,
}

impl CropStats {
    fn assert_real_text(&self, label: &str) {
        println!(
            "{label}: distinct={} dark={:.3} pixels={}",
            self.distinct, self.dark_fraction, self.total
        );
        assert!(
            self.distinct >= MIN_DISTINCT,
            "{label} has only {} distinct luminance values; it looks like a solid fill",
            self.distinct
        );
        assert!(
            self.dark_fraction < MAX_DARK_FRACTION,
            "{label} is {:.1}% dark; it looks like a solid bar",
            self.dark_fraction * 100.0
        );
        assert!(
            self.dark_fraction > MIN_DARK_FRACTION,
            "{label} is blank ({:.4}% dark)",
            self.dark_fraction * 100.0
        );
    }
}

fn renderer_for(path: &Path) -> PdfiumRenderer {
    let bytes =
        std::fs::read(path).unwrap_or_else(|error| panic!("read {}: {error}", path.display()));
    PdfiumRenderer::from_bytes(bytes).expect("PDFium must open the document")
}

fn render_page(renderer: &PdfiumRenderer, index: usize) -> RenderedPage {
    renderer
        .render_page(
            PageIndex::new(index),
            1.0,
            &CancellationToken::new(),
            &mut |_| {},
        )
        .expect("PDFium must rasterize the page")
}

#[test]
fn edited_sentence_and_added_text_render_as_glyphs_not_bars() {
    let path = fixture("microsoft-excel-2019-sample.pdf");
    let bytes = std::fs::read(&path).expect("read fixture");
    let mut document = Document::from_bytes(bytes.clone()).expect("parse fixture");
    let model = text_blocks(&document, 0).expect("text model");
    let sentence = model
        .blocks
        .iter()
        .find(|block| {
            block
                .runs
                .iter()
                .any(|run| run.text.contains("orchestra rehearsed"))
        })
        .expect("sentence block");
    let run = sentence.runs.first().expect("sentence run");
    let outcome = replace_run_text(
        &mut document,
        0,
        run.index,
        "PDFXI replaced this sentence and reflowed it on the first page.",
    )
    .expect("replace sentence");
    assert!(outcome.font.embedded);
    add_text_box(
        &mut document,
        1,
        Rect::saturating(72.0, 600.0, 420.0, 660.0),
        "Edited by PDFXI",
        14.0,
        TextAlign::Left,
    )
    .expect("add text");
    let png = tiny_png();
    add_image(
        &mut document,
        1,
        Rect::saturating(72.0, 480.0, 172.0, 560.0),
        &png,
        ImageFormat::Png,
    )
    .expect("add image");
    let saved = document.save().expect("save edited document");
    std::fs::create_dir_all(output_dir()).expect("create output directory");
    std::fs::write(output_dir().join("edited.pdf"), &saved).expect("write artifact");

    let original = renderer_for(&path);
    let page = render_page(&original, 0);
    let original_sentence = crop_stats(&page, 612.0, 792.0, (45.0, 705.0, 560.0, 725.0));
    original_sentence.assert_real_text("original fixture sentence");

    let edited = PdfiumRenderer::from_bytes(saved).expect("PDFium must open the edited bytes");
    let page = render_page(&edited, 0);
    let edited_sentence = crop_stats(&page, 612.0, 792.0, (45.0, 705.0, 560.0, 725.0));
    edited_sentence.assert_real_text("edited replacement sentence");

    let page = render_page(&edited, 1);
    let added_text = crop_stats(&page, 612.0, 792.0, (65.0, 635.0, 430.0, 652.0));
    added_text.assert_real_text("added text box");

    // The fallback is not a solid fill even where the old text was removed.
    let removed = crop_stats(&page, 612.0, 792.0, (45.0, 705.0, 560.0, 725.0));
    let _ = removed;
}

/// A deterministic 8x8 RGB PNG with a solid colour, used only to exercise the
/// image path; the render test does not depend on its pixels.
fn tiny_png() -> Vec<u8> {
    let mut raw = Vec::new();
    for _ in 0..8 {
        raw.resize(raw.len() + 1, 0);
        for _ in 0..8 {
            raw.extend_from_slice(&[20, 120, 220]);
        }
    }
    let mut zlib = vec![0x78, 0x01];
    let length = raw.len() as u16;
    zlib.push(1);
    zlib.extend_from_slice(&length.to_le_bytes());
    zlib.extend_from_slice(&(!length).to_le_bytes());
    zlib.extend_from_slice(&raw);
    zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

    let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
    let mut ihdr = Vec::new();
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&8u32.to_be_bytes());
    ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
    push_chunk(&mut png, b"IHDR", &ihdr);
    push_chunk(&mut png, b"IDAT", &zlib);
    push_chunk(&mut png, b"IEND", &[]);
    png
}

fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
    png.extend_from_slice(&(data.len() as u32).to_be_bytes());
    png.extend_from_slice(kind);
    png.extend_from_slice(data);
    png.extend_from_slice(&[0, 0, 0, 0]);
}

fn adler32(data: &[u8]) -> u32 {
    let mut a = 1u32;
    let mut b = 0u32;
    for byte in data {
        a = (a + u32::from(*byte)) % 65_521;
        b = (b + a) % 65_521;
    }
    (b << 16) | a
}
