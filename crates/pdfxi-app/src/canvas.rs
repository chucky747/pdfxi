//! Dependency-free canvas layout, hit testing, and coordinate mapping.
//!
//! This module is the geometric brain of the viewer canvas. It arranges pages
//! into rows for the four [`ViewMode`] layouts, scales them from PDF points to
//! canvas units, answers "which page is under this point?", maps canvas points
//! back into page-local coordinates, and clamps scroll offsets to the laid-out
//! content. It deliberately depends on **no renderer and no text backend**: the
//! only inputs are page sizes (in PDF points), a [`Zoom`] policy, a viewport
//! size, and a [`ViewMode`]. That keeps the whole model testable without a GUI.
//!
//! # Coordinate systems
//!
//! - Page sizes are supplied in PDF points (`1 pt = 1/72 in`).
//! - Laid-out geometry lives in canvas units, with the origin at the top-left
//!   of the content area, `x` increasing right and `y` increasing down. The
//!   scale factor converts points to canvas units (a "CSS-like" device scale).
//! - [`CanvasLayout::page_point`] reports a point in page-local coordinates:
//!   origin at the page's top-left corner, one unit per PDF point, `y`
//!   increasing **downward**. Callers adapting a bottom-up PDF text span must
//!   flip `y` themselves (see [`crate::selection`]).
//!
//! # Rotation
//!
//! Layout is rotation-aware. The effective rotation of a page is its intrinsic
//! PDF `/Rotate` combined with the per-tab view rotation
//! ([`Rotation::combine`]); the per-tab view rotation is kept separate and
//! never mutates the document. Quarter turns swap the displayed width/height
//! before scale resolution, so fit modes fit what the user actually sees.
//! Pointer hits are mapped through the *inverse* rotation in
//! [`CanvasLayout::page_point`], while page-local overlay rectangles are mapped
//! forward in [`CanvasLayout::page_rect_to_canvas`]. See
//! [`effective_page_size`] and [`uv_quad_for_rotation`] for the pure primitives
//! used by layout and raster placement respectively.

use crate::view::{DEFAULT_ZOOM, Rotation, ViewMode, Zoom};

/// Gap, in canvas units, between adjacent pages and between vertical rows.
pub const DEFAULT_GAP: f32 = 16.0;

/// Padding, in canvas units, around the laid-out content.
pub const DEFAULT_PADDING: f32 = 24.0;

/// A 2D point in canvas units (`x` right, `y` down).
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasPoint {
    /// Horizontal coordinate.
    pub x: f32,
    /// Vertical coordinate.
    pub y: f32,
}

impl CanvasPoint {
    /// Create a point at `(x, y)`.
    pub const fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }
}

/// An axis-aligned rectangle in canvas units.
///
/// Rectangles are half-open: a point is contained when
/// `x >= left && x < right && y >= top && y < bottom`. This makes hits at page
/// boundaries deterministic — the right/bottom edge belongs to the gap, not to
/// the page.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasRect {
    /// Left edge (inclusive).
    pub x: f32,
    /// Top edge (inclusive).
    pub y: f32,
    /// Rectangle width.
    pub width: f32,
    /// Rectangle height.
    pub height: f32,
}

impl CanvasRect {
    /// Create a rectangle from its top-left corner and size.
    pub const fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
        }
    }

    /// The inclusive left edge.
    pub fn left(self) -> f32 {
        self.x
    }

    /// The inclusive top edge.
    pub fn top(self) -> f32 {
        self.y
    }

    /// The exclusive right edge.
    pub fn right(self) -> f32 {
        self.x + self.width
    }

    /// The exclusive bottom edge.
    pub fn bottom(self) -> f32 {
        self.y + self.height
    }

    /// Return `true` when `point` lies inside this half-open rectangle.
    pub fn contains(self, point: CanvasPoint) -> bool {
        point.x >= self.left()
            && point.x < self.right()
            && point.y >= self.top()
            && point.y < self.bottom()
    }
}

/// The size of the canvas viewport, in canvas units.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct CanvasSize {
    /// Viewport width.
    pub width: f32,
    /// Viewport height.
    pub height: f32,
}

impl CanvasSize {
    /// Create a viewport size.
    pub const fn new(width: f32, height: f32) -> Self {
        Self { width, height }
    }
}

/// The intrinsic size of one page, in PDF points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct PageSize {
    /// Page width in points.
    pub width_pt: f32,
    /// Page height in points.
    pub height_pt: f32,
}

impl PageSize {
    /// Create a page size from its width and height in points.
    pub const fn new(width_pt: f32, height_pt: f32) -> Self {
        Self {
            width_pt,
            height_pt,
        }
    }
}

/// Where a canvas point landed relative to the laid-out pages.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CanvasHit {
    /// The point is over the page with this zero-based index.
    Page(usize),
    /// The point is inside the content bounds but between pages.
    Gap,
    /// The point is outside the content bounds entirely.
    Outside,
}

impl CanvasHit {
    /// Return the page index when this hit is over a page.
    pub fn page_index(self) -> Option<usize> {
        match self {
            Self::Page(index) => Some(index),
            _ => None,
        }
    }

    /// Return `true` when this hit is over a page.
    pub fn is_page(self) -> bool {
        matches!(self, Self::Page(_))
    }
}

/// The resolved geometry of a single laid-out page.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageGeometry {
    /// Zero-based page index.
    pub page: usize,
    /// The page's rectangle in canvas units (excludes scroll).
    ///
    /// For quarter turns this rectangle's width and height are the *swapped*
    /// intrinsic extents multiplied by [`Self::scale`], matching what is shown
    /// on screen.
    pub rect: CanvasRect,
    /// Scale from page points to canvas units for this layout.
    pub scale: f32,
    /// Intrinsic page width in points (before rotation).
    pub width_pt: f32,
    /// Intrinsic page height in points (before rotation).
    pub height_pt: f32,
    /// The page's intrinsic `/Rotate` from the PDF, independent of the view.
    pub page_rotation: Rotation,
    /// The effective rotation: page `/Rotate` combined with the view rotation.
    pub rotation: Rotation,
}

/// A deterministic arrangement of pages for one view configuration.
///
/// Build one with [`CanvasLayout::new`] whenever the view mode, zoom, viewport,
/// or page sizes change, then query it with [`Self::page_at`],
/// [`Self::page_point`], and [`Self::clamp_scroll`]. The layout owns a snapshot
/// of the geometry; it never reads live document state.
#[derive(Clone, Debug, PartialEq)]
pub struct CanvasLayout {
    pages: Vec<PageGeometry>,
    content_width: f32,
    content_height: f32,
    viewport: CanvasSize,
    mode: ViewMode,
    scale: f32,
    gap: f32,
    rotation: Rotation,
}

impl CanvasLayout {
    /// Lay out `page_sizes` with no rotation (the standard unrotated case).
    ///
    /// `gap` and `padding` are expressed in canvas units and let callers tune
    /// spacing; pass [`DEFAULT_GAP`] / [`DEFAULT_PADDING`] for the standard look.
    /// Fit zoom modes are resolved against the widest row of pages so they are
    /// fully visible; the resulting scale is clamped by [`Zoom::effective_scale`].
    /// Non-finite or non-positive page extents are sanitized to one point so the
    /// function never divides by zero or produces `NaN` geometry.
    pub fn new(
        mode: ViewMode,
        zoom: Zoom,
        viewport: CanvasSize,
        page_sizes: &[PageSize],
        gap: f32,
        padding: f32,
    ) -> Self {
        Self::with_page_rotations(
            mode,
            zoom,
            viewport,
            page_sizes,
            gap,
            padding,
            Rotation::Deg0,
            &[],
        )
    }

    /// Lay out pages with a per-tab view rotation applied to every page.
    ///
    /// The view rotation is independent of each page's intrinsic `/Rotate`;
    /// [`Self::with_page_rotations`] supplies both when they are known. The
    /// effective rotation used for geometry is
    /// `page_rotation.combine(view_rotation)`.
    pub fn with_rotation(
        mode: ViewMode,
        zoom: Zoom,
        viewport: CanvasSize,
        page_sizes: &[PageSize],
        gap: f32,
        padding: f32,
        view_rotation: Rotation,
    ) -> Self {
        Self::with_page_rotations(
            mode,
            zoom,
            viewport,
            page_sizes,
            gap,
            padding,
            view_rotation,
            &[],
        )
    }

    /// Lay out pages with the standard gap and padding and a view rotation.
    pub fn standard_rotated(
        mode: ViewMode,
        zoom: Zoom,
        viewport: CanvasSize,
        page_sizes: &[PageSize],
        view_rotation: Rotation,
    ) -> Self {
        Self::with_rotation(
            mode,
            zoom,
            viewport,
            page_sizes,
            DEFAULT_GAP,
            DEFAULT_PADDING,
            view_rotation,
        )
    }

    /// Lay out pages with explicit per-page `/Rotate` values and a view rotation.
    ///
    /// `page_rotations` is indexed by page; missing entries default to
    /// [`Rotation::Deg0`]. The intrinsic rotation belongs to the PDF and is
    /// never mutated by a view command, so view and page rotation stay
    /// independent while their sum drives the geometry.
    // This low-level constructor intentionally mirrors `new`'s geometry knobs
    // and adds the two rotation inputs; callers use the shorter wrappers.
    #[allow(clippy::too_many_arguments)]
    pub fn with_page_rotations(
        mode: ViewMode,
        zoom: Zoom,
        viewport: CanvasSize,
        page_sizes: &[PageSize],
        gap: f32,
        padding: f32,
        view_rotation: Rotation,
        page_rotations: &[Rotation],
    ) -> Self {
        let gap = finite_non_negative(gap);
        let padding = finite_non_negative(padding);
        let sizes: Vec<PageSize> = page_sizes
            .iter()
            .map(|size| {
                PageSize::new(
                    sanitize_extent(size.width_pt),
                    sanitize_extent(size.height_pt),
                )
            })
            .collect();
        let effective: Vec<Rotation> = (0..sizes.len())
            .map(|index| {
                let intrinsic = page_rotations.get(index).copied().unwrap_or(Rotation::Deg0);
                intrinsic.combine(view_rotation)
            })
            .collect();
        let displayed: Vec<PageSize> = sizes
            .iter()
            .zip(effective.iter())
            .map(|(size, rotation)| effective_page_size(*size, *rotation))
            .collect();
        // Rotation is composed first, then zoom is resolved against the rotated
        // extents, so fit modes fit what is actually displayed.
        let scale = resolve_scale(mode, zoom, &displayed, &viewport);
        let page_break = if mode.is_continuous() {
            0.0
        } else {
            finite_non_negative(viewport.height)
        };

        let per_row = if mode.is_two_page() { 2 } else { 1 };
        let rows: Vec<&[PageSize]> = displayed.chunks(per_row.max(1)).collect();

        let row_metrics: Vec<(f32, f32)> = rows
            .iter()
            .map(|row| {
                let width: f32 = row.iter().map(|size| size.width_pt * scale).sum::<f32>()
                    + gap * (row.len().saturating_sub(1) as f32);
                let height = row
                    .iter()
                    .map(|size| size.height_pt * scale)
                    .fold(0.0_f32, f32::max);
                (width, height)
            })
            .collect();

        let inner_width = row_metrics
            .iter()
            .map(|(width, _)| *width)
            .fold(0.0_f32, f32::max);
        let content_width = inner_width + padding * 2.0;

        let mut pages = Vec::with_capacity(sizes.len());
        let mut y = padding;
        let mut page_index = 0usize;

        for (row, (row_width, row_height)) in rows.iter().zip(row_metrics.iter()) {
            let mut x = padding + (inner_width - row_width) * 0.5;
            for size in row.iter() {
                let scaled_width = size.width_pt * scale;
                let scaled_height = size.height_pt * scale;
                let y_offset = (row_height - scaled_height) * 0.5;
                let intrinsic = sizes[page_index];
                pages.push(PageGeometry {
                    page: page_index,
                    rect: CanvasRect::new(x, y + y_offset, scaled_width, scaled_height),
                    scale,
                    width_pt: intrinsic.width_pt,
                    height_pt: intrinsic.height_pt,
                    page_rotation: page_rotations
                        .get(page_index)
                        .copied()
                        .unwrap_or(Rotation::Deg0),
                    rotation: effective[page_index],
                });
                x += scaled_width + gap;
                page_index = page_index.saturating_add(1);
            }
            y += row_height + gap + page_break;
        }

        let content_height = if row_metrics.is_empty() {
            0.0
        } else {
            let row_heights: f32 = row_metrics.iter().map(|(_, height)| *height).sum();
            let breaks = gap * (row_metrics.len().saturating_sub(1) as f32)
                + page_break * (row_metrics.len().saturating_sub(1) as f32);
            row_heights + breaks + padding * 2.0
        };

        Self {
            pages,
            content_width,
            content_height,
            viewport,
            mode,
            scale,
            gap,
            rotation: view_rotation,
        }
    }

    /// Lay out pages with the standard gap and padding.
    pub fn standard(
        mode: ViewMode,
        zoom: Zoom,
        viewport: CanvasSize,
        page_sizes: &[PageSize],
    ) -> Self {
        Self::new(
            mode,
            zoom,
            viewport,
            page_sizes,
            DEFAULT_GAP,
            DEFAULT_PADDING,
        )
    }

    /// The laid-out pages, in page-index order.
    pub fn pages(&self) -> &[PageGeometry] {
        &self.pages
    }

    /// The geometry of one page by index, if present.
    pub fn page(&self, index: usize) -> Option<&PageGeometry> {
        self.pages.get(index)
    }

    /// The number of laid-out pages.
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// The resolved point-to-canvas-unit scale.
    pub fn scale(&self) -> f32 {
        self.scale
    }

    /// The view mode used to build this layout.
    pub fn mode(&self) -> ViewMode {
        self.mode
    }

    /// The gap between pages, in canvas units.
    pub fn gap(&self) -> f32 {
        self.gap
    }

    /// The per-tab view rotation used to build this layout.
    ///
    /// This is the *view* rotation only; each page's effective rotation
    /// (view + intrinsic `/Rotate`) is available on [`PageGeometry::rotation`].
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The viewport this layout was built for.
    pub fn viewport(&self) -> CanvasSize {
        self.viewport
    }

    /// Total laid-out content width, in canvas units.
    pub fn content_width(&self) -> f32 {
        self.content_width
    }

    /// Total laid-out content height, in canvas units.
    pub fn content_height(&self) -> f32 {
        self.content_height
    }

    /// Whether this layout contains no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Determine what `point` (in unscrolled canvas units) is over.
    pub fn page_at(&self, point: CanvasPoint) -> CanvasHit {
        for page in &self.pages {
            if page.rect.contains(point) {
                return CanvasHit::Page(page.page);
            }
        }
        let within_content = point.x >= 0.0
            && point.x < self.content_width
            && point.y >= 0.0
            && point.y < self.content_height;
        if within_content {
            CanvasHit::Gap
        } else {
            CanvasHit::Outside
        }
    }

    /// Map `point` to `(page_index, x_pt, y_pt)` in page-local coordinates.
    ///
    /// The returned point has its origin at the page's top-left corner, y
    /// increasing downward, one unit per PDF point. The canvas point is first
    /// expressed in the page's displayed coordinate frame and then mapped back
    /// through the *inverse* of the page's effective rotation, so hit testing
    /// works at every quarter turn. Coordinates are clamped to the page's
    /// intrinsic width/height. Returns `None` when the point is not over a page.
    pub fn page_point(&self, point: CanvasPoint) -> Option<(usize, f32, f32)> {
        let index = self.page_at(point).page_index()?;
        let page = self.pages.get(index)?;
        // Displayed frame in intrinsic points: width/height swap at a quarter
        // turn, matching `page.rect` (both divided by the uniform scale).
        let dx = ((point.x - page.rect.x) / page.scale).clamp(0.0, page.rect.width / page.scale);
        let dy = ((point.y - page.rect.y) / page.scale).clamp(0.0, page.rect.height / page.scale);
        let (x_pt, y_pt) = unrotate_point(dx, dy, page.width_pt, page.height_pt, page.rotation);
        Some((
            index,
            x_pt.clamp(0.0, page.width_pt),
            y_pt.clamp(0.0, page.height_pt),
        ))
    }

    /// Map a page-local rectangle into an unscrolled canvas rectangle.
    ///
    /// `local` is expressed in intrinsic page points with a top-left origin and
    /// `y` increasing downward (the same frame used by text spans and find
    /// matches). The rectangle's four corners are transformed by the page's
    /// effective rotation, scaled, and offset by the page's laid-out position.
    /// The result is the axis-aligned bounding box in canvas units, suitable
    /// for drawing selection highlights and other overlays. Returns `None` when
    /// `page` is not part of this layout.
    pub fn page_rect_to_canvas(&self, page: usize, local: CanvasRect) -> Option<CanvasRect> {
        let geometry = self.pages.get(page)?;
        let corners = [
            (local.left(), local.top()),
            (local.right(), local.top()),
            (local.right(), local.bottom()),
            (local.left(), local.bottom()),
        ];
        let mut min_x = f32::INFINITY;
        let mut min_y = f32::INFINITY;
        let mut max_x = f32::NEG_INFINITY;
        let mut max_y = f32::NEG_INFINITY;
        for (x, y) in corners {
            let (dx, dy) = rotate_point(
                x,
                y,
                geometry.width_pt,
                geometry.height_pt,
                geometry.rotation,
            );
            let cx = geometry.rect.x + dx * geometry.scale;
            let cy = geometry.rect.y + dy * geometry.scale;
            min_x = min_x.min(cx);
            min_y = min_y.min(cy);
            max_x = max_x.max(cx);
            max_y = max_y.max(cy);
        }
        Some(CanvasRect::new(min_x, min_y, max_x - min_x, max_y - min_y))
    }

    /// The maximum scroll offset in each axis.
    ///
    /// Both components are at least zero: a viewport larger than the content
    /// cannot scroll.
    pub fn max_scroll(&self) -> CanvasPoint {
        CanvasPoint::new(
            (self.content_width - self.viewport.width).max(0.0),
            (self.content_height - self.viewport.height).max(0.0),
        )
    }

    /// Clamp a scroll offset into the laid-out content bounds.
    pub fn clamp_scroll(&self, x: f32, y: f32) -> CanvasPoint {
        let max = self.max_scroll();
        // `x`/`y` may be non-finite; sanitize before clamping.
        let sx = if x.is_finite() { x } else { 0.0 };
        let sy = if y.is_finite() { y } else { 0.0 };
        CanvasPoint::new(sx.clamp(0.0, max.x), sy.clamp(0.0, max.y))
    }
}

/// Resolve a zoom policy to a single concrete scale used by every page.
fn resolve_scale(mode: ViewMode, zoom: Zoom, sizes: &[PageSize], viewport: &CanvasSize) -> f32 {
    if let Some(scale) = zoom.scale() {
        // A custom zoom is already clamped by `Zoom`; use it directly so the
        // exact factor (e.g. 2.0) is preserved without rounding drift.
        return scale;
    }

    let per_row = if mode.is_two_page() { 2 } else { 1 };
    let mut reference_width = 0.0_f32;
    let mut reference_height = 0.0_f32;
    for row in sizes.chunks(per_row.max(1)) {
        let row_width: f32 = row.iter().map(|size| size.width_pt).sum();
        reference_width = reference_width.max(row_width);
        for size in row {
            reference_height = reference_height.max(size.height_pt);
        }
    }

    let vw = finite_non_negative(viewport.width);
    let vh = finite_non_negative(viewport.height);
    let fit_width = if reference_width > 0.0 {
        vw / reference_width
    } else {
        DEFAULT_ZOOM
    };
    let fit_page = if reference_height > 0.0 {
        fit_width.min(vh / reference_height)
    } else {
        fit_width
    };
    zoom.effective_scale(fit_width, fit_page)
}

/// Return the page extents as displayed after applying `rotation`.
///
/// Quarter turns exchange width and height; half/full turns keep them. This is
/// the first step of the documented rotation-then-zoom composition order.
pub fn effective_page_size(size: PageSize, rotation: Rotation) -> PageSize {
    if rotation.swaps_dimensions() {
        PageSize::new(size.height_pt, size.width_pt)
    } else {
        size
    }
}

/// Rotate a point from intrinsic page coordinates into the displayed frame.
///
/// Inputs are `(x, y)` in intrinsic points with a top-left origin and `y` down;
/// `width`/`height` are the intrinsic extents. The output uses the same origin
/// convention in the (possibly swapped) displayed frame.
pub fn rotate_point(x: f32, y: f32, width: f32, height: f32, rotation: Rotation) -> (f32, f32) {
    match rotation {
        Rotation::Deg0 => (x, y),
        Rotation::Deg90 => (height - y, x),
        Rotation::Deg180 => (width - x, height - y),
        Rotation::Deg270 => (y, width - x),
    }
}

/// Inverse of [`rotate_point`]: map a displayed point back to intrinsic page
/// coordinates. Used by hit testing in [`CanvasLayout::page_point`].
pub fn unrotate_point(dx: f32, dy: f32, width: f32, height: f32, rotation: Rotation) -> (f32, f32) {
    match rotation {
        Rotation::Deg0 => (dx, dy),
        Rotation::Deg90 => (dy, height - dx),
        Rotation::Deg180 => (width - dx, height - dy),
        Rotation::Deg270 => (width - dy, dx),
    }
}

/// Texture UV coordinates for the four corners of a rotated page image.
///
/// The returned quadruple is ordered `(top-left, top-right, bottom-right,
/// bottom-left)` of the *displayed* rectangle, with each entry an `(u, v)` pair
/// in the texture's own top-left-origin frame. A renderer building a textured
/// mesh can assign these UVs to the corresponding rectangle corners so the
/// raster appears rotated instead of stretched into a swapped page rectangle.
pub fn uv_quad_for_rotation(rotation: Rotation) -> [(f32, f32); 4] {
    match rotation {
        Rotation::Deg0 => [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)],
        Rotation::Deg90 => [(0.0, 1.0), (0.0, 0.0), (1.0, 0.0), (1.0, 1.0)],
        Rotation::Deg180 => [(1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0)],
        Rotation::Deg270 => [(1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)],
    }
}

/// Return a finite, non-negative value, mapping bad inputs to zero.
fn finite_non_negative(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        0.0
    }
}

/// Sanitize a page extent to a finite positive value (at least one point).
fn sanitize_extent(value: f32) -> f32 {
    if value.is_finite() && value > 0.0 {
        value
    } else {
        1.0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn letter_pages(count: usize) -> Vec<PageSize> {
        (0..count).map(|_| PageSize::new(612.0, 792.0)).collect()
    }

    fn custom(scale: f32) -> Zoom {
        Zoom::custom(scale)
    }

    fn layout(mode: ViewMode, pages: &[PageSize]) -> CanvasLayout {
        CanvasLayout::standard(mode, custom(1.0), CanvasSize::new(800.0, 600.0), pages)
    }

    #[test]
    fn layout_ordering_for_all_four_modes() {
        for mode in [
            ViewMode::SinglePage,
            ViewMode::Continuous,
            ViewMode::TwoPage,
            ViewMode::TwoPageContinuous,
        ] {
            let pages = layout(mode, &letter_pages(5));
            assert_eq!(pages.page_count(), 5, "{mode}");
            let indices: Vec<usize> = pages.pages().iter().map(|p| p.page).collect();
            assert_eq!(indices, vec![0, 1, 2, 3, 4], "ordering in {mode}");
            // Every page lies fully within the content bounds.
            for page in pages.pages() {
                assert!(page.rect.x >= 0.0, "{mode} page {}", page.page);
                assert!(page.rect.right() <= pages.content_width() + 0.001, "{mode}");
            }
        }
    }

    #[test]
    fn two_page_spread_pairing_into_rows() {
        for mode in [ViewMode::TwoPage, ViewMode::TwoPageContinuous] {
            let pages = layout(mode, &letter_pages(5));
            let first = pages.page(0).expect("page 0").rect;
            let second = pages.page(1).expect("page 1").rect;
            let third = pages.page(2).expect("page 2").rect;
            let fourth = pages.page(3).expect("page 3").rect;
            let fifth = pages.page(4).expect("page 4").rect;
            // 0 and 1 share a row (same top), 2 and 3 share the next row.
            assert!((first.y - second.y).abs() < 0.001, "{mode}");
            assert!(third.y > first.y, "{mode}");
            assert!((third.y - fourth.y).abs() < 0.001, "{mode}");
            assert!(fifth.y > third.y, "{mode}");
            // Side-by-side pages do not overlap horizontally.
            assert!(second.x > first.x, "{mode}");
            assert!(
                (second.x - first.right() - pages.gap()).abs() < 0.001,
                "{mode}"
            );
        }
        // Single-page modes never pair: every page starts a new row.
        let pages = layout(ViewMode::Continuous, &letter_pages(3));
        assert!(pages.page(1).expect("p1").rect.y > pages.page(0).expect("p0").rect.y);
    }

    #[test]
    fn single_page_modes_use_a_page_break_between_rows() {
        let continuous = layout(ViewMode::Continuous, &letter_pages(3));
        let paged = layout(ViewMode::SinglePage, &letter_pages(3));
        let continuous_step =
            continuous.page(1).expect("p1").rect.y - continuous.page(0).expect("p0").rect.y;
        let paged_step = paged.page(1).expect("p1").rect.y - paged.page(0).expect("p0").rect.y;
        // The paged variant inserts one viewport height between screens.
        assert!((paged_step - continuous_step - 600.0).abs() < 0.001);
    }

    #[test]
    fn zoom_scaling_multiplies_page_extent() {
        let pages = layout(ViewMode::Continuous, &letter_pages(1));
        let r1 = pages.page(0).expect("p0").rect;
        assert!((r1.width - 612.0).abs() < 0.001);
        assert!((pages.scale() - 1.0).abs() < 0.001);

        let doubled = CanvasLayout::standard(
            ViewMode::Continuous,
            custom(2.0),
            CanvasSize::new(800.0, 600.0),
            &letter_pages(1),
        );
        let r2 = doubled.page(0).expect("p0").rect;
        assert!((r2.width - 1224.0).abs() < 0.001, "{}", r2.width);
        assert!((doubled.scale() - 2.0).abs() < 0.001);
    }

    #[test]
    fn fit_modes_resolve_against_the_widest_row() {
        // One page 612pt wide in an 612pt-wide viewport: fit-width ~= 1.0.
        let fit = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::FitWidth,
            CanvasSize::new(612.0, 792.0),
            &[PageSize::new(612.0, 792.0)],
        );
        assert!((fit.scale() - 1.0).abs() < 0.001, "{}", fit.scale());

        let half = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::FitWidth,
            CanvasSize::new(306.0, 792.0),
            &[PageSize::new(612.0, 792.0)],
        );
        assert!((half.scale() - 0.5).abs() < 0.001, "{}", half.scale());

        // Fit-page also respects height.
        let fit_page = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::FitPage,
            CanvasSize::new(1000.0, 396.0),
            &[PageSize::new(612.0, 792.0)],
        );
        assert!(
            (fit_page.scale() - 0.5).abs() < 0.001,
            "{}",
            fit_page.scale()
        );
    }

    #[test]
    fn scroll_clamping_is_bounded_and_finite() {
        let pages = layout(ViewMode::Continuous, &letter_pages(3));
        let max = pages.max_scroll();
        assert!(max.x >= 0.0 && max.y >= 0.0);
        assert_eq!(pages.clamp_scroll(-50.0, -50.0), CanvasPoint::new(0.0, 0.0));
        let clamped = pages.clamp_scroll(max.x + 1000.0, max.y + 1000.0);
        assert!((clamped.x - max.x).abs() < 0.001);
        assert!((clamped.y - max.y).abs() < 0.001);
        assert_eq!(
            pages.clamp_scroll(f32::NAN, f32::INFINITY),
            CanvasPoint::new(0.0, 0.0)
        );
    }

    #[test]
    fn page_hit_testing_at_edges_is_deterministic() {
        let pages = layout(ViewMode::Continuous, &letter_pages(2));
        let first = pages.page(0).expect("p0").rect;
        let inside = CanvasPoint::new(first.x + 1.0, first.y + 1.0);
        let left_edge = CanvasPoint::new(first.x, first.y);
        let right_edge = CanvasPoint::new(first.right(), first.y + 1.0);
        let bottom_edge = CanvasPoint::new(first.x + 1.0, first.bottom());
        assert_eq!(pages.page_at(inside), CanvasHit::Page(0));
        assert_eq!(
            pages.page_at(left_edge),
            CanvasHit::Page(0),
            "left/top are inclusive"
        );
        assert_eq!(
            pages.page_at(right_edge),
            CanvasHit::Gap,
            "right edge is exclusive"
        );
        assert_eq!(
            pages.page_at(bottom_edge),
            CanvasHit::Gap,
            "bottom edge is exclusive"
        );
        assert_eq!(
            pages.page_at(CanvasPoint::new(-1.0, -1.0)),
            CanvasHit::Outside
        );
        assert!(CanvasHit::Page(2).is_page());
        assert_eq!(CanvasHit::Gap.page_index(), None);
    }

    #[test]
    fn point_to_page_coordinate_mapping() {
        let pages = layout(ViewMode::Continuous, &letter_pages(1));
        let rect = pages.page(0).expect("p0").rect;
        // Center of the page maps to the center in page points.
        let center = CanvasPoint::new(rect.x + rect.width * 0.5, rect.y + rect.height * 0.5);
        let (index, x_pt, y_pt) = pages.page_point(center).expect("on page");
        assert_eq!(index, 0);
        assert!((x_pt - 306.0).abs() < 0.001, "{x_pt}");
        assert!((y_pt - 396.0).abs() < 0.001, "{y_pt}");
        // Off-page points map to None; out-of-bounds points clamp.
        assert!(pages.page_point(CanvasPoint::new(-5.0, -5.0)).is_none());
        assert!(pages.page_point(CanvasPoint::new(rect.x, rect.y)).is_some());
        let clamped = pages.page_point(CanvasPoint::new(rect.right() - 0.01, rect.bottom() - 0.01));
        assert!(clamped.is_some());
    }

    #[test]
    fn degenerate_inputs_do_not_panic_or_produce_nan() {
        let nested = CanvasLayout::standard(
            ViewMode::TwoPageContinuous,
            Zoom::FitPage,
            CanvasSize::new(f32::NAN, f32::INFINITY),
            &[PageSize::new(0.0, -1.0), PageSize::new(f32::NAN, 792.0)],
        );
        assert!(!nested.scale().is_nan());
        for page in nested.pages() {
            assert!(page.rect.width.is_finite());
            assert!(page.rect.height.is_finite());
        }
        let empty = layout(ViewMode::Continuous, &[]);
        assert!(empty.is_empty());
        assert_eq!(
            empty.page_at(CanvasPoint::new(0.0, 0.0)),
            CanvasHit::Outside
        );
    }

    // ---- T-0083 rotation-aware geometry ---------------------------------

    fn portrait() -> PageSize {
        PageSize::new(200.0, 400.0)
    }

    fn rotated_layout(rotation: Rotation) -> CanvasLayout {
        CanvasLayout::standard_rotated(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(1000.0, 1000.0),
            &[portrait()],
            rotation,
        )
    }

    #[test]
    fn quarter_turns_swap_layout_extents() {
        let flat = rotated_layout(Rotation::Deg0);
        let turned = rotated_layout(Rotation::Deg90);
        let half = rotated_layout(Rotation::Deg180);
        let turned_back = rotated_layout(Rotation::Deg270);

        let flat_rect = flat.page(0).expect("p0").rect;
        let turned_rect = turned.page(0).expect("p0").rect;
        // 90/270 show the page on its side: width/height exchange.
        assert!((flat_rect.width - 200.0).abs() < 0.001);
        assert!((flat_rect.height - 400.0).abs() < 0.001);
        assert!((turned_rect.width - 400.0).abs() < 0.001);
        assert!((turned_rect.height - 200.0).abs() < 0.001);
        assert!((turned_back.page(0).expect("p0").rect.width - 400.0).abs() < 0.001);
        // 0/180 keep portrait proportions.
        let half_rect = half.page(0).expect("p0").rect;
        assert!((half_rect.width - 200.0).abs() < 0.001);
        assert!((half_rect.height - 400.0).abs() < 0.001);
        // Intrinsic dimensions are never mutated by the view rotation.
        for layout in [flat, turned, half, turned_back] {
            let geometry = layout.page(0).expect("p0");
            assert!((geometry.width_pt - 200.0).abs() < 0.001);
            assert!((geometry.height_pt - 400.0).abs() < 0.001);
        }
    }

    #[test]
    fn hit_test_transform_is_correct_at_every_rotation() {
        // Round-trip a page-local point through the display transform and back
        // through `page_point` for each quarter turn.
        let intrinsic = (50.0_f32, 300.0_f32);
        for rotation in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let layout = rotated_layout(rotation);
            let rect = layout.page(0).expect("p0").rect;
            let (dx, dy) = rotate_point(intrinsic.0, intrinsic.1, 200.0, 400.0, rotation);
            let canvas_point = CanvasPoint::new(rect.x + dx, rect.y + dy);
            assert_eq!(
                layout.page_at(canvas_point),
                CanvasHit::Page(0),
                "rotated point lands on the page at {rotation:?}"
            );
            let (page, x_pt, y_pt) = layout.page_point(canvas_point).expect("on page");
            assert_eq!(page, 0);
            assert!((x_pt - intrinsic.0).abs() < 0.001, "{rotation:?}: {x_pt}");
            assert!((y_pt - intrinsic.1).abs() < 0.001, "{rotation:?}: {y_pt}");
        }
    }

    #[test]
    fn selection_rect_maps_forward_under_rotation() {
        let local = CanvasRect::new(50.0, 100.0, 20.0, 30.0);
        for rotation in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            let layout = rotated_layout(rotation);
            let rect = layout.page(0).map(|p| p.rect).expect("geometry");
            // Compare against the explicit four-corner transform.
            let mut expected = [0.0_f32; 8];
            let corners = [
                (local.left(), local.top()),
                (local.right(), local.top()),
                (local.right(), local.bottom()),
                (local.left(), local.bottom()),
            ];
            for (index, (x, y)) in corners.iter().enumerate() {
                let (dx, dy) = rotate_point(*x, *y, 200.0, 400.0, rotation);
                expected[index * 2] = rect.x + dx * layout.scale();
                expected[index * 2 + 1] = rect.y + dy * layout.scale();
            }
            let min_x = expected[0]
                .min(expected[2])
                .min(expected[4])
                .min(expected[6]);
            let max_x = expected[0]
                .max(expected[2])
                .max(expected[4])
                .max(expected[6]);
            let min_y = expected[1]
                .min(expected[3])
                .min(expected[5])
                .min(expected[7]);
            let max_y = expected[1]
                .max(expected[3])
                .max(expected[5])
                .max(expected[7]);
            let mapped = layout.page_rect_to_canvas(0, local).expect("page 0");
            assert!((mapped.x - min_x).abs() < 0.001, "{rotation:?}");
            assert!((mapped.y - min_y).abs() < 0.001, "{rotation:?}");
            assert!(
                (mapped.width - (max_x - min_x)).abs() < 0.001,
                "{rotation:?}"
            );
            assert!(
                (mapped.height - (max_y - min_y)).abs() < 0.001,
                "{rotation:?}"
            );
            // Quarter turns exchange the overlay's width and height.
            if rotation.swaps_dimensions() {
                assert!((mapped.width - 30.0).abs() < 0.001, "{rotation:?}");
                assert!((mapped.height - 20.0).abs() < 0.001, "{rotation:?}");
            } else {
                assert!((mapped.width - 20.0).abs() < 0.001, "{rotation:?}");
                assert!((mapped.height - 30.0).abs() < 0.001, "{rotation:?}");
            }
        }
        assert!(
            rotated_layout(Rotation::Deg0)
                .page_rect_to_canvas(9, local)
                .is_none()
        );
    }

    #[test]
    fn fit_width_uses_rotated_page_extent() {
        let viewport = CanvasSize::new(200.0, 1000.0);
        let flat = CanvasLayout::standard_rotated(
            ViewMode::Continuous,
            Zoom::FitWidth,
            viewport,
            &[portrait()],
            Rotation::Deg0,
        );
        let turned = CanvasLayout::standard_rotated(
            ViewMode::Continuous,
            Zoom::FitWidth,
            viewport,
            &[portrait()],
            Rotation::Deg90,
        );
        // Unrotated fits 200pt into 200px => 1.0; rotated fits 400pt => 0.5.
        assert!((flat.scale() - 1.0).abs() < 0.001, "{}", flat.scale());
        assert!((turned.scale() - 0.5).abs() < 0.001, "{}", turned.scale());
    }

    #[test]
    fn scroll_clamp_tracks_rotated_content_height() {
        let sizes = [portrait(), portrait()];
        let viewport = CanvasSize::new(500.0, 300.0);
        let flat = CanvasLayout::standard_rotated(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            viewport,
            &sizes,
            Rotation::Deg0,
        );
        let turned = CanvasLayout::standard_rotated(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            viewport,
            &sizes,
            Rotation::Deg90,
        );
        // Rotated pages are shorter, so the content scroll range shrinks.
        assert!(turned.max_scroll().y < flat.max_scroll().y);
        let max = turned.max_scroll();
        let clamped = turned.clamp_scroll(max.x + 10_000.0, max.y + 10_000.0);
        assert!((clamped.x - max.x).abs() < 0.001);
        assert!((clamped.y - max.y).abs() < 0.001);
        assert_eq!(
            turned.clamp_scroll(f32::NAN, f32::INFINITY),
            CanvasPoint::new(0.0, 0.0)
        );
    }

    #[test]
    fn page_rotate_is_independent_and_composes_with_view() {
        let intrinsic = Rotation::Deg90;
        let view0 = CanvasLayout::with_page_rotations(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(1000.0, 1000.0),
            &[portrait()],
            DEFAULT_GAP,
            DEFAULT_PADDING,
            Rotation::Deg0,
            &[intrinsic],
        );
        let view90 = CanvasLayout::with_page_rotations(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(1000.0, 1000.0),
            &[portrait()],
            DEFAULT_GAP,
            DEFAULT_PADDING,
            Rotation::Deg90,
            &[intrinsic],
        );
        let a = view0.page(0).expect("p0");
        let b = view90.page(0).expect("p0");
        // The intrinsic rotation is preserved, never overwritten by the view.
        assert_eq!(a.page_rotation, intrinsic);
        assert_eq!(b.page_rotation, intrinsic);
        assert_eq!(intrinsic, Rotation::Deg90);
        // Effective rotations compose additively.
        assert_eq!(a.rotation, Rotation::Deg90);
        assert_eq!(b.rotation, Rotation::Deg180);
        // Different effective rotations produce different geometry.
        assert!((a.rect.width - b.rect.width).abs() > 1.0);
    }

    #[test]
    fn uv_quad_maps_texture_corners_for_each_rotation() {
        assert_eq!(
            uv_quad_for_rotation(Rotation::Deg0),
            [(0.0, 0.0), (1.0, 0.0), (1.0, 1.0), (0.0, 1.0)]
        );
        assert_eq!(
            uv_quad_for_rotation(Rotation::Deg90),
            [(0.0, 1.0), (0.0, 0.0), (1.0, 0.0), (1.0, 1.0)]
        );
        assert_eq!(
            uv_quad_for_rotation(Rotation::Deg180),
            [(1.0, 1.0), (0.0, 1.0), (0.0, 0.0), (1.0, 0.0)]
        );
        assert_eq!(
            uv_quad_for_rotation(Rotation::Deg270),
            [(1.0, 0.0), (1.0, 1.0), (0.0, 1.0), (0.0, 0.0)]
        );
        // A full rotation cycle returns the original UV quad.
        let mut r = Rotation::Deg0;
        for _ in 0..4 {
            r = r.clockwise();
        }
        assert_eq!(
            uv_quad_for_rotation(r),
            uv_quad_for_rotation(Rotation::Deg0)
        );
    }
}
