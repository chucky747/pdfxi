//! Dependency-light page raster cache for the viewer canvas.
//!
//! This module bridges the backend-agnostic [`Renderer`] contract from
//! `pdfxi-render` to the geometry produced by [`CanvasLayout`]. It has **no
//! `egui` types**: the GUI layer in the `ui` module is responsible for uploading a
//! cached [`RenderedPage`] into a texture. Keeping the cache GUI-free means the
//! render/zoom/rotate/eviction behaviour is fully testable with a mock renderer
//! and no window.
//!
//! # Cache key
//!
//! Every raster is keyed by [`RasterKey`] = `(page, scale_bits, rotation)`.
//! - `page` is the zero-based page index.
//! - `scale_bits` is the IEEE-754 bit pattern of the render scale, so two zoom
//!   levels never collide and an exact scale can be recovered with
//!   [`RasterKey::scale`].
//! - `rotation` is the quarter-turn in degrees (`0`/`90`/`180`/`270`).
//!
//! Changing any of the three produces a different key: zoom and rotation
//! therefore invalidate the previous raster naturally, while
//! [`RasterCache::invalidate_page`] / [`RasterCache::invalidate_all`] handle
//! document mutations explicitly.
//!
//! # Capacity
//!
//! [`RasterCache`] is a bounded **LRU** cache. [`DEFAULT_RASTER_LIMIT`] entries
//! are kept by default; [`RasterCache::new`] accepts any limit, and a limit of
//! zero disables caching. [`RasterCache::get`] promotes an entry, so the
//! least-recently-used raster is evicted first when a new one is inserted.
//! This bounds memory for large documents instead of growing without limit.
//!
//! # Cancellation
//!
//! [`RasterCache::ensure_visible`] checks the supplied [`CancellationToken`]
//! before every render and forwards it to the renderer, mapping any mid-render
//! cancellation to [`RasterError::Cancelled`]. Closed documents can therefore
//! stop outstanding work promptly.

use std::collections::{HashMap, VecDeque};
#[cfg(feature = "pdfium")]
use std::path::PathBuf;

use pdfxi_render::{CancellationToken, PageIndex, Progress, RenderError, RenderedPage, Renderer};

use crate::canvas::{CanvasLayout, CanvasPoint, CanvasRect, CanvasSize};
use crate::view::Rotation;

/// Default maximum number of rasters retained by a [`RasterCache`].
///
/// Sixty-four letter-size RGBA rasters are roughly 128 MiB at 72 DPI, which
/// bounds memory while comfortably covering a typical two-page spread at
/// several zoom levels.
pub const DEFAULT_RASTER_LIMIT: usize = 64;

/// Identifies one cached raster: a page at a particular scale and rotation.
///
/// See the [module docs](crate::raster#cache-key) for why the scale is stored as
/// IEEE-754 bits.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct RasterKey {
    page: usize,
    scale_bits: u32,
    rotation: u16,
}

impl RasterKey {
    /// Build a key for `page` rendered at `scale` with `rotation`.
    pub fn new(page: usize, scale: f32, rotation: Rotation) -> Self {
        Self {
            page,
            scale_bits: scale.to_bits(),
            rotation: rotation.degrees() as u16,
        }
    }

    /// The zero-based page index.
    pub fn page(self) -> usize {
        self.page
    }

    /// The raw IEEE-754 bit pattern of the render scale.
    pub fn scale_bits(self) -> u32 {
        self.scale_bits
    }

    /// Recover the render scale from its stored bit pattern.
    pub fn scale(self) -> f32 {
        f32::from_bits(self.scale_bits)
    }

    /// The quarter-turn in degrees (`0`/`90`/`180`/`270`).
    pub fn rotation_degrees(self) -> u16 {
        self.rotation
    }
}

/// Errors produced while filling or reading the raster cache.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum RasterError {
    /// The operation was cancelled through its [`CancellationToken`].
    #[error("raster rendering was cancelled")]
    Cancelled,
    /// The underlying [`Renderer`] failed.
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// A bounded least-recently-used cache of rendered page rasters.
///
/// The cache is GUI-agnostic: it stores [`RenderedPage`] values keyed by
/// [`RasterKey`] and exposes them to a texture/upload layer through
/// [`RasterCache::get`]. See the [module docs](crate::raster#capacity) for the
/// eviction policy and memory bound.
#[derive(Debug)]
pub struct RasterCache {
    entries: HashMap<RasterKey, RenderedPage>,
    /// Most-recently-used at the front, least-recently-used at the back.
    recency: VecDeque<RasterKey>,
    limit: usize,
}

impl Default for RasterCache {
    fn default() -> Self {
        Self::new(DEFAULT_RASTER_LIMIT)
    }
}

impl RasterCache {
    /// Create an empty cache holding at most `limit` rasters.
    ///
    /// A `limit` of zero disables caching: [`Self::put`] becomes a no-op and
    /// [`Self::ensure_visible`] still renders but retains nothing.
    pub fn new(limit: usize) -> Self {
        Self {
            entries: HashMap::new(),
            recency: VecDeque::new(),
            limit,
        }
    }

    /// The maximum number of rasters this cache retains.
    pub fn limit(&self) -> usize {
        self.limit
    }

    /// The number of rasters currently cached.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Whether the cache holds no rasters.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Whether a raster for exactly `key` is cached.
    pub fn contains(&self, key: RasterKey) -> bool {
        self.entries.contains_key(&key)
    }

    /// Borrow a cached raster, promoting it to most-recently-used.
    ///
    /// Returns `None` when `key` is not cached. Promotion affects only eviction
    /// order; it never triggers a render.
    pub fn get(&mut self, key: RasterKey) -> Option<&RenderedPage> {
        self.touch(key);
        self.entries.get(&key)
    }

    /// Insert `page` under `key`, evicting the least-recently-used entry when
    /// the cache is over capacity. No-op when the limit is zero.
    pub fn put(&mut self, key: RasterKey, page: RenderedPage) {
        if self.limit == 0 {
            return;
        }
        self.entries.insert(key, page);
        self.touch(key);
        while self.entries.len() > self.limit {
            match self.recency.pop_back() {
                Some(lru) => {
                    self.entries.remove(&lru);
                }
                None => break,
            }
        }
    }

    /// Drop every raster belonging to `page` across all scales and rotations.
    ///
    /// Use this after a document edit changes a page's content.
    pub fn invalidate_page(&mut self, page: usize) {
        let doomed: Vec<RasterKey> = self
            .entries
            .keys()
            .copied()
            .filter(|key| key.page() == page)
            .collect();
        for key in doomed {
            self.entries.remove(&key);
            self.recency.retain(|entry| *entry != key);
        }
    }

    /// Drop every cached raster, for example when the document is closed.
    pub fn invalidate_all(&mut self) {
        self.entries.clear();
        self.recency.clear();
    }

    /// Render every page visible in `layout` that is not already cached.
    ///
    /// `scroll` is the current viewport origin in unscrolled canvas units.
    /// Each raster is keyed by the page's **effective rotation**
    /// (`/Rotate` combined with the view rotation, as stored on
    /// [`PageGeometry::rotation`](crate::PageGeometry::rotation)), the same
    /// value the UI uses when painting the cached raster. Progress reports
    /// from the renderer are forwarded unchanged to `progress`. Cached pages
    /// are promoted but not re-rendered.
    ///
    /// Returns the number of rasters newly rendered (zero when everything
    /// visible was already cached, or when the layout is empty).
    ///
    /// # Errors
    ///
    /// Returns [`RasterError::Cancelled`] if `cancellation` fires before or
    /// during a render, and [`RasterError::Render`] for any backend error
    /// (including an out-of-range page when a layout contains more pages than
    /// the renderer has).
    pub fn ensure_visible(
        &mut self,
        renderer: &dyn Renderer,
        layout: &CanvasLayout,
        scroll: CanvasPoint,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<usize, RasterError> {
        let viewport = layout.viewport();
        let mut rendered = 0usize;

        for geometry in layout.pages() {
            if !rect_intersects_viewport(geometry.rect, scroll, viewport) {
                continue;
            }
            if cancellation.is_cancelled() {
                return Err(RasterError::Cancelled);
            }
            let key = RasterKey::new(geometry.page, geometry.scale, geometry.rotation);
            if self.entries.contains_key(&key) {
                self.touch(key);
                continue;
            }
            let page = match renderer.render_page(
                PageIndex::new(geometry.page),
                geometry.scale,
                cancellation,
                progress,
            ) {
                Ok(page) => page,
                Err(RenderError::Cancelled) => return Err(RasterError::Cancelled),
                Err(error) => return Err(RasterError::Render(error)),
            };
            self.put(key, page);
            rendered += 1;
        }

        Ok(rendered)
    }

    /// Move `key` to the front of the recency list, if it is present.
    fn touch(&mut self, key: RasterKey) {
        self.recency.retain(|entry| *entry != key);
        self.recency.push_front(key);
    }
}

/// Whether a page rectangle overlaps the viewport at the given scroll offset.
fn rect_intersects_viewport(rect: CanvasRect, scroll: CanvasPoint, viewport: CanvasSize) -> bool {
    let left = scroll.x;
    let top = scroll.y;
    let right = scroll.x + viewport.width.max(0.0);
    let bottom = scroll.y + viewport.height.max(0.0);
    rect.right() > left && rect.x < right && rect.bottom() > top && rect.y < bottom
}

/// Resolve a fixture path relative to the workspace root.
///
/// `CARGO_MANIFEST_DIR` for this crate is `crates/pdfxi-app`, so the workspace
/// root is two levels up. The returned path is used only by the demo viewer and
/// the PDFium integration test; it is not part of any document-loading API.
/// Callers should check [`Path::is_file`](std::path::Path::is_file) before use.
#[cfg(feature = "pdfium")]
pub fn workspace_fixture_path(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("fixtures")
        .join(name)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::canvas::CanvasSize;
    use crate::view::{ViewMode, Zoom};

    /// A deterministic renderer that counts renders and can be made to fail.
    struct CountingRenderer {
        renders: AtomicUsize,
        fail: bool,
        side: u32,
    }

    impl CountingRenderer {
        fn new() -> Self {
            Self {
                renders: AtomicUsize::new(0),
                fail: false,
                side: 2,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::new()
            }
        }

        fn renders(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
        }
    }

    use std::sync::atomic::{AtomicUsize, Ordering};

    impl Renderer for CountingRenderer {
        fn render_page(
            &self,
            page: PageIndex,
            scale: f32,
            cancellation: &CancellationToken,
            progress: &mut dyn FnMut(Progress),
        ) -> Result<RenderedPage, RenderError> {
            cancellation.check()?;
            if self.fail {
                return Err(RenderError::Backend("boom".to_owned()));
            }
            if !scale.is_finite() || scale <= 0.0 {
                return Err(RenderError::InvalidScale(scale));
            }
            self.renders.fetch_add(1, Ordering::SeqCst);
            progress(Progress::new(1, 1));
            // Like the real backends, honour cancellation raised by the
            // progress callback before building the raster.
            cancellation.check()?;
            let side = self.side as usize;
            let pixels = vec![0xA0u8; side * side * 4];
            Ok(RenderedPage::new(page, self.side, self.side, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(16)
        }
    }

    fn layout(pages: &[crate::canvas::PageSize], scale: f32) -> CanvasLayout {
        CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(scale),
            CanvasSize::new(800.0, 600.0),
            pages,
        )
    }

    fn letter(count: usize) -> Vec<crate::canvas::PageSize> {
        (0..count)
            .map(|_| crate::canvas::PageSize::new(612.0, 792.0))
            .collect()
    }

    fn raster(page: usize) -> RenderedPage {
        RenderedPage::new(PageIndex::new(page), 2, 2, vec![0x30u8; 16])
    }

    fn zero_scroll() -> CanvasPoint {
        CanvasPoint::new(0.0, 0.0)
    }

    #[test]
    fn caching_avoids_a_second_render() {
        let renderer = CountingRenderer::new();
        let layout = layout(&letter(1), 1.0);
        let mut cache = RasterCache::new(8);
        let token = CancellationToken::new();

        let first = cache
            .ensure_visible(&renderer, &layout, zero_scroll(), &token, &mut |_| {})
            .expect("first ensure succeeds");
        let second = cache
            .ensure_visible(&renderer, &layout, zero_scroll(), &token, &mut |_| {})
            .expect("second ensure succeeds");

        assert_eq!(first, 1);
        assert_eq!(second, 0);
        assert_eq!(renderer.renders(), 1, "the cached raster was reused");
        assert!(cache.contains(RasterKey::new(0, 1.0, Rotation::Deg0)));
    }

    #[test]
    fn zoom_and_rotation_changes_invalidate_the_previous_raster() {
        let renderer = CountingRenderer::new();
        let mut cache = RasterCache::new(8);
        let token = CancellationToken::new();

        cache
            .ensure_visible(
                &renderer,
                &layout(&letter(1), 1.0),
                zero_scroll(),
                &token,
                &mut |_| {},
            )
            .expect("scale 1.0");
        cache
            .ensure_visible(
                &renderer,
                &layout(&letter(1), 2.0),
                zero_scroll(),
                &token,
                &mut |_| {},
            )
            .expect("scale 2.0");
        // A rotated layout changes each page's effective rotation and therefore
        // its cache key; no separate rotation argument is needed.
        cache
            .ensure_visible(
                &renderer,
                &CanvasLayout::standard_rotated(
                    ViewMode::Continuous,
                    Zoom::custom(1.0),
                    CanvasSize::new(800.0, 600.0),
                    &letter(1),
                    Rotation::Deg90,
                ),
                zero_scroll(),
                &token,
                &mut |_| {},
            )
            .expect("rotation 90");

        assert_eq!(renderer.renders(), 3, "each distinct key rendered once");
        assert!(cache.contains(RasterKey::new(0, 1.0, Rotation::Deg0)));
        assert!(cache.contains(RasterKey::new(0, 2.0, Rotation::Deg0)));
        assert!(cache.contains(RasterKey::new(0, 1.0, Rotation::Deg90)));
        assert_eq!(RasterKey::new(0, 2.0, Rotation::Deg0).scale(), 2.0);
    }

    #[test]
    fn lru_eviction_removes_the_least_recently_used_entry() {
        let mut cache = RasterCache::new(2);
        let k0 = RasterKey::new(0, 1.0, Rotation::Deg0);
        let k1 = RasterKey::new(1, 1.0, Rotation::Deg0);
        let k2 = RasterKey::new(2, 1.0, Rotation::Deg0);

        cache.put(k0, raster(0));
        cache.put(k1, raster(1));
        // Promote page 0, so page 1 becomes the least-recently-used entry.
        assert!(cache.get(k0).is_some());
        cache.put(k2, raster(2));

        assert_eq!(cache.len(), 2);
        assert!(cache.contains(k0), "recently used page 0 stays");
        assert!(!cache.contains(k1), "page 1 was the LRU victim");
        assert!(cache.contains(k2), "newly inserted page 2 stays");
    }

    #[test]
    fn backend_errors_propagate_as_raster_errors() {
        let renderer = CountingRenderer::failing();
        let layout = layout(&letter(1), 1.0);
        let mut cache = RasterCache::new(8);
        let result = cache.ensure_visible(
            &renderer,
            &layout,
            zero_scroll(),
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert!(
            matches!(&result, Err(RasterError::Render(RenderError::Backend(msg))) if msg.contains("boom")),
            "{result:?}"
        );
        assert!(cache.is_empty(), "a failed render is not cached");
    }

    #[test]
    fn cancellation_is_honoured_before_and_during_render() {
        let renderer = CountingRenderer::new();
        let layout = layout(&letter(2), 1.0);
        let mut cache = RasterCache::new(8);

        let cancelled = CancellationToken::new();
        cancelled.cancel();
        let before =
            cache.ensure_visible(&renderer, &layout, zero_scroll(), &cancelled, &mut |_| {});
        assert_eq!(before, Err(RasterError::Cancelled));
        assert_eq!(renderer.renders(), 0, "nothing rendered after cancellation");

        // A token cancelled from inside the progress callback stops the loop.
        let live = CancellationToken::new();
        let token_for_callback = live.clone();
        let during = cache.ensure_visible(&renderer, &layout, zero_scroll(), &live, &mut |_| {
            token_for_callback.cancel()
        });
        assert_eq!(during, Err(RasterError::Cancelled));
    }

    #[test]
    fn empty_layout_renders_nothing() {
        let renderer = CountingRenderer::new();
        let layout = layout(&[], 1.0);
        let mut cache = RasterCache::new(8);
        assert!(layout.is_empty());
        let count = cache
            .ensure_visible(
                &renderer,
                &layout,
                zero_scroll(),
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("empty layout is not an error");
        assert_eq!(count, 0);
        assert_eq!(renderer.renders(), 0);
        assert!(cache.is_empty());
    }

    #[test]
    fn invalidate_page_and_all_drop_the_expected_entries() {
        let mut cache = RasterCache::new(8);
        cache.put(RasterKey::new(0, 1.0, Rotation::Deg0), raster(0));
        cache.put(RasterKey::new(0, 2.0, Rotation::Deg0), raster(0));
        cache.put(RasterKey::new(1, 1.0, Rotation::Deg0), raster(1));

        cache.invalidate_page(0);
        assert!(!cache.contains(RasterKey::new(0, 1.0, Rotation::Deg0)));
        assert!(!cache.contains(RasterKey::new(0, 2.0, Rotation::Deg0)));
        assert!(cache.contains(RasterKey::new(1, 1.0, Rotation::Deg0)));

        cache.invalidate_all();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.limit(), 8);
    }

    #[test]
    fn zero_limit_disables_caching() {
        let renderer = CountingRenderer::new();
        let layout = layout(&letter(1), 1.0);
        let mut cache = RasterCache::new(0);
        let token = CancellationToken::new();
        for _ in 0..2 {
            assert_eq!(
                cache.ensure_visible(&renderer, &layout, zero_scroll(), &token, &mut |_| {},),
                Ok(1)
            );
        }
        assert_eq!(renderer.renders(), 2);
        assert!(cache.is_empty());
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn pdfium_raster_fills_cache_with_non_uniform_pixels() {
        use pdfxi_render::PdfiumRenderer;

        let path = workspace_fixture_path("minimal.pdf");
        assert!(path.is_file(), "missing fixture: {}", path.display());
        let renderer = PdfiumRenderer::from_file(&path).expect("open the minimal fixture");

        // Direct render of page 0 at scale 1.0 (72 DPI).
        let token = CancellationToken::new();
        let mut reports = Vec::new();
        let direct = renderer
            .render_page(PageIndex::new(0), 1.0, &token, &mut |p| reports.push(p))
            .expect("render page 0 at scale 1.0");
        assert!(direct.width() > 0 && direct.height() > 0);
        assert_eq!(
            direct.pixels().len(),
            direct.width() as usize * direct.height() as usize * 4,
            "RGBA length must be width * height * 4"
        );
        assert!(!reports.is_empty(), "the renderer reports progress");

        // The same raster, obtained through the app's raster cache at scale 1.0.
        let layout = layout(&letter(1), 1.0);
        let mut cache = RasterCache::default();
        let rendered = cache
            .ensure_visible(&renderer, &layout, zero_scroll(), &token, &mut |_| {})
            .expect("fill the cache from the fixture");
        assert_eq!(rendered, 1, "page 0 rendered through the cache");

        let key = RasterKey::new(0, 1.0, Rotation::Deg0);
        let cached = cache.get(key).expect("cached page 0 raster");
        assert!(cached.width() > 0 && cached.height() > 0);
        let pixels = cached.pixels();
        assert_eq!(
            pixels.len(),
            cached.width() as usize * cached.height() as usize * 4
        );
        let first = pixels.first().copied().expect("raster has pixels");
        assert!(
            pixels.iter().any(|byte| *byte != first),
            "the 'Hello, PDFXI!' text must make the raster non-uniform"
        );
    }
}
