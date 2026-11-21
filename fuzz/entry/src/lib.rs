//! Bounded, panic-free entry points for the `redaction` and `edit` fuzz targets.
//!
//! The five original targets in `fuzz/fuzz_targets/` wrap plain functions in
//! `pdfxi_testkit::fuzz`. Task T-0260A owns `fuzz/**` only, so these two entry
//! points live here instead of that shared crate; they reuse its
//! [`FuzzOutcome`] and [`MAX_FUZZ_INPUT`] so the shape and the input bound stay
//! identical.
//!
//! [`fuzz_redaction`] drives the real content-removing paths: the `pdfxi-edit`
//! redaction rewrite (`apply_redaction`) followed by a full save, the
//! independent verification reparse (`verify_redaction`), and the
//! `pdfxi-security` sanitize path (`sanitize_document_full`) that removes
//! hidden information and returns rewritten bytes, which are then checked by
//! `verify_sanitized`.
//!
//! [`fuzz_edit`] drives the annotation and content-edit entry points
//! (`add_highlight`, `add_text_note`, `text_blocks`, `list_objects`) with
//! bounded page indices, rectangles, points, and text derived from the input.
//!
//! Every function bounds its input, never indexes without a check, never uses
//! `unsafe`, and counts every failure instead of propagating it. A malformed
//! PDF is ordinary input here: a parse failure returns normally, and every
//! rejection from an edit entry point is a typed error.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use pdfxi_core::{Document, Rectangle};
use pdfxi_edit::content_edit::{list_objects, text_blocks};
use pdfxi_edit::document_annotations::{add_highlight, add_text_note};
use pdfxi_edit::geometry::{Point, Rect};
use pdfxi_edit::{RedactionMark, RedactionRequest, apply_redaction, verify_redaction};
use pdfxi_security::{SanitizeRequest, sanitize_document_full, verify_sanitized};
use pdfxi_testkit::fuzz::{FuzzOutcome, MAX_FUZZ_INPUT};

/// Maximum number of redaction marks derived from one input.
const MAX_MARKS: usize = 3;

/// Maximum number of highlight quads derived from one input.
const MAX_QUADS: usize = 3;

/// Maximum length of one annotation text field derived from the input.
const MAX_TEXT_BYTES: usize = 64;

/// Return a bounds-checked prefix of `data`.
fn bounded(data: &[u8]) -> &[u8] {
    let len = data.len().min(MAX_FUZZ_INPUT);
    &data[..len]
}

/// Build the common outcome prefix for a bounded input.
fn outcome_for(data: &[u8]) -> FuzzOutcome {
    FuzzOutcome {
        input_len: data.len(),
        ..FuzzOutcome::default()
    }
}

/// Add one success or failure to `outcome` without interpreting the value.
fn tally<T, E>(outcome: &mut FuzzOutcome, result: Result<T, E>) {
    if result.is_ok() {
        outcome.accepted += 1;
    } else {
        outcome.rejected += 1;
    }
}

/// Return one input byte, or zero past the end.
fn byte_at(data: &[u8], index: usize) -> u8 {
    data.get(index).copied().unwrap_or(0)
}

/// Return the page media box when it is finite and has a positive extent.
fn page_geometry(document: &Document, page: usize) -> Option<Rectangle> {
    let media_box = document.page(page).ok()?.media_box;
    let finite = [
        media_box.left,
        media_box.bottom,
        media_box.right,
        media_box.top,
    ]
    .iter()
    .all(|value| value.is_finite());
    if finite && media_box.width() > 0.0 && media_box.height() > 0.0 {
        Some(media_box)
    } else {
        None
    }
}

/// Build one in-page rectangle from two bytes, or `None` for a degenerate box.
fn mark_rect(media_box: &Rectangle, seed: u8) -> Option<Rect> {
    let width = media_box.width() as f32;
    let height = media_box.height() as f32;
    let left = media_box.left as f32;
    let bottom = media_box.bottom as f32;
    let offset = 0.05 + f32::from(seed % 8) / 32.0;
    let span_x = 0.10 + f32::from(seed % 5) / 40.0;
    let span_y = 0.05 + f32::from(seed % 3) / 40.0;
    let x0 = left + width * offset;
    let y0 = bottom + height * offset;
    Rect::new(x0, y0, x0 + width * span_x, y0 + height * span_y).ok()
}

/// Derive up to [`MAX_MARKS`] page-scoped redaction marks from the input.
fn bounded_marks(data: &[u8], media_box: &Rectangle, pages: usize) -> Vec<RedactionMark> {
    let mut marks = Vec::with_capacity(MAX_MARKS);
    for index in 0..MAX_MARKS {
        let seed = byte_at(data, index);
        let Some(rect) = mark_rect(media_box, seed) else {
            continue;
        };
        marks.push(RedactionMark::new(usize::from(seed) % pages, rect));
    }
    marks
}

/// Derive up to [`MAX_QUADS`] in-page highlight rectangles from the input.
fn bounded_quads(media_box: &Rectangle, data: &[u8]) -> Vec<Rect> {
    let width = media_box.width() as f32;
    let height = media_box.height() as f32;
    let left = media_box.left as f32;
    let bottom = media_box.bottom as f32;
    let count = usize::from(byte_at(data, 0) % (MAX_QUADS as u8)) + 1;
    let mut quads = Vec::with_capacity(count);
    for index in 0..count {
        let seed = byte_at(data, index + 1);
        let offset_x = 0.05 + f32::from(seed % 16) / 32.0;
        let offset_y = 0.05 + f32::from(seed % 16) / 32.0;
        let x0 = left + width * offset_x;
        let y0 = bottom + height * offset_y;
        if let Ok(rect) = Rect::new(x0, y0, x0 + width * 0.10, y0 + height * 0.04) {
            quads.push(rect);
        }
    }
    quads
}

/// Build a zero-area rectangle so a markup call returns its typed rejection.
///
/// `add_highlight` intentionally accepts quads outside the media box, so the
/// documented rejection it does enforce is the degenerate one (`EmptyQuads`).
fn degenerate_quad(media_box: &Rectangle, seed: u8) -> Rect {
    let width = media_box.width() as f32;
    let height = media_box.height() as f32;
    let x = media_box.left as f32 + width * f32::from(seed % 8) / 8.0;
    let y = media_box.bottom as f32 + height * f32::from(seed % 8) / 8.0;
    Rect::saturating(x, y, x, y)
}

/// Build an in-page point from one byte.
fn point_on_page(media_box: &Rectangle, seed: u8) -> Point {
    let width = media_box.width() as f32;
    let height = media_box.height() as f32;
    let fraction_x = 0.10 + f32::from(seed % 16) / 20.0;
    let fraction_y = 0.10 + f32::from(seed % 12) / 20.0;
    Point::new(
        media_box.left as f32 + width * fraction_x,
        media_box.bottom as f32 + height * fraction_y,
    )
}

/// Copy a bounded, printable slice of the input into a UTF-8 string.
fn bounded_text(data: &[u8], offset: usize, limit: usize) -> String {
    let start = offset.min(data.len());
    let end = start.saturating_add(limit).min(data.len());
    String::from_utf8_lossy(&data[start..end]).trim().to_owned()
}

/// Derive one printable secret candidate for the verification passes.
fn secret_candidates(data: &[u8]) -> Vec<String> {
    let secret: String = data
        .iter()
        .copied()
        .filter(u8::is_ascii_graphic)
        .take(8)
        .map(char::from)
        .collect();
    if secret.is_empty() {
        Vec::new()
    } else {
        vec![secret]
    }
}

/// Exercise the redaction removal, save, verification, and sanitize paths.
///
/// A parse failure is a counted rejection and returns normally. Otherwise one
/// redaction plan derived from the input is applied to a clone, the result is
/// saved as a full rewrite and reparsed, the independent verifier re-reads the
/// saved bytes, and the `pdfxi-security` sanitizer rewrites the original with
/// its own verification pass.
pub fn fuzz_redaction(data: &[u8]) -> FuzzOutcome {
    let data = bounded(data);
    let mut outcome = outcome_for(data);

    let Ok(document) = Document::from_bytes(data.to_vec()) else {
        outcome.rejected += 1;
        return outcome;
    };
    outcome.accepted += 1;

    let Ok(pages) = document.page_count() else {
        outcome.rejected += 1;
        return outcome;
    };
    if pages == 0 {
        outcome.rejected += 1;
        return outcome;
    }

    let secrets = secret_candidates(data);
    let secret_refs: Vec<&str> = secrets.iter().map(String::as_str).collect();

    let page_index = usize::from(byte_at(data, 0)) % pages;
    let marks = page_geometry(&document, page_index)
        .map(|media_box| bounded_marks(data, &media_box, pages))
        .unwrap_or_default();
    let flags = byte_at(data, 3);
    let request = RedactionRequest {
        marks,
        keep_images: flags & 0x01 != 0,
        remove_annotations: flags & 0x02 != 0,
        remove_links: flags & 0x04 != 0,
        remove_form_fields: flags & 0x08 != 0,
        remove_metadata: flags & 0x10 != 0,
    };

    let mut working = document.clone();
    match apply_redaction(&mut working, &request) {
        Ok(report) => {
            outcome.accepted += 1;
            match working.save() {
                Ok(bytes) => tally(&mut outcome, Document::from_bytes(bytes)),
                Err(_) => outcome.rejected += 1,
            }
            tally(
                &mut outcome,
                verify_redaction(&working, &secret_refs, &report).map(|_| ()),
            );
        }
        Err(_) => outcome.rejected += 1,
    }

    match sanitize_document_full(&document, &SanitizeRequest::all()) {
        Ok(sanitized) => {
            outcome.accepted += 1;
            tally(&mut outcome, sanitized.document().map(|_| ()));
            tally(
                &mut outcome,
                verify_sanitized(sanitized.bytes(), &secret_refs).map(|_| ()),
            );
        }
        Err(_) => outcome.rejected += 1,
    }

    outcome
}

/// Exercise the annotation and content-edit entry points.
///
/// A parse failure is a counted rejection and returns normally. Otherwise the
/// read-only content model, a highlight, a sticky note, and two documented
/// rejections (an empty identifier and a degenerate quad) run against the real
/// `pdfxi-edit` code on one page chosen by the input. Every edit returns a
/// typed error rather than panicking.
pub fn fuzz_edit(data: &[u8]) -> FuzzOutcome {
    let data = bounded(data);
    let mut outcome = outcome_for(data);

    let Ok(mut document) = Document::from_bytes(data.to_vec()) else {
        outcome.rejected += 1;
        return outcome;
    };
    outcome.accepted += 1;

    let Ok(pages) = document.page_count() else {
        outcome.rejected += 1;
        return outcome;
    };
    if pages == 0 {
        outcome.rejected += 1;
        return outcome;
    }
    let page_index = usize::from(byte_at(data, 0)) % pages;

    tally(&mut outcome, text_blocks(&document, page_index).map(|_| ()));
    tally(
        &mut outcome,
        list_objects(&document, page_index).map(|_| ()),
    );

    let Some(media_box) = page_geometry(&document, page_index) else {
        outcome.rejected += 1;
        return outcome;
    };
    let contents = bounded_text(data, 1, MAX_TEXT_BYTES);
    let author = bounded_text(data, 8, MAX_TEXT_BYTES / 2);
    let quads = bounded_quads(&media_box, data);

    tally(
        &mut outcome,
        add_highlight(&mut document, page_index, &quads, &contents, &author).map(|_| ()),
    );

    let point = point_on_page(&media_box, byte_at(data, 1));
    let identifier = format!("fuzz-note-{}", byte_at(data, 2));
    tally(
        &mut outcome,
        add_text_note(
            &mut document,
            page_index,
            point,
            &contents,
            &author,
            &identifier,
        )
        .map(|_| ()),
    );

    // Documented rejections: an empty identifier and a degenerate quad.
    tally(
        &mut outcome,
        add_text_note(&mut document, page_index, point, &contents, &author, "").map(|_| ()),
    );
    tally(
        &mut outcome,
        add_highlight(
            &mut document,
            page_index,
            &[degenerate_quad(&media_box, byte_at(data, 3))],
            &contents,
            &author,
        )
        .map(|_| ()),
    );

    outcome
}
