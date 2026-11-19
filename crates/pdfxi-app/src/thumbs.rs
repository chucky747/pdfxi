//! Dependency-light state for the page-thumbnail navigation pane.
//!
//! This module is the window-free brain of the thumbnail pane. It owns the
//! per-page metadata, the visible-row math for a scrollable vertical list, the
//! active/selected page, and a **bounded** queue of thumbnail-generation
//! requests. It deliberately contains **no `egui` types**: the feature-gated
//! native viewer shell is responsible for drawing tiles and uploading cached
//! RGBA thumbnails into textures. Keeping this state GUI-free means every
//! deterministic behavior — visibility, mapping, dedupe, prefetch, selection,
//! and cache reuse — can be tested with a mock [`Renderer`] and no window.
//!
//! # Layout model
//!
//! The pane is a single vertical list with one page per fixed-height row. Row
//! `r` depicts page `r` and occupies the half-open vertical span
//! `[r * row_height, (r + 1) * row_height)` in pane coordinates. Consequently
//! [`ThumbnailPaneState::page_at_row`] and
//! [`ThumbnailPaneState::row_for_page`] are inverse identity maps with bounds
//! checks; keeping them as explicit methods makes the boundary behavior
//! (zero, last valid index, and out-of-range) testable and gives the GUI a
//! single place to change if the pane ever becomes a grid.
//!
//! # Prefetch and bounded requests
//!
//! [`ThumbnailPaneState::prefetch_range`] widens the visible range by
//! [`ThumbnailPaneState::prefetch_margin_rows`] rows on each side (clamped to
//! the document), so scrolling has thumbnails ready just before they appear.
//! [`ThumbnailPaneState::enqueue_visible`] fills a bounded request queue from
//! that range: already-cached pages and already-queued pages are skipped
//! (dedupe), and when the queue is full the oldest pending request is
//! discarded so the most recently visible pages always win. Requests are
//! turned into real thumbnails by [`ThumbnailPaneState::generate_pending`],
//! which drives `pdfxi-render`'s [`ThumbnailCache`] through the [`Renderer`]
//! trait; the cache makes repeated queries of the same visible region free.

use std::collections::VecDeque;

use pdfxi_render::thumbnail::{Thumbnail, ThumbnailCache, ThumbnailError, ThumbnailSpec};
use pdfxi_render::{CancellationToken, PageIndex, Progress, Renderer};

/// Default height of one thumbnail row in pane units (pixels).
pub const DEFAULT_THUMB_ROW_HEIGHT: f32 = 112.0;

/// Default viewport height used until the GUI reports the real panel height.
///
/// The GUI calls [`ThumbnailPaneState::set_viewport_height`] every frame, so
/// this value only matters for headless use and tests that do not set one.
pub const DEFAULT_THUMB_VIEWPORT_HEIGHT: f32 = 600.0;

/// Default number of extra rows prefetched above and below the visible range.
pub const DEFAULT_PREFETCH_ROWS: usize = 2;

/// Default maximum number of pending thumbnail-generation requests.
pub const DEFAULT_THUMB_CAPACITY: usize = 64;

/// Default maximum number of generated thumbnails retained by the LRU cache.
pub const DEFAULT_THUMBNAIL_CACHE_LIMIT: usize = 128;

/// Alias of [`DEFAULT_THUMB_ROW_HEIGHT`] kept for callers using the shorter name.
pub const DEFAULT_ROW_HEIGHT: f32 = DEFAULT_THUMB_ROW_HEIGHT;

/// Alias of [`DEFAULT_THUMB_VIEWPORT_HEIGHT`] kept for callers using the shorter name.
pub const DEFAULT_VIEWPORT_HEIGHT: f32 = DEFAULT_THUMB_VIEWPORT_HEIGHT;

/// Alias of [`DEFAULT_PREFETCH_ROWS`] with the explicit "margin" name.
pub const DEFAULT_PREFETCH_MARGIN_ROWS: usize = DEFAULT_PREFETCH_ROWS;

/// Alias of [`DEFAULT_THUMB_CAPACITY`] with the explicit "request" name.
pub const DEFAULT_REQUEST_CAPACITY: usize = DEFAULT_THUMB_CAPACITY;

/// Generation status of one page's thumbnail.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ThumbnailStatus {
    /// The page has no cached thumbnail and no pending request.
    Idle,
    /// A generation request for this page is in the bounded queue.
    Queued,
    /// A thumbnail for this page is available in the cache.
    Ready,
}

/// Per-page metadata shown by the thumbnail pane.
///
/// Each entry is created by [`ThumbnailPaneState::reset`] and maps one page to
/// its row and current [`ThumbnailStatus`]. The status is informational: it
/// mirrors the request queue and cache so the GUI can render a spinner or a
/// placeholder while a thumbnail is pending.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ThumbnailMeta {
    /// Zero-based page index depicted by this row.
    pub page: usize,
    /// Zero-based row index (equal to `page` in the single-column list).
    pub row: usize,
    /// Current generation status of this page's thumbnail.
    pub status: ThumbnailStatus,
}

/// An inclusive range of page rows, or an empty range.
///
/// A range is empty when [`Self::is_empty`] is true; in that case
/// [`Self::iter`] yields nothing and the `first`/`last` accessors should be
/// ignored. Empty is encoded internally as `first > last`.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct VisibleRange {
    first: usize,
    last: usize,
}

impl VisibleRange {
    /// Create an empty range.
    pub const fn empty() -> Self {
        Self { first: 1, last: 0 }
    }

    /// Create an inclusive `first..=last` range.
    ///
    /// When `first > last` the range is empty; callers normally use
    /// [`Self::empty`] for that case.
    pub const fn new(first: usize, last: usize) -> Self {
        Self { first, last }
    }

    /// Return `true` when this range contains no pages.
    pub const fn is_empty(&self) -> bool {
        self.first > self.last
    }

    /// The first (topmost) visible page, valid only when not empty.
    pub const fn first_page(&self) -> usize {
        self.first
    }

    /// The last (bottommost) visible page, valid only when not empty.
    pub const fn last_page(&self) -> usize {
        self.last
    }

    /// The first visible row (equal to [`Self::first_page`]).
    pub const fn first_row(&self) -> usize {
        self.first
    }

    /// The last visible row (equal to [`Self::last_page`]).
    pub const fn last_row(&self) -> usize {
        self.last
    }

    /// The number of pages in this range (zero when empty).
    pub fn len(&self) -> usize {
        if self.is_empty() {
            0
        } else {
            self.last - self.first + 1
        }
    }

    /// Return `true` when `page` lies in this range.
    pub fn contains(&self, page: usize) -> bool {
        !self.is_empty() && page >= self.first && page <= self.last
    }

    /// Iterate over the pages in this range in ascending order.
    pub fn iter(&self) -> std::ops::Range<usize> {
        if self.is_empty() {
            0..0
        } else {
            self.first..self.last.saturating_add(1)
        }
    }
}

/// Compute the rows visible for a scroll offset and row height.
///
/// Row `r` is visible when its half-open span `[r * row_height, (r+1) *
/// row_height)` intersects the viewport `[scroll, scroll + viewport_height)`.
/// The result is clamped to `0..page_count`. Non-finite or non-positive values
/// are sanitized: a non-positive `row_height` falls back to
/// [`DEFAULT_THUMB_ROW_HEIGHT`], and a non-positive viewport yields an empty range.
/// An empty document always yields an empty range.
pub fn visible_range_for(
    scroll: f32,
    row_height: f32,
    viewport_height: f32,
    page_count: usize,
) -> VisibleRange {
    if page_count == 0 || !viewport_height.is_finite() || viewport_height <= 0.0 {
        return VisibleRange::empty();
    }
    let height = if row_height.is_finite() && row_height > 0.0 {
        row_height
    } else {
        DEFAULT_THUMB_ROW_HEIGHT
    };
    let top = if scroll.is_finite() && scroll > 0.0 {
        scroll
    } else {
        0.0
    };
    let bottom = top + viewport_height;
    let max_page = page_count - 1;

    // The row containing the top edge is the first visible row. If the top
    // edge is exactly on a boundary, that row's top touches the viewport and
    // it is visible (the viewport is half-open at its bottom only).
    let first = ((top / height).floor() as usize).min(max_page);
    // The row containing the bottom edge is `ceil(bottom / height) - 1`; when
    // the bottom lands exactly on a boundary, that boundary row starts
    // outside the half-open viewport and is excluded.
    let last = ((bottom / height).ceil() as usize)
        .saturating_sub(1)
        .min(max_page);

    if first > last {
        VisibleRange::empty()
    } else {
        VisibleRange::new(first, last)
    }
}

/// Window-free state for the page-thumbnail navigation pane.
///
/// Construct with [`Self::new`] (or reset an existing pane with
/// [`Self::reset`] when a new document is opened), then let the GUI report the
/// panel height and scroll offset through [`Self::set_viewport_height`] and
/// [`Self::set_scroll_offset`]. Call [`Self::enqueue_visible`] and
/// [`Self::generate_pending`] to keep thumbnails ready for the visible region.
#[derive(Debug)]
pub struct ThumbnailPaneState {
    page_count: usize,
    row_height: f32,
    viewport_height: f32,
    scroll_offset: f32,
    prefetch_margin_rows: usize,
    request_capacity: usize,
    active_page: usize,
    selected_page: Option<usize>,
    metadata: Vec<ThumbnailMeta>,
    requests: VecDeque<usize>,
    cache: ThumbnailCache,
}

impl Default for ThumbnailPaneState {
    fn default() -> Self {
        Self::new(0)
    }
}

impl ThumbnailPaneState {
    /// Create pane state for a document with `page_count` pages.
    pub fn new(page_count: usize) -> Self {
        let mut state = Self {
            page_count: 0,
            row_height: DEFAULT_THUMB_ROW_HEIGHT,
            viewport_height: DEFAULT_THUMB_VIEWPORT_HEIGHT,
            scroll_offset: 0.0,
            prefetch_margin_rows: DEFAULT_PREFETCH_ROWS,
            request_capacity: DEFAULT_THUMB_CAPACITY,
            active_page: 0,
            selected_page: None,
            metadata: Vec::new(),
            requests: VecDeque::new(),
            cache: ThumbnailCache::with_limit(DEFAULT_THUMBNAIL_CACHE_LIMIT),
        };
        state.reset(page_count);
        state
    }

    /// Reset all state for a newly opened document with `page_count` pages.
    ///
    /// The scroll offset, active/selected page, request queue, and thumbnail
    /// cache are all cleared so no page from the previous document leaks in.
    pub fn reset(&mut self, page_count: usize) {
        self.page_count = page_count;
        self.scroll_offset = 0.0;
        self.active_page = 0;
        self.selected_page = None;
        self.requests.clear();
        self.cache.clear();
        self.metadata = (0..page_count)
            .map(|page| ThumbnailMeta {
                page,
                row: page,
                status: ThumbnailStatus::Idle,
            })
            .collect();
    }

    /// Set the fixed height of one thumbnail row.
    ///
    /// Non-finite or non-positive values are ignored; the existing height
    /// stays in effect.
    pub fn set_row_height(&mut self, row_height: f32) {
        if row_height.is_finite() && row_height > 0.0 {
            self.row_height = row_height;
        }
    }

    /// Set the visible height of the pane, normally reported by the GUI.
    ///
    /// Non-finite or negative values are ignored.
    pub fn set_viewport_height(&mut self, viewport_height: f32) {
        if viewport_height.is_finite() && viewport_height >= 0.0 {
            self.viewport_height = viewport_height;
        }
    }

    /// Set the number of extra rows prefetched on each side of the visible
    /// range.
    pub fn set_prefetch_margin_rows(&mut self, rows: usize) {
        self.prefetch_margin_rows = rows;
    }

    /// Set the maximum number of pending generation requests.
    ///
    /// A capacity of zero disables queueing entirely.
    pub fn set_request_capacity(&mut self, capacity: usize) {
        self.request_capacity = capacity;
        self.trim_queue();
    }

    /// Set the current vertical scroll offset in pane coordinates.
    ///
    /// Non-finite and negative values are sanitized to zero.
    pub fn set_scroll_offset(&mut self, scroll: f32) {
        self.scroll_offset = if scroll.is_finite() && scroll > 0.0 {
            scroll
        } else {
            0.0
        };
    }

    /// The number of pages in the current document.
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// The fixed height of one thumbnail row.
    pub fn row_height(&self) -> f32 {
        self.row_height
    }

    /// The visible height of the pane.
    pub fn viewport_height(&self) -> f32 {
        self.viewport_height
    }

    /// The current vertical scroll offset.
    pub fn scroll_offset(&self) -> f32 {
        self.scroll_offset
    }

    /// The configured prefetch margin, in rows.
    pub fn prefetch_margin_rows(&self) -> usize {
        self.prefetch_margin_rows
    }

    /// The maximum number of pending generation requests.
    pub fn request_capacity(&self) -> usize {
        self.request_capacity
    }

    /// Map a row index to its page, or `None` when the row is out of range.
    pub fn page_at_row(&self, row: usize) -> Option<usize> {
        if row < self.page_count {
            Some(row)
        } else {
            None
        }
    }

    /// Map a page index to its row, or `None` when the page is out of range.
    pub fn row_for_page(&self, page: usize) -> Option<usize> {
        if page < self.page_count {
            Some(page)
        } else {
            None
        }
    }

    /// Compute the page containing a given vertical pane offset, assuming
    /// uniform [`Self::row_height`] rows.
    ///
    /// The result is clamped to the last page; it returns zero for an empty
    /// document or a non-positive offset.
    pub fn page_at_offset(&self, offset: f32) -> usize {
        if self.page_count == 0 || !offset.is_finite() || offset <= 0.0 {
            return 0;
        }
        ((offset / self.row_height).floor() as usize).min(self.page_count - 1)
    }

    /// The rows visible at the current scroll offset.
    pub fn visible_range(&self) -> VisibleRange {
        visible_range_for(
            self.scroll_offset,
            self.row_height,
            self.viewport_height,
            self.page_count,
        )
    }

    /// The visible range widened by the prefetch margin on each side.
    ///
    /// On an empty document the result is empty. Otherwise the first row is
    /// `visible_first.saturating_sub(margin)` and the last row is
    /// `visible_last.saturating_add(margin)`, both clamped to the document.
    pub fn prefetch_range(&self) -> VisibleRange {
        let visible = self.visible_range();
        if visible.is_empty() {
            return visible;
        }
        let first = visible
            .first_page()
            .saturating_sub(self.prefetch_margin_rows);
        let last = visible
            .last_page()
            .saturating_add(self.prefetch_margin_rows)
            .min(self.page_count - 1);
        VisibleRange::new(first, last)
    }

    /// The zero-based active (highlighted) page.
    pub fn active_page(&self) -> usize {
        self.active_page
    }

    /// Set the active page, clamping to the document.
    ///
    /// Returns `true` when the value changed. For an empty document the active
    /// page is always zero and this returns `false`.
    pub fn set_active_page(&mut self, page: usize) -> bool {
        if self.page_count == 0 {
            return false;
        }
        let clamped = page.min(self.page_count - 1);
        if self.active_page == clamped {
            false
        } else {
            self.active_page = clamped;
            true
        }
    }

    /// The page explicitly selected by a click, if any.
    pub fn selected_page(&self) -> Option<usize> {
        self.selected_page
    }

    /// Select a page (as a thumbnail click would), marking it active too.
    ///
    /// Returns the validated page on success or `None` when `page` is out of
    /// range for an empty document.
    pub fn select_page(&mut self, page: usize) -> Option<usize> {
        if page >= self.page_count {
            return None;
        }
        self.selected_page = Some(page);
        self.active_page = page;
        Some(page)
    }

    /// Borrow the per-page metadata slice.
    pub fn metadata(&self) -> &[ThumbnailMeta] {
        &self.metadata
    }

    /// Borrow the metadata for one page, if it exists.
    pub fn meta(&self, page: usize) -> Option<&ThumbnailMeta> {
        self.metadata.get(page)
    }

    /// The number of pending generation requests.
    pub fn pending_request_count(&self) -> usize {
        self.requests.len()
    }

    /// Whether `page` currently has a pending generation request.
    pub fn is_queued(&self, page: usize) -> bool {
        self.requests.contains(&page)
    }

    /// Borrow the pending request queue in FIFO order.
    pub fn requests(&self) -> &VecDeque<usize> {
        &self.requests
    }

    /// Fill the request queue from the prefetch range.
    ///
    /// Pages already cached for `spec` and pages already queued are skipped,
    /// which makes repeated calls deduplicate naturally. When the queue is at
    /// capacity the oldest request is discarded to make room, so the newest
    /// visible pages are always kept. Returns the number of pages newly
    /// enqueued.
    pub fn enqueue_visible(&mut self, spec: &ThumbnailSpec) -> usize {
        let range = self.prefetch_range();
        let mut added = 0usize;
        for page in range.iter() {
            if self.cache.contains(PageIndex::new(page), spec) || self.is_queued(page) {
                continue;
            }
            if self.request_capacity == 0 {
                break;
            }
            if self.requests.len() >= self.request_capacity
                && let Some(older) = self.requests.pop_front()
            {
                self.set_status(older, ThumbnailStatus::Idle);
            }
            self.requests.push_back(page);
            self.set_status(page, ThumbnailStatus::Queued);
            added += 1;
        }
        added
    }

    /// Generate up to `max` queued thumbnails through the renderer.
    ///
    /// Requests are popped in FIFO order and passed to the
    /// [`ThumbnailCache::generate`] API, so a page already cached is reused
    /// without touching the renderer. On success the page's status becomes
    /// [`ThumbnailStatus::Ready`]; on failure the page returns to
    /// [`ThumbnailStatus::Idle`] and the error is propagated (matching the
    /// raster cache's fail-fast behavior). Cancellation is checked between
    /// requests and forwarded to the renderer. Returns the number of
    /// thumbnails generated in this call.
    ///
    /// # Errors
    ///
    /// Propagates any [`ThumbnailError`], including cancellation and renderer
    /// failures, wrapped from the underlying [`Renderer`].
    pub fn generate_pending(
        &mut self,
        renderer: &dyn Renderer,
        spec: &ThumbnailSpec,
        max: usize,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<usize, ThumbnailError> {
        let mut generated = 0usize;
        for _ in 0..max {
            cancellation.check()?;
            let Some(page) = self.requests.pop_front() else {
                break;
            };
            match self
                .cache
                .generate(renderer, PageIndex::new(page), spec, cancellation, progress)
            {
                Ok(_) => {
                    self.set_status(page, ThumbnailStatus::Ready);
                    generated += 1;
                }
                Err(error) => {
                    self.set_status(page, ThumbnailStatus::Idle);
                    return Err(error);
                }
            }
        }
        Ok(generated)
    }

    /// Borrow a cached thumbnail for `page` and `spec`, if one is available.
    pub fn cached_thumbnail(&self, page: usize, spec: &ThumbnailSpec) -> Option<&Thumbnail> {
        self.cache.get(PageIndex::new(page), spec)
    }

    /// Generate (or reuse) the thumbnail for one specific page.
    ///
    /// This is the direct-fetch path used when the GUI needs a page's
    /// thumbnail immediately (for example after a click). It always goes
    /// through [`ThumbnailCache::generate`], so requesting an already-cached
    /// page is a cache hit that never touches the renderer. The page's status
    /// becomes [`ThumbnailStatus::Ready`] on success.
    ///
    /// # Errors
    ///
    /// Propagates any [`ThumbnailError`] from the renderer or spec validation.
    pub fn generate_page(
        &mut self,
        renderer: &dyn Renderer,
        page: usize,
        spec: &ThumbnailSpec,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Thumbnail, ThumbnailError> {
        let thumbnail =
            self.cache
                .generate(renderer, PageIndex::new(page), spec, cancellation, progress)?;
        self.set_status(page, ThumbnailStatus::Ready);
        Ok(thumbnail)
    }

    /// The number of cache hits recorded by the underlying thumbnail cache.
    pub fn cache_hits(&self) -> u64 {
        self.cache.hits()
    }

    /// Drop all cached thumbnails while keeping metadata and active state.
    pub fn clear_cache(&mut self) {
        self.cache.clear();
        for meta in &mut self.metadata {
            meta.status = ThumbnailStatus::Idle;
        }
    }

    /// Update one page's status if the page index is valid.
    fn set_status(&mut self, page: usize, status: ThumbnailStatus) {
        if let Some(meta) = self.metadata.get_mut(page) {
            meta.status = status;
        }
    }

    /// Drop requests beyond the configured capacity from the back.
    fn trim_queue(&mut self) {
        while self.requests.len() > self.request_capacity {
            if let Some(page) = self.requests.pop_back() {
                self.set_status(page, ThumbnailStatus::Idle);
            } else {
                break;
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use pdfxi_render::RenderedPage;

    use super::*;

    /// Deterministic scale-aware renderer that counts render phases.
    struct CountingRenderer {
        page_count: usize,
        renders: Arc<AtomicUsize>,
    }

    impl CountingRenderer {
        fn new(page_count: usize) -> Self {
            Self {
                page_count,
                renders: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn renders(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
        }
    }

    impl Renderer for CountingRenderer {
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
            self.renders.fetch_add(1, Ordering::SeqCst);
            progress(Progress::new(1, 1));
            let side = 8u32;
            let pixels = vec![0x40u8; side as usize * side as usize * 4];
            Ok(RenderedPage::new(page, side, side, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    use pdfxi_render::RenderError;

    fn spec() -> ThumbnailSpec {
        ThumbnailSpec::new(96, 96)
    }

    fn pane(pages: usize) -> ThumbnailPaneState {
        let mut state = ThumbnailPaneState::new(pages);
        state.set_row_height(100.0);
        // 250px / 100px rows -> exactly three visible rows (0..=2).
        state.set_viewport_height(250.0);
        state
    }

    // ------------------------------------------------------------------
    // Visible-range math (short and long documents)
    // ------------------------------------------------------------------

    #[test]
    fn visible_range_short_document_shows_all_pages() {
        // Three 100px rows, 600px viewport: every page fits.
        let range = visible_range_for(0.0, 100.0, 600.0, 3);
        assert_eq!(range, VisibleRange::new(0, 2));
        assert_eq!(range.len(), 3);
        assert!(range.contains(0) && range.contains(2) && !range.contains(3));
        assert_eq!(range.iter().collect::<Vec<_>>(), vec![0, 1, 2]);
    }

    #[test]
    fn visible_range_long_document_tracks_scroll_offset() {
        // 100px rows, 200px viewport, ten pages: exactly two rows fit because
        // the viewport's bottom edge is exclusive.
        let top = visible_range_for(0.0, 100.0, 200.0, 10);
        assert_eq!(top, VisibleRange::new(0, 1), "half-open bottom edge");

        let scrolled = visible_range_for(250.0, 100.0, 200.0, 10);
        assert_eq!(scrolled, VisibleRange::new(2, 4));

        // Exact boundary: top on row 3, bottom on row 6 -> rows 3,4,5.
        let boundary = visible_range_for(300.0, 100.0, 300.0, 10);
        assert_eq!(boundary, VisibleRange::new(3, 5));

        // Near the end, the last row clamps to the document.
        let bottom = visible_range_for(950.0, 100.0, 200.0, 10);
        assert_eq!(bottom, VisibleRange::new(9, 9));
    }

    #[test]
    fn visible_range_empty_and_degenerate_inputs_are_safe() {
        assert!(visible_range_for(0.0, 100.0, 200.0, 0).is_empty());
        assert!(visible_range_for(0.0, 100.0, 0.0, 5).is_empty());
        assert_eq!(
            visible_range_for(-50.0, 0.0, 200.0, 5),
            VisibleRange::new(0, 1)
        );
        assert_eq!(
            visible_range_for(f32::NAN, f32::INFINITY, 200.0, 5),
            VisibleRange::new(0, 1)
        );
        assert!(VisibleRange::empty().is_empty());
        assert_eq!(VisibleRange::empty().len(), 0);
        assert_eq!(VisibleRange::empty().iter().count(), 0);
        assert!(!VisibleRange::empty().contains(0));
    }

    #[test]
    fn visible_range_method_uses_pane_state() {
        let mut state = pane(10);
        state.set_scroll_offset(250.0);
        assert_eq!(state.visible_range(), VisibleRange::new(2, 4));
        state.set_scroll_offset(f32::NAN);
        assert_eq!(state.scroll_offset(), 0.0);
        assert_eq!(state.visible_range(), VisibleRange::new(0, 2));
    }

    // ------------------------------------------------------------------
    // Page/row mapping boundaries
    // ------------------------------------------------------------------

    #[test]
    fn page_row_mapping_boundaries_are_inverse() {
        let state = pane(4);
        assert_eq!(state.page_at_row(0), Some(0));
        assert_eq!(state.page_at_row(3), Some(3));
        assert_eq!(state.page_at_row(4), None);
        assert_eq!(state.row_for_page(0), Some(0));
        assert_eq!(state.row_for_page(3), Some(3));
        assert_eq!(state.row_for_page(4), None);
        for page in 0..4 {
            assert_eq!(
                state.page_at_row(state.row_for_page(page).unwrap()),
                Some(page)
            );
        }
    }

    #[test]
    fn page_at_offset_clamps_to_document() {
        let state = pane(5);
        assert_eq!(state.page_at_offset(-10.0), 0);
        assert_eq!(state.page_at_offset(0.0), 0);
        assert_eq!(state.page_at_offset(250.0), 2);
        assert_eq!(state.page_at_offset(99_999.0), 4);
        assert_eq!(ThumbnailPaneState::new(0).page_at_offset(50.0), 0);
    }

    // ------------------------------------------------------------------
    // Prefetch margin and bounded request queue (dedupe)
    // ------------------------------------------------------------------

    #[test]
    fn prefetch_range_expands_visible_range_and_clamps() {
        let mut state = pane(10);
        state.set_prefetch_margin_rows(2);
        state.set_scroll_offset(300.0);
        // Visible 3..=5, expanded by two rows each side -> 1..=7.
        assert_eq!(state.visible_range(), VisibleRange::new(3, 5));
        assert_eq!(state.prefetch_range(), VisibleRange::new(1, 7));

        state.set_scroll_offset(0.0);
        assert_eq!(state.prefetch_range(), VisibleRange::new(0, 4));
        state.set_scroll_offset(950.0);
        assert_eq!(state.prefetch_range(), VisibleRange::new(7, 9));
    }

    #[test]
    fn enqueue_visible_deduplicates_and_is_bounded() {
        let mut state = pane(10);
        state.set_prefetch_margin_rows(1);
        state.set_request_capacity(5);
        state.set_scroll_offset(200.0); // visible 2..=4, prefetch 1..=5
        let first = state.enqueue_visible(&spec());
        assert_eq!(first, 5, "all five prefetch rows are enqueued");
        assert_eq!(state.pending_request_count(), 5);
        assert_eq!(
            state.requests().iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3, 4, 5]
        );

        // A second identical query adds nothing: every page in the range is
        // already queued, so dedupe leaves the queue untouched.
        let again = state.enqueue_visible(&spec());
        assert_eq!(again, 0, "everything in the range is already queued");
        assert_eq!(state.pending_request_count(), 5);

        // Shrinking the capacity trims from the back; scrolling further then
        // drops the oldest requests so the newest visible pages always win.
        state.set_request_capacity(3);
        assert_eq!(
            state.requests().iter().copied().collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        state.set_scroll_offset(700.0); // visible 7..=9, prefetch 6..=9
        assert_eq!(state.enqueue_visible(&spec()), 4);
        assert_eq!(
            state.requests().iter().copied().collect::<Vec<_>>(),
            vec![7, 8, 9]
        );
        assert!(state.pending_request_count() <= state.request_capacity());

        // Capacity zero never queues anything.
        state.set_request_capacity(0);
        assert_eq!(state.enqueue_visible(&spec()), 0);
        assert_eq!(state.pending_request_count(), 0);
    }

    #[test]
    fn enqueue_marks_metadata_queued_and_dedupes_across_scroll() {
        let mut state = pane(10);
        state.set_prefetch_margin_rows(0);
        state.set_scroll_offset(0.0);
        assert_eq!(state.enqueue_visible(&spec()), 3); // rows 0,1,2
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Queued);
        assert!(!state.is_queued(9));
        // Metadata covers one entry per page with correct page/row mapping.
        assert_eq!(state.metadata().len(), 10);
        assert_eq!(state.metadata()[7].row, 7);
    }

    // ------------------------------------------------------------------
    // Active-page tracking and selection on click
    // ------------------------------------------------------------------

    #[test]
    fn active_page_tracking_clamps_and_reports_changes() {
        let mut state = pane(6);
        assert_eq!(state.active_page(), 0);
        assert!(state.set_active_page(4));
        assert_eq!(state.active_page(), 4);
        assert!(!state.set_active_page(4), "no change reports false");
        assert!(state.set_active_page(999));
        assert_eq!(state.active_page(), 5, "clamped to last page");
        assert!(!ThumbnailPaneState::new(0).set_active_page(3));
    }

    #[test]
    fn select_page_on_click_sets_active_and_selected() {
        let mut state = pane(5);
        assert_eq!(state.selected_page(), None);
        assert_eq!(state.select_page(2), Some(2));
        assert_eq!(state.selected_page(), Some(2));
        assert_eq!(state.active_page(), 2);
        assert_eq!(state.select_page(0), Some(0));
        assert_eq!(state.selected_page(), Some(0));
        assert_eq!(state.active_page(), 0);
        assert_eq!(state.select_page(5), None, "out of range is rejected");
        assert_eq!(state.selected_page(), Some(0), "selection is unchanged");
    }

    #[test]
    fn reset_clears_all_document_scoped_state() {
        let mut state = pane(5);
        state.set_active_page(3);
        let _ = state.select_page(3);
        state.enqueue_visible(&spec());
        state.reset(2);
        assert_eq!(state.page_count(), 2);
        assert_eq!(state.active_page(), 0);
        assert_eq!(state.selected_page(), None);
        assert_eq!(state.pending_request_count(), 0);
        assert_eq!(state.metadata().len(), 2);
        assert_eq!(state.meta(1).unwrap().status, ThumbnailStatus::Idle);
    }

    // ------------------------------------------------------------------
    // Generation through the Renderer trait and cache reuse
    // ------------------------------------------------------------------

    #[test]
    fn generate_pending_uses_renderer_and_marks_ready() {
        let renderer = CountingRenderer::new(10);
        let mut state = pane(10);
        state.set_prefetch_margin_rows(0);
        state.set_scroll_offset(0.0);
        state.enqueue_visible(&spec());
        let generated = state
            .generate_pending(
                &renderer,
                &spec(),
                8,
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("generation succeeds");
        assert_eq!(generated, 3);
        assert_eq!(renderer.renders(), 6, "two render phases per thumbnail");
        assert_eq!(state.pending_request_count(), 0);
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Ready);
        let thumb = state.cached_thumbnail(0, &spec()).expect("page 0 cached");
        assert!(!thumb.is_empty());
    }

    #[test]
    fn repeated_visible_queries_reuse_the_cache() {
        let renderer = CountingRenderer::new(10);
        let spec = spec();
        let mut state = pane(10);
        state.set_prefetch_margin_rows(0);
        state.set_scroll_offset(0.0);

        // First pass: enqueue + generate three thumbnails.
        state.enqueue_visible(&spec);
        let first = state
            .generate_pending(&renderer, &spec, 8, &CancellationToken::new(), &mut |_| {})
            .expect("first generation");
        assert_eq!(first, 3);
        let renders_after_first = renderer.renders();
        let first_thumb = state
            .cached_thumbnail(0, &spec)
            .cloned()
            .expect("page 0 cached");

        // Second pass over the same region: everything is cached, so no new
        // requests are enqueued, no renders happen, and the identical pixels
        // are reused.
        assert_eq!(state.enqueue_visible(&spec), 0);
        let second = state
            .generate_pending(&renderer, &spec, 8, &CancellationToken::new(), &mut |_| {})
            .expect("second generation");
        assert_eq!(second, 0);
        assert_eq!(renderer.renders(), renders_after_first, "cache reused");
        assert_eq!(state.cached_thumbnail(0, &spec), Some(&first_thumb));
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Ready);
    }

    #[test]
    fn direct_page_fetch_records_a_cache_hit() {
        let renderer = CountingRenderer::new(10);
        let spec = spec();
        let mut state = pane(10);
        let token = CancellationToken::new();

        let first = state
            .generate_page(&renderer, 0, &spec, &token, &mut |_| {})
            .expect("first fetch generates");
        assert_eq!(state.cache_hits(), 0);
        let second = state
            .generate_page(&renderer, 0, &spec, &token, &mut |_| {})
            .expect("second fetch reuses");
        assert_eq!(first, second, "identical cached pixels");
        assert_eq!(state.cache_hits(), 1, "the second fetch was a cache hit");
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Ready);
    }

    #[test]
    fn generate_pending_honours_cancellation() {
        let renderer = CountingRenderer::new(10);
        let mut state = pane(10);
        state.enqueue_visible(&spec());
        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let result = state.generate_pending(&renderer, &spec(), 8, &cancelled, &mut |_| {});
        assert!(matches!(
            result,
            Err(ThumbnailError::Render(RenderError::Cancelled))
        ));
        assert_eq!(renderer.renders(), 0);
    }

    #[test]
    fn clear_cache_resets_status_but_keeps_selection() {
        let renderer = CountingRenderer::new(10);
        let spec = spec();
        let mut state = pane(10);
        state.select_page(2);
        state.enqueue_visible(&spec);
        let _ = state
            .generate_pending(&renderer, &spec, 8, &CancellationToken::new(), &mut |_| {})
            .expect("generation");
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Ready);
        state.clear_cache();
        assert_eq!(state.meta(0).unwrap().status, ThumbnailStatus::Idle);
        assert_eq!(state.selected_page(), Some(2));
        assert_eq!(state.active_page(), 2);
    }
}
