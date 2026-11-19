//! Text-selection model: drag points to ordered `(page, char offset)` ranges.
//!
//! Like [`crate::canvas`], this module is dependency-free. Rather than depend on
//! `pdfxi-text` (or any other crate), it defines the small [`SelectableTextPage`]
//! trait describing just the geometry selection needs. A caller that already
//! has `pdfxi_text::TextPage` values can implement this local trait for that
//! foreign type in its own crate (the trait is local, so the orphan rule
//! permits it), mapping bottom-up PDF baselines into the top-left-origin,
//! y-down page coordinates documented on [`SpanView`]. An owned
//! [`OwnedTextPage`] is provided for callers that do not have a backend.
//!
//! Offsets are **character** (Unicode scalar) offsets into a page's recovered
//! text, which makes slicing safe and deterministic for multibyte text. Drag
//! anchors and heads are normalized into an ordered [`Selection`] so selecting
//! forward or backward yields the same result. [`SelectionModel::selected_text`]
//! joins fragments from multiple pages with a newline.

use crate::canvas::{CanvasLayout, CanvasPoint};

/// A borrowed view of one text span's geometry.
///
/// Coordinates are in PDF points with the origin at the page's top-left corner
/// and `y` increasing downward. `width`/`height` are the span's bounding box;
/// per-character positions are interpolated evenly across `width`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SpanView<'a> {
    /// The span's UTF-8 text.
    pub text: &'a str,
    /// Left edge of the span's box, in points.
    pub x: f32,
    /// Top edge of the span's box, in points (y-down).
    pub y: f32,
    /// Width of the span's box, in points.
    pub width: f32,
    /// Height of the span's box, in points.
    pub height: f32,
}

/// A minimal text-page interface required by [`SelectionModel`].
///
/// Implement this for any backend text page. It intentionally exposes only the
/// page number and span geometry needed for hit testing, keeping this crate
/// dependency-light.
pub trait SelectableTextPage {
    /// The zero-based page number this text belongs to.
    fn page_number(&self) -> usize;

    /// Borrowed geometry for every span, in reading order.
    fn spans(&self) -> Vec<SpanView<'_>>;
}

/// An owned text span used by [`OwnedTextPage`].
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedSpan {
    /// The span's UTF-8 text.
    pub text: String,
    /// Left edge of the span's box, in points.
    pub x: f32,
    /// Top edge of the span's box, in points (y-down).
    pub y: f32,
    /// Width of the span's box, in points.
    pub width: f32,
    /// Height of the span's box, in points.
    pub height: f32,
}

impl OwnedSpan {
    /// Create a span with `text` at `(x, y)` and the given box size.
    pub fn new(text: impl Into<String>, x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            width,
            height,
        }
    }
}

/// A backend-free text page implementing [`SelectableTextPage`].
#[derive(Clone, Debug, PartialEq)]
pub struct OwnedTextPage {
    page: usize,
    spans: Vec<OwnedSpan>,
}

impl OwnedTextPage {
    /// Create a text page from its zero-based `page` number and spans.
    pub fn new(page: usize, spans: Vec<OwnedSpan>) -> Self {
        Self { page, spans }
    }

    /// Create an empty text page.
    pub fn empty(page: usize) -> Self {
        Self::new(page, Vec::new())
    }

    /// The zero-based page number.
    pub fn page_number(&self) -> usize {
        self.page
    }

    /// The spans in reading order.
    pub fn spans(&self) -> &[OwnedSpan] {
        &self.spans
    }
}

impl SelectableTextPage for OwnedTextPage {
    fn page_number(&self) -> usize {
        self.page
    }

    fn spans(&self) -> Vec<SpanView<'_>> {
        self.spans
            .iter()
            .map(|span| SpanView {
                text: &span.text,
                x: span.x,
                y: span.y,
                width: span.width,
                height: span.height,
            })
            .collect()
    }
}

/// One end of a text selection: a page and a character offset into its text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct TextAnchor {
    /// Zero-based page number.
    pub page: usize,
    /// Character offset into the page's concatenated text.
    pub offset: usize,
}

impl TextAnchor {
    /// Create an anchor for `page` at character `offset`.
    pub const fn new(page: usize, offset: usize) -> Self {
        Self { page, offset }
    }

    /// Ordering key used to normalize a selection.
    fn key(self) -> (usize, usize) {
        (self.page, self.offset)
    }
}

/// An ordered, normalized range of text across one or more pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Selection {
    /// The earlier end (by page, then offset).
    pub start: TextAnchor,
    /// The later end (by page, then offset).
    pub end: TextAnchor,
}

impl Selection {
    /// Return `true` when the selection covers no characters.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Return `true` when the selection spans more than one page.
    pub fn spans_pages(&self) -> bool {
        self.start.page != self.end.page
    }

    /// The number of pages touched by this selection (at least one).
    pub fn page_span(&self) -> usize {
        self.end.page.saturating_sub(self.start.page) + 1
    }
}

/// Tracks a text-selection drag and produces deterministic copy text.
///
/// Call [`Self::begin_at`] on pointer-down, [`Self::update_to`] while dragging,
/// and read [`Self::selection`] / [`Self::selected_text`] at any time. Anchors
/// may also be set directly with [`Self::set_anchor`] / [`Self::set_head`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SelectionModel {
    anchor: Option<TextAnchor>,
    head: Option<TextAnchor>,
}

impl SelectionModel {
    /// Create an empty selection model.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether a drag has started (an anchor exists).
    pub fn is_active(&self) -> bool {
        self.anchor.is_some()
    }

    /// The drag anchor, if any.
    pub fn anchor(&self) -> Option<TextAnchor> {
        self.anchor
    }

    /// The current drag head, if any.
    pub fn head(&self) -> Option<TextAnchor> {
        self.head
    }

    /// Clear the selection entirely.
    pub fn clear(&mut self) {
        self.anchor = None;
        self.head = None;
    }

    /// Set the anchor directly (the head follows it until moved).
    pub fn set_anchor(&mut self, anchor: TextAnchor) {
        self.anchor = Some(anchor);
        if self.head.is_none() {
            self.head = Some(anchor);
        }
    }

    /// Set the head directly.
    pub fn set_head(&mut self, head: TextAnchor) {
        self.head = Some(head);
        if self.anchor.is_none() {
            self.anchor = Some(head);
        }
    }

    /// Set both ends at once, in any order.
    pub fn drag(&mut self, from: TextAnchor, to: TextAnchor) {
        self.anchor = Some(from);
        self.head = Some(to);
    }

    /// Start a drag at a canvas point.
    ///
    /// The point is mapped through `layout` to a page-local coordinate, then the
    /// nearest character on the matching [`SelectableTextPage`] becomes the
    /// anchor. If the point is off-page or the page has no characters, the model
    /// is cleared.
    pub fn begin_at<P: SelectableTextPage>(
        &mut self,
        point: CanvasPoint,
        layout: &CanvasLayout,
        pages: &[P],
    ) {
        self.clear();
        if let Some(anchor) = anchor_at(point, layout, pages) {
            self.anchor = Some(anchor);
            self.head = Some(anchor);
        }
    }

    /// Move the head of an active drag to a canvas point.
    ///
    /// Ignored when no drag is active or the point cannot be resolved.
    pub fn update_to<P: SelectableTextPage>(
        &mut self,
        point: CanvasPoint,
        layout: &CanvasLayout,
        pages: &[P],
    ) {
        if self.anchor.is_none() {
            return;
        }
        if let Some(anchor) = anchor_at(point, layout, pages) {
            self.head = Some(anchor);
        }
    }

    /// The normalized selection, or `None` when no drag is active.
    pub fn selection(&self) -> Option<Selection> {
        let anchor = self.anchor?;
        let head = self.head?;
        let (start, end) = if anchor.key() <= head.key() {
            (anchor, head)
        } else {
            (head, anchor)
        };
        Some(Selection { start, end })
    }

    /// Produce deterministic copy text for the current selection.
    ///
    /// Fragments from each touched page are concatenated in ascending page
    /// order and separated by a single newline. Returns an empty string when no
    /// selection exists, it is zero-length, or none of its pages are supplied.
    pub fn selected_text<P: SelectableTextPage>(&self, pages: &[P]) -> String {
        let Some(selection) = self.selection() else {
            return String::new();
        };
        if selection.is_empty() {
            return String::new();
        }

        let mut fragments: Vec<String> = Vec::new();
        for page_number in selection.start.page..=selection.end.page {
            let Some(page) = pages.iter().find(|page| page.page_number() == page_number) else {
                continue;
            };
            let chars = page_chars(page);
            let from = if page_number == selection.start.page {
                selection.start.offset
            } else {
                0
            };
            let to = if page_number == selection.end.page {
                selection.end.offset
            } else {
                chars.len()
            };
            let from = from.min(chars.len());
            let to = to.min(chars.len());
            if from < to {
                fragments.push(chars[from..to].iter().collect());
            }
        }
        fragments.join("\n")
    }
}

/// Resolve a canvas point to the nearest character anchor on its page.
fn anchor_at<P: SelectableTextPage>(
    point: CanvasPoint,
    layout: &CanvasLayout,
    pages: &[P],
) -> Option<TextAnchor> {
    let (page_index, x_pt, y_pt) = layout.page_point(point)?;
    let page = pages.iter().find(|page| page.page_number() == page_index)?;
    nearest_anchor(page, x_pt, y_pt)
}

/// Find the character whose interpolated box center is closest to `(x_pt, y_pt)`.
fn nearest_anchor<P: SelectableTextPage>(page: &P, x_pt: f32, y_pt: f32) -> Option<TextAnchor> {
    let spans = page.spans();
    let mut best: Option<(usize, f64)> = None;
    let mut cumulative = 0usize;

    for span in &spans {
        let chars: Vec<char> = span.text.chars().collect();
        let count = chars.len();
        let char_width = if count > 0 && span.width > 0.0 {
            span.width / count as f32
        } else {
            0.0
        };
        let center_y = span.y + span.height * 0.5;
        for index in 0..count {
            let center_x = span.x + (index as f32 + 0.5) * char_width;
            let dx = f64::from(center_x - x_pt);
            let dy = f64::from(center_y - y_pt);
            let distance = dx * dx + dy * dy;
            if best.is_none_or(|(_, best_distance)| distance < best_distance) {
                best = Some((cumulative + index, distance));
            }
        }
        cumulative = cumulative.saturating_add(count);
    }

    best.map(|(offset, _)| TextAnchor::new(page.page_number(), offset))
}

/// Concatenate all span characters of a page into one vector.
fn page_chars<P: SelectableTextPage>(page: &P) -> Vec<char> {
    let mut chars = Vec::new();
    for span in page.spans() {
        chars.extend(span.text.chars());
    }
    chars
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::{CanvasSize, PageSize};
    use crate::view::{ViewMode, Zoom};

    fn page_zero() -> OwnedTextPage {
        OwnedTextPage::new(0, vec![OwnedSpan::new("Hello", 0.0, 0.0, 50.0, 10.0)])
    }

    fn page_one() -> OwnedTextPage {
        OwnedTextPage::new(1, vec![OwnedSpan::new("World", 0.0, 0.0, 50.0, 10.0)])
    }

    fn single_page_layout() -> CanvasLayout {
        CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 900.0),
            &[PageSize::new(612.0, 792.0)],
        )
    }

    #[test]
    fn selection_normalization_in_both_drag_directions() {
        let mut forward = SelectionModel::new();
        forward.drag(TextAnchor::new(0, 2), TextAnchor::new(0, 5));
        let mut backward = SelectionModel::new();
        backward.drag(TextAnchor::new(0, 5), TextAnchor::new(0, 2));
        assert_eq!(forward.selection(), backward.selection());
        assert_eq!(
            forward.selection(),
            Some(Selection {
                start: TextAnchor::new(0, 2),
                end: TextAnchor::new(0, 5),
            })
        );

        // Reversed page order is normalized by (page, offset).
        let mut cross = SelectionModel::new();
        cross.drag(TextAnchor::new(1, 3), TextAnchor::new(0, 1));
        let selection = cross.selection().expect("normalized");
        assert_eq!(selection.start, TextAnchor::new(0, 1));
        assert_eq!(selection.end, TextAnchor::new(1, 3));
        assert_eq!(selection.page_span(), 2);
    }

    #[test]
    fn empty_selection_copies_to_empty_string() {
        let pages = [page_zero(), page_one()];
        let mut model = SelectionModel::new();
        assert_eq!(model.selected_text(&pages), "");
        assert!(model.selection().is_none());

        model.drag(TextAnchor::new(0, 3), TextAnchor::new(0, 3));
        assert!(
            model
                .selection()
                .is_some_and(|selection| selection.is_empty())
        );
        assert_eq!(model.selected_text(&pages), "");

        model.clear();
        assert!(!model.is_active());
        assert_eq!(model.selected_text(&pages), "");
    }

    #[test]
    fn cross_page_selection_copy_joins_with_newline_in_page_order() {
        let pages = [page_zero(), page_one()]; // "Hello" / "World"
        let mut model = SelectionModel::new();
        // From character 3 of page 0 ("lo") through character 3 of page 1 ("Wor").
        model.drag(TextAnchor::new(0, 3), TextAnchor::new(1, 3));
        assert_eq!(model.selected_text(&pages), "lo\nWor");

        // Supplying pages out of order still produces ascending page order.
        let reversed = [page_one(), page_zero()];
        assert_eq!(model.selected_text(&reversed), "lo\nWor");
    }

    #[test]
    fn same_page_selection_copy_uses_character_offsets() {
        let pages = [page_zero()];
        let mut model = SelectionModel::new();
        model.drag(TextAnchor::new(0, 1), TextAnchor::new(0, 4));
        assert_eq!(model.selected_text(&pages), "ell");
    }

    #[test]
    fn multibyte_copy_slices_on_character_boundaries() {
        let pages = [OwnedTextPage::new(
            0,
            vec![OwnedSpan::new("café", 0.0, 0.0, 40.0, 10.0)],
        )];
        let mut model = SelectionModel::new();
        // "café" has four characters; offset 3 is the 'é' (2 bytes).
        model.drag(TextAnchor::new(0, 2), TextAnchor::new(0, 4));
        assert_eq!(model.selected_text(&pages), "fé");
    }

    #[test]
    fn begin_and_update_map_points_to_characters() {
        let layout = single_page_layout();
        let pages = [page_zero()]; // "ABCD"-style evenly spaced text on page 0
        let rect = layout.page(0).expect("page 0").rect;

        let mut model = SelectionModel::new();
        // Span x=0 width=50 for "Hello": char centers at 5,15,25,35,45.
        // x=37 selects char index 3 ('l'); y=5 is the row center.
        model.begin_at(
            CanvasPoint::new(rect.x + 37.0, rect.y + 5.0),
            &layout,
            &pages,
        );
        assert_eq!(model.anchor(), Some(TextAnchor::new(0, 3)));
        assert_eq!(model.head(), Some(TextAnchor::new(0, 3)));

        // Drag backward to x=7 selects char index 0 ('H').
        model.update_to(
            CanvasPoint::new(rect.x + 7.0, rect.y + 5.0),
            &layout,
            &pages,
        );
        assert_eq!(model.head(), Some(TextAnchor::new(0, 0)));
        assert_eq!(model.selected_text(&pages), "Hel");
    }

    #[test]
    fn off_page_begin_clears_the_model_and_update_is_ignored() {
        let layout = single_page_layout();
        let pages = [page_zero()];
        let mut model = SelectionModel::new();
        model.drag(TextAnchor::new(0, 1), TextAnchor::new(0, 2));
        model.begin_at(CanvasPoint::new(-20.0, -20.0), &layout, &pages);
        assert!(!model.is_active());
        assert_eq!(model.selected_text(&pages), "");

        model.set_anchor(TextAnchor::new(0, 0));
        let before = model.head();
        model.update_to(CanvasPoint::new(-20.0, -20.0), &layout, &pages);
        assert_eq!(model.head(), before);
    }

    #[test]
    fn empty_page_produces_no_anchor() {
        let layout = single_page_layout();
        let pages = [OwnedTextPage::empty(0)];
        let rect = layout.page(0).expect("page 0").rect;
        let mut model = SelectionModel::new();
        model.begin_at(
            CanvasPoint::new(rect.x + 10.0, rect.y + 5.0),
            &layout,
            &pages,
        );
        assert!(!model.is_active());
    }
}
