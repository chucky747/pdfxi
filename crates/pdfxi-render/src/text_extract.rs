//! Backend-agnostic conversion of per-character text into [`TextPage`] values.
//!
//! PDF backends report text one character at a time, each with its own box and
//! font metrics. This module groups consecutive characters that share a
//! baseline, font size, and direction into [`TextSpan`]s and normalizes every
//! numeric field so the resulting page never contains non-finite coordinates.
//! It has no native dependency: the PDFium backend (with the `pdfium` feature)
//! only has to fill in [`RawChar`] values, and default builds can test the
//! conversion logic with synthetic characters.
//!
//! PDFium's character stream is already in the order the backend returns it.
//! [`chars_to_spans`] deliberately preserves that order rather than sorting by
//! geometry; see its documentation.

use pdfxi_text::{TextPage, TextSpan};

/// One character as reported by a text backend, in PDF page points.
///
/// The coordinate space follows the PDF convention used by PDFium: the origin
/// is the bottom-left of the page, x increases to the right, and y increases
/// upwards. Boxes may arrive with reversed edges or non-finite values from a
/// malformed document; [`chars_to_spans`] normalizes them.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct RawChar {
    /// UTF-8 text of this character; empty values are ignored.
    pub text: String,
    /// Left x coordinate of the character's tight box.
    pub left: f32,
    /// Right x coordinate of the character's tight box.
    pub right: f32,
    /// Bottom y coordinate of the character's tight box.
    pub bottom: f32,
    /// Top y coordinate of the character's tight box.
    pub top: f32,
    /// Baseline y coordinate (the character's text origin when available).
    pub baseline_y: f32,
    /// Font size of the character in page points.
    pub font_size: f32,
    /// Backend-reported baseline rotation in degrees.
    pub direction_degrees: f32,
}

impl RawChar {
    /// Build a character with explicit metrics.
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        text: impl Into<String>,
        left: f32,
        right: f32,
        bottom: f32,
        top: f32,
        baseline_y: f32,
        font_size: f32,
        direction_degrees: f32,
    ) -> Self {
        Self {
            text: text.into(),
            left,
            right,
            bottom,
            top,
            baseline_y,
            font_size,
            direction_degrees,
        }
    }
}

/// Return `value` when it is finite, otherwise `0.0`.
fn finite_or_zero(value: f32) -> f32 {
    if value.is_finite() { value } else { 0.0 }
}

/// Normalize an unordered coordinate pair into `(low, extent)`.
///
/// Non-finite inputs map to zero, the two coordinates are ordered, and the
/// returned extent is always finite and non-negative. This is the single place
/// where malformed geometry is made safe for the text model.
fn normalize_extent(first: f32, second: f32) -> (f32, f32) {
    let a = finite_or_zero(first);
    let b = finite_or_zero(second);
    let low = a.min(b);
    let high = a.max(b);
    let extent = high - low;
    (low, if extent.is_finite() { extent } else { 0.0 })
}

/// Return `true` when two finite values differ by at most `epsilon`.
fn within_epsilon(a: f32, b: f32, epsilon: f32) -> bool {
    (finite_or_zero(a) - finite_or_zero(b)).abs() <= epsilon
}

const BASELINE_EPSILON: f32 = 0.5;
const FONT_SIZE_EPSILON: f32 = 0.5;
const DIRECTION_EPSILON: f32 = 0.5;
/// A gap larger than this (in page points) ends a run even on a shared
/// baseline, so two distant columns are not merged into one span.
const MAX_RUN_GAP: f32 = 2.0;

/// Return `true` when `next` continues the same text run as `previous`.
fn same_run(previous: &RawChar, next: &RawChar) -> bool {
    if !within_epsilon(previous.baseline_y, next.baseline_y, BASELINE_EPSILON)
        || !within_epsilon(previous.font_size, next.font_size, FONT_SIZE_EPSILON)
        || !within_epsilon(
            previous.direction_degrees,
            next.direction_degrees,
            DIRECTION_EPSILON,
        )
    {
        return false;
    }

    let (previous_left, previous_width) = normalize_extent(previous.left, previous.right);
    let (next_left, _) = normalize_extent(next.left, next.right);
    let previous_right = finite_or_zero(previous_left + previous_width);
    next_left >= previous_left - MAX_RUN_GAP && next_left <= previous_right + MAX_RUN_GAP
}

/// Merge a non-empty group of consecutive characters into one span.
///
/// The group's text is concatenated verbatim, its box is the union of the
/// character boxes, and the metrics come from the first character in the group.
/// The fallback branch keeps this helper total (no indexing panic) if it is
/// ever reused with an empty slice.
fn merge_run(run: &[&RawChar]) -> TextSpan {
    let Some(first) = run.first() else {
        return TextSpan::new("", 0.0, 0.0);
    };

    let mut text = String::new();
    let mut min_left = f32::INFINITY;
    let mut max_right = f32::NEG_INFINITY;
    let mut min_bottom = f32::INFINITY;
    let mut max_top = f32::NEG_INFINITY;

    for character in run {
        text.push_str(&character.text);
        let (left, width) = normalize_extent(character.left, character.right);
        let (bottom, height) = normalize_extent(character.bottom, character.top);
        min_left = min_left.min(left);
        max_right = max_right.max(finite_or_zero(left + width));
        min_bottom = min_bottom.min(bottom);
        max_top = max_top.max(finite_or_zero(bottom + height));
    }

    let (x, width) = normalize_extent(min_left, max_right);
    let (_, height) = normalize_extent(min_bottom, max_top);

    TextSpan::new(text, x, finite_or_zero(first.baseline_y))
        .with_metrics(width, height, finite_or_zero(first.font_size))
        .with_direction(finite_or_zero(first.direction_degrees))
}

/// Convert backend characters into spans, preserving the backend's order.
///
/// Characters are visited in the order supplied and consecutive characters
/// with the same baseline, font size, direction, and near-adjacent boxes are
/// merged into one span. Empty-text characters are skipped. The caller's order
/// is never sorted or rearranged: when PDFium is the backend, the resulting
/// [`TextPage`] is in exactly the order PDFium returned the characters (its
/// text-page order, which follows the document's text operators for ordinary
/// PDFs). All numeric fields are finite and non-negative where appropriate.
pub(crate) fn chars_to_spans(chars: &[RawChar]) -> Vec<TextSpan> {
    let mut spans = Vec::new();
    let mut run: Vec<&RawChar> = Vec::new();

    for character in chars {
        if character.text.is_empty() {
            continue;
        }
        if run.last().is_some_and(|last| !same_run(last, character)) {
            spans.push(merge_run(&run));
            run.clear();
        }
        run.push(character);
    }
    if !run.is_empty() {
        spans.push(merge_run(&run));
    }

    spans
}

/// Build a [`TextPage`] from backend characters on `page`.
pub(crate) fn chars_to_text_page(page: usize, chars: &[RawChar]) -> TextPage {
    TextPage::new(page, chars_to_spans(chars))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a character on one line with a simple 6x10 box.
    fn character(text: &str, left: f32) -> RawChar {
        RawChar::new(text, left, left + 6.0, 700.0, 710.0, 702.0, 12.0, 0.0)
    }

    #[test]
    fn adjacent_characters_with_shared_metrics_merge_into_one_span() {
        let chars = vec![
            character("P", 10.0),
            character("D", 16.0),
            character("F", 22.0),
        ];
        let spans = chars_to_spans(&chars);

        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert_eq!(span.text, "PDF");
        assert_eq!((span.x, span.width), (10.0, 18.0));
        assert_eq!((span.height, span.font_size), (10.0, 12.0));
        assert_eq!(span.y, 702.0);
        assert_eq!(span.direction_degrees, 0.0);
    }

    #[test]
    fn changes_in_baseline_size_or_direction_start_a_new_span() {
        let mut next_line = character("B", 10.0);
        next_line.baseline_y = 682.0;
        next_line.bottom = 680.0;
        next_line.top = 690.0;

        let mut bigger = character("C", 16.0);
        bigger.font_size = 20.0;

        let mut rotated = character("D", 22.0);
        rotated.direction_degrees = 90.0;

        let chars = vec![character("A", 10.0), next_line, bigger, rotated];
        let spans = chars_to_spans(&chars);
        assert_eq!(
            spans
                .iter()
                .map(|span| span.text.as_str())
                .collect::<Vec<_>>(),
            vec!["A", "B", "C", "D"]
        );
    }

    #[test]
    fn reversed_and_non_finite_boxes_are_normalized() {
        let bad = RawChar::new(
            "X",
            50.0,
            10.0, // reversed x edges
            f32::NAN,
            15.0,
            f32::INFINITY,
            f32::NAN,
            f32::NEG_INFINITY,
        );
        let spans = chars_to_spans(&[bad]);

        assert_eq!(spans.len(), 1);
        let span = &spans[0];
        assert!(span.x.is_finite() && span.width.is_finite());
        assert!(span.y.is_finite() && span.height.is_finite());
        assert!(span.font_size.is_finite() && span.direction_degrees.is_finite());
        assert_eq!((span.x, span.width), (10.0, 40.0));
        assert_eq!(span.y, 0.0);
        assert_eq!(
            (span.height, span.font_size, span.direction_degrees),
            (15.0, 0.0, 0.0)
        );
    }

    #[test]
    fn empty_characters_are_skipped_without_splitting_a_run() {
        let empty = RawChar::new("", 13.0, 19.0, 700.0, 710.0, 702.0, 12.0, 0.0);
        let chars = vec![character("A", 10.0), empty, character("B", 16.0)];
        let spans = chars_to_spans(&chars);

        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].text, "AB");
    }

    #[test]
    fn distant_columns_on_one_baseline_are_not_merged() {
        let chars = vec![character("A", 10.0), character("B", 100.0)];
        let spans = chars_to_spans(&chars);
        assert_eq!(spans.len(), 2);
        assert_eq!(spans[0].text, "A");
        assert_eq!(spans[1].text, "B");
    }

    #[test]
    fn text_page_preserves_the_supplied_character_order() {
        let chars = vec![character("z", 10.0), character("a", 16.0)];
        let page = chars_to_text_page(3, &chars);
        assert_eq!(page.page(), 3);
        assert_eq!(page.text(), "za");
        assert_eq!(page.span_count(), 1);
    }
}
