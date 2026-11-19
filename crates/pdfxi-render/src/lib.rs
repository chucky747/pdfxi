//! Rendering contracts for PDFXI.
//!
//! This crate freezes the public rendering surface described in
//! `docs/ARCHITECTURE.md` without pulling in a native backend yet. The types
//! here are deliberately backend-agnostic: a later PDFium implementation plugs
//! into the [`Renderer`] trait without changing any caller.
//!
//! Long operations are cooperative. Callers hand a [`CancellationToken`] and a
//! progress callback to [`Renderer::render_page`]; an implementation must check
//! cancellation between steps and report [`Progress`] as work advances. This
//! keeps the UI responsive and lets a closed document stop outstanding work.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A zero-based index identifying one page of a document.
///
/// ```text
/// use pdfxi_render::PageIndex;
///
/// let first = PageIndex::new(0);
/// let third = PageIndex::new(2);
/// assert_eq!(first.get(), 0);
/// assert!(third > first);
/// ```
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageIndex(pub usize);

impl PageIndex {
    /// Create a page index from a zero-based `index`.
    pub const fn new(index: usize) -> Self {
        Self(index)
    }

    /// Return the underlying zero-based page number.
    pub const fn get(self) -> usize {
        self.0
    }
}

impl std::fmt::Display for PageIndex {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        // Pages are shown one-based in user-facing text.
        write!(f, "{}", self.0 + 1)
    }
}

/// A snapshot of how far a long-running operation has advanced.
///
/// `total == 0` means the amount of work is not yet known; in that case
/// [`Progress::fraction`] returns `0.0`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    /// Number of work units completed so far.
    pub completed: usize,
    /// Total number of work units, or `0` when unknown.
    pub total: usize,
}

impl Progress {
    /// Create a progress report from `completed` and `total` work units.
    pub const fn new(completed: usize, total: usize) -> Self {
        Self { completed, total }
    }

    /// Return the completed fraction in `0.0..=1.0`, or `0.0` when unknown.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.completed.min(self.total) as f32) / (self.total as f32)
        }
    }

    /// Return `true` when all known work is finished.
    pub fn is_complete(&self) -> bool {
        self.total > 0 && self.completed >= self.total
    }
}

/// A cheap, cloneable token used to cooperatively cancel an operation.
///
/// All clones share one flag, so a token held by a UI thread can stop work
/// running on a renderer worker. Implementations check [`Self::is_cancelled`]
/// (or [`Self::check`]) between steps and unwind with [`RenderError::Cancelled`].
///
/// ```text
/// use pdfxi_render::{CancellationToken, RenderError};
///
/// let token = CancellationToken::new();
/// assert!(token.check().is_ok());
/// token.cancel();
/// assert_eq!(token.check(), Err(RenderError::Cancelled));
/// ```
#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a token in the not-yet-cancelled state.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. This is idempotent and visible to all clones.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Return `true` once [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Return [`RenderError::Cancelled`] if cancellation has been requested.
    pub fn check(&self) -> Result<(), RenderError> {
        if self.is_cancelled() {
            Err(RenderError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// A rasterized page returned by a [`Renderer`].
///
/// Pixels are stored as tightly packed 8-bit RGBA rows beginning at the top
/// left of the page. An empty page has zero width, zero height, and no pixels.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RenderedPage {
    page: PageIndex,
    width_px: u32,
    height_px: u32,
    pixels: Vec<u8>,
}

impl RenderedPage {
    /// Create a rendered page.
    ///
    /// The `pixels` slice is expected to be `width_px * height_px * 4` bytes
    /// long; the type does not allocate or copy, it only stores the buffer.
    pub fn new(page: PageIndex, width_px: u32, height_px: u32, pixels: Vec<u8>) -> Self {
        Self {
            page,
            width_px,
            height_px,
            pixels,
        }
    }

    /// Create an empty (zero-sized) rendered page.
    pub fn empty(page: PageIndex) -> Self {
        Self::new(page, 0, 0, Vec::new())
    }

    /// Return the page this raster belongs to.
    pub fn page(&self) -> PageIndex {
        self.page
    }

    /// Return the raster width in pixels.
    pub fn width(&self) -> u32 {
        self.width_px
    }

    /// Return the raster height in pixels.
    pub fn height(&self) -> u32 {
        self.height_px
    }

    /// Return the tightly packed RGBA pixel buffer.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Return `true` when the page has no pixel data.
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }
}

/// Errors produced by a [`Renderer`] implementation.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RenderError {
    /// The operation was cancelled through its [`CancellationToken`].
    #[error("rendering was cancelled")]
    Cancelled,
    /// The requested page does not exist in the document.
    #[error("page {0} is out of range")]
    PageOutOfRange(usize),
    /// The requested scale is not usable (not finite or not positive).
    #[error("invalid render scale {0}")]
    InvalidScale(f32),
    /// A backend-specific failure that does not fit a typed variant.
    #[error("render backend error: {0}")]
    Backend(String),
}

/// A backend capable of rasterizing document pages.
///
/// Implementations must:
///
/// 1. validate inputs and return [`RenderError::InvalidScale`] /
///    [`RenderError::PageOutOfRange`] rather than panicking;
/// 2. call `progress` as work advances so callers can display feedback; and
/// 3. check `cancellation` between steps and return
///    [`RenderError::Cancelled`] promptly when it fires.
///
/// The trait is object-safe, so callers can hold `&dyn Renderer`.
pub trait Renderer {
    /// Rasterize `page` at the given `scale` (1.0 is 72 DPI).
    ///
    /// `progress` receives increasing [`Progress`] reports; `cancellation`
    /// may stop the operation at any point.
    fn render_page(
        &self,
        page: PageIndex,
        scale: f32,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<RenderedPage, RenderError>;

    /// Return the number of pages available to this renderer.
    fn page_count(&self) -> Result<usize, RenderError>;
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic renderer used to exercise the contract.
    ///
    /// It exposes `page_count` synthetic pages, each a `size x size` RGBA
    /// buffer filled with a constant byte, and deliberately checks
    /// cancellation at each report point.
    struct MockRenderer {
        page_count: usize,
        side: u32,
    }

    impl MockRenderer {
        fn new(page_count: usize) -> Self {
            Self {
                page_count,
                side: 2,
            }
        }
    }

    impl Renderer for MockRenderer {
        fn render_page(
            &self,
            page: PageIndex,
            scale: f32,
            cancellation: &CancellationToken,
            progress: &mut dyn FnMut(Progress),
        ) -> Result<RenderedPage, RenderError> {
            cancellation.check()?;
            if !scale.is_finite() || scale <= 0.0 {
                return Err(RenderError::InvalidScale(scale));
            }
            if page.get() >= self.page_count {
                return Err(RenderError::PageOutOfRange(page.get()));
            }

            // Report a two-stage render, checking cancellation between stages.
            progress(Progress::new(0, 2));
            cancellation.check()?;
            let side = self.side;
            let pixels = vec![0x80u8; side as usize * side as usize * 4];
            progress(Progress::new(1, 2));
            cancellation.check()?;
            progress(Progress::new(2, 2));
            Ok(RenderedPage::new(page, side, side, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    #[test]
    fn mock_renderer_produces_expected_raster_and_progress() {
        let renderer = MockRenderer::new(3);
        let mut reports = Vec::new();
        let page = renderer
            .render_page(
                PageIndex::new(1),
                1.0,
                &CancellationToken::new(),
                &mut |p| reports.push(p),
            )
            .expect("render succeeds");

        assert_eq!(renderer.page_count(), Ok(3));
        assert_eq!(page.page(), PageIndex::new(1));
        assert_eq!((page.width(), page.height()), (2, 2));
        assert_eq!(page.pixels().len(), 16);
        assert!(!page.is_empty());
        assert_eq!(
            reports,
            vec![
                Progress::new(0, 2),
                Progress::new(1, 2),
                Progress::new(2, 2)
            ]
        );
        assert!(reports.last().is_some_and(Progress::is_complete));
    }

    #[test]
    fn cancellation_stops_render_before_pixels_are_built() {
        let renderer = MockRenderer::new(3);
        let token = CancellationToken::new();
        token.cancel();
        let mut reports = Vec::new();
        let result = renderer.render_page(PageIndex::new(0), 1.0, &token, &mut |p| reports.push(p));

        assert_eq!(result, Err(RenderError::Cancelled));
        assert!(
            reports.is_empty(),
            "cancelled render must not report progress"
        );
    }

    #[test]
    fn empty_document_and_out_of_range_pages_are_rejected() {
        let empty = MockRenderer::new(0);
        assert_eq!(empty.page_count(), Ok(0));
        let result = empty.render_page(
            PageIndex::new(0),
            1.0,
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert_eq!(result, Err(RenderError::PageOutOfRange(0)));

        let blank = RenderedPage::empty(PageIndex::new(0));
        assert!(blank.is_empty());
        assert_eq!((blank.width(), blank.height()), (0, 0));
    }

    #[test]
    fn invalid_scale_is_rejected() {
        let renderer = MockRenderer::new(1);
        for scale in [0.0, -1.0, f32::INFINITY, f32::NEG_INFINITY] {
            let result = renderer.render_page(
                PageIndex::new(0),
                scale,
                &CancellationToken::new(),
                &mut |_| {},
            );
            assert_eq!(result, Err(RenderError::InvalidScale(scale)));
        }
        // NaN is never equal to itself, so compare the variant instead.
        let nan_result = renderer.render_page(
            PageIndex::new(0),
            f32::NAN,
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert!(matches!(nan_result, Err(RenderError::InvalidScale(v)) if v.is_nan()));
    }

    #[test]
    fn progress_fraction_handles_unknown_and_complete_work() {
        let unknown = Progress::new(0, 0);
        assert_eq!(unknown.fraction(), 0.0);
        assert!(!unknown.is_complete());

        let half = Progress::new(1, 2);
        assert!((half.fraction() - 0.5).abs() < f32::EPSILON);
        assert!(!half.is_complete());

        let done = Progress::new(2, 2);
        assert_eq!(done.fraction(), 1.0);
        assert!(done.is_complete());
    }
}
