//! Page-thumbnail generation on top of the [`Renderer`] contract.
//!
//! A thumbnail is a small RGBA raster that fits inside a caller-supplied box
//! while preserving the page's aspect ratio. This module is backend-agnostic:
//! every renderer interaction goes through [`Renderer`], so the same code works
//! with a mock in tests and with the PDFium backend in production.
//!
//! # Generation strategy
//!
//! To avoid upscaling a raster that is smaller than the requested box, a
//! [`crate::thumbnail::ThumbnailGenerator`] works in two phases:
//!
//! 1. It renders the page at scale `1.0` to learn the page's intrinsic pixel
//!    dimensions.
//! 2. It computes the *cover scale* — the factor that makes a raster of those
//!    dimensions at least as large as the box in **both** axes — multiplies it
//!    by [`crate::thumbnail::ThumbnailSpec::scale`], renders again at that scale,
//!    and downscales the result to the contained size with
//!    [`crate::thumbnail::resize_rgba`].
//!
//! Phase 2 always samples *down* (never up) because the cover scale guarantees
//! the intermediate raster covers the box. The optional `scale` multiplier
//! oversamples further for callers that want denser source pixels.
//!
//! # Aspect-preserving fit rules
//!
//! [`crate::thumbnail::contain_dimensions`] implements a "contain" fit: it uses the smaller of the
//! two box-to-source ratios, so the result never exceeds `max_width` or
//! `max_height` and at least one dimension touches a box edge. Integer
//! rounding may make the ratio differ by less than one pixel; results are
//! clamped to at least `1 x 1`. Call [`crate::thumbnail::contains_dimensions`]
//! to check exact
//! invariants without allocating.
//!
//! # Determinism and cancellation
//!
//! For a given renderer, page, and spec the output is byte-for-byte identical:
//! all arithmetic is integer or deterministic `f64`, and nearest-neighbor
//! sampling has no randomness. Both render phases receive the caller's
//! [`CancellationToken`] and progress callback; cancellation is checked before,
//! between, and inside both phases.

use std::collections::{HashMap, VecDeque};

use crate::{CancellationToken, PageIndex, Progress, RenderError, Renderer};

/// Errors produced while validating a spec, fitting dimensions, resizing, or
/// rendering a thumbnail.
///
/// Renderer failures are wrapped in [`ThumbnailError::Render`] so callers can
/// match on cancellation and out-of-range pages without losing the underlying
/// [`RenderError`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum ThumbnailError {
    /// The requested box has a zero width or height.
    #[error("thumbnail box must have a positive width and height")]
    EmptyBox,
    /// The render scale is not finite or is less than `1.0`.
    #[error("invalid thumbnail render scale {0}")]
    InvalidScale(f32),
    /// A raster had a zero width or height.
    #[error("cannot resize a zero-sized raster")]
    ZeroSizedRaster,
    /// A pixel-count calculation overflowed the platform `usize`.
    #[error("raster dimensions overflow")]
    SizeOverflow,
    /// The supplied buffer length did not match the declared dimensions.
    #[error("pixel buffer has {actual} bytes, expected {expected}")]
    PixelBufferMismatch {
        /// Number of bytes required by the declared dimensions.
        expected: usize,
        /// Number of bytes actually supplied.
        actual: usize,
    },
    /// A renderer failed; the original error is preserved.
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// The requested thumbnail bounding box and optional oversampling scale.
///
/// Both box dimensions must be positive. [`Self::scale`] is a multiplier
/// applied to the computed cover scale and must be finite and at least `1.0`;
/// `1.0` (the default) renders exactly enough to cover the box, while larger
/// values oversample before the nearest-neighbor downscale.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ThumbnailSpec {
    /// Maximum thumbnail width in pixels.
    pub max_width: u32,
    /// Maximum thumbnail height in pixels.
    pub max_height: u32,
    /// Oversampling multiplier applied to the computed cover scale.
    pub scale: f32,
}

impl ThumbnailSpec {
    /// Default oversampling multiplier (`1.0`, no extra oversampling).
    pub const DEFAULT_SCALE: f32 = 1.0;

    /// Create a spec for a box of `max_width x max_height` with the default
    /// scale.
    ///
    /// Use [`Self::with_scale`] to request oversampling.
    pub const fn new(max_width: u32, max_height: u32) -> Self {
        Self {
            max_width,
            max_height,
            scale: Self::DEFAULT_SCALE,
        }
    }

    /// Return a copy of this spec with `scale` as the oversampling multiplier.
    ///
    /// The value is validated during generation; see [`Self::validate`].
    pub const fn with_scale(mut self, scale: f32) -> Self {
        self.scale = scale;
        self
    }

    /// Validate the box dimensions and scale.
    ///
    /// Returns [`ThumbnailError::EmptyBox`] if either box dimension is zero and
    /// [`ThumbnailError::InvalidScale`] if the scale is not finite or is less
    /// than `1.0`.
    pub fn validate(&self) -> Result<(), ThumbnailError> {
        if self.max_width == 0 || self.max_height == 0 {
            return Err(ThumbnailError::EmptyBox);
        }
        if !self.scale.is_finite() || self.scale < 1.0 {
            return Err(ThumbnailError::InvalidScale(self.scale));
        }
        Ok(())
    }
}

/// Compute an aspect-preserving "contain" fit of a `src_width x src_height`
/// raster into a `max_width x max_height` box.
///
/// The smaller of the two box-to-source ratios is used, so the returned
/// dimensions never exceed the box and preserve the source aspect ratio up to
/// integer rounding. At least one returned dimension touches a box edge (for a
/// positive-sized source and box). The result is clamped to `1 x 1` so it is
/// never zero.
///
/// # Errors
///
/// Returns [`ThumbnailError::ZeroSizedRaster`] if either source dimension is
/// zero and [`ThumbnailError::EmptyBox`] if either box dimension is zero.
pub fn contain_dimensions(
    src_width: u32,
    src_height: u32,
    max_width: u32,
    max_height: u32,
) -> Result<(u32, u32), ThumbnailError> {
    if src_width == 0 || src_height == 0 {
        return Err(ThumbnailError::ZeroSizedRaster);
    }
    if max_width == 0 || max_height == 0 {
        return Err(ThumbnailError::EmptyBox);
    }

    let ratio = (f64::from(max_width) / f64::from(src_width))
        .min(f64::from(max_height) / f64::from(src_height));
    // Because ratio <= max_width / src_width, src_width * ratio <= max_width,
    // so both casts are bounded by the (u32) box dimensions.
    let fitted_width = ((f64::from(src_width) * ratio).floor() as u32).clamp(1, max_width);
    let fitted_height = ((f64::from(src_height) * ratio).floor() as u32).clamp(1, max_height);
    Ok((fitted_width, fitted_height))
}

/// Check whether `width x height` is a valid aspect-preserving contain fit of
/// `src_width x src_height` inside `max_width x max_height`.
///
/// This is the allocation-free predicate used by tests and by callers that want
/// to verify a fit before resizing. It returns `true` only when
/// [`contain_dimensions`] would produce exactly `(width, height)`.
pub fn contains_dimensions(
    src_width: u32,
    src_height: u32,
    max_width: u32,
    max_height: u32,
    width: u32,
    height: u32,
) -> bool {
    contain_dimensions(src_width, src_height, max_width, max_height) == Ok((width, height))
}

/// Resize a tightly packed RGBA buffer with nearest-neighbor sampling.
///
/// For every destination pixel `(x, y)` the source sample is
///
/// ```text
/// sx = min(src_width  - 1, x * src_width  / dst_width)
/// sy = min(src_height - 1, y * src_height / dst_height)
/// ```
///
/// using integer arithmetic widened to `u64`. The `min` clamp guarantees the
/// final column and row sample the last source column/row even when integer
/// division would otherwise land one short; downscales pick evenly spaced
/// samples, and upscales replicate pixels. Callers that want an
/// aspect-preserving result should pass dimensions from [`contain_dimensions`];
/// this helper maps the source to the exact destination dimensions it is given
/// and does not enforce an aspect ratio itself.
///
/// # Errors
///
/// Returns [`ThumbnailError::ZeroSizedRaster`] if any dimension is zero,
/// [`ThumbnailError::SizeOverflow`] if the byte counts overflow `usize`, and
/// [`ThumbnailError::PixelBufferMismatch`] if `pixels` is not exactly
/// `src_width * src_height * 4` bytes long.
pub fn resize_rgba(
    pixels: &[u8],
    src_width: u32,
    src_height: u32,
    dst_width: u32,
    dst_height: u32,
) -> Result<Vec<u8>, ThumbnailError> {
    if src_width == 0 || src_height == 0 || dst_width == 0 || dst_height == 0 {
        return Err(ThumbnailError::ZeroSizedRaster);
    }

    let src_area = checked_area(src_width, src_height)?;
    let dst_area = checked_area(dst_width, dst_height)?;
    let expected = src_area
        .checked_mul(4)
        .ok_or(ThumbnailError::SizeOverflow)?;
    if pixels.len() != expected {
        return Err(ThumbnailError::PixelBufferMismatch {
            expected,
            actual: pixels.len(),
        });
    }
    let capacity = dst_area
        .checked_mul(4)
        .ok_or(ThumbnailError::SizeOverflow)?;

    let mut output = Vec::with_capacity(capacity);
    let source_width = u64::from(src_width);
    let source_height = u64::from(src_height);
    for y in 0..u64::from(dst_height) {
        let source_y = (y * source_height / u64::from(dst_height)).min(source_height - 1);
        for x in 0..u64::from(dst_width) {
            let source_x = (x * source_width / u64::from(dst_width)).min(source_width - 1);
            // `source_y * source_width + source_x` is below `src_area`, which
            // fits `usize`; multiplying by 4 was checked above.
            let source_index = ((source_y * source_width + source_x) as usize) * 4;
            output.extend_from_slice(&pixels[source_index..source_index + 4]);
        }
    }
    Ok(output)
}

/// Compute `width * height` in `usize`, rejecting overflow.
fn checked_area(width: u32, height: u32) -> Result<usize, ThumbnailError> {
    usize::try_from(width)
        .ok()
        .and_then(|w| usize::try_from(height).ok().and_then(|h| w.checked_mul(h)))
        .ok_or(ThumbnailError::SizeOverflow)
}

/// An aspect-preserving RGBA thumbnail of one page.
///
/// Pixels are tightly packed 8-bit RGBA rows beginning at the top left, just
/// like [`RenderedPage`](crate::RenderedPage). A thumbnail is never zero-sized: a successful
/// generation always has positive dimensions and a buffer of exactly
/// `width * height * 4` bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Thumbnail {
    page: PageIndex,
    width: u32,
    height: u32,
    pixels: Vec<u8>,
}

impl Thumbnail {
    /// Build a thumbnail from validated dimensions and pixels.
    fn new(page: PageIndex, width: u32, height: u32, pixels: Vec<u8>) -> Self {
        Self {
            page,
            width,
            height,
            pixels,
        }
    }

    /// Return the page this thumbnail depicts.
    pub fn page(&self) -> PageIndex {
        self.page
    }

    /// Return the thumbnail width in pixels.
    pub fn width(&self) -> u32 {
        self.width
    }

    /// Return the thumbnail height in pixels.
    pub fn height(&self) -> u32 {
        self.height
    }

    /// Return the tightly packed RGBA pixel buffer.
    pub fn pixels(&self) -> &[u8] {
        &self.pixels
    }

    /// Return `true` when the thumbnail has no pixels.
    ///
    /// A successfully generated thumbnail is never empty; this exists for
    /// symmetry with [`RenderedPage::is_empty`](crate::RenderedPage::is_empty) and for manually inspected
    /// buffers.
    pub fn is_empty(&self) -> bool {
        self.pixels.is_empty()
    }
}

/// A stateless generator that converts renderer output into [`Thumbnail`]s.
///
/// The generator keeps no state, so it is safe to reuse across pages and
/// documents. Per-call state lives in [`ThumbnailCache`]. See the [module
/// docs](self) for the two-phase cover-and-downscale strategy.
#[derive(Clone, Copy, Debug, Default)]
pub struct ThumbnailGenerator;

impl ThumbnailGenerator {
    /// Generate a thumbnail for `page` using `renderer`.
    ///
    /// `progress` receives reports covering both render phases: reports from
    /// the measurement phase use the first half of the range and reports from
    /// the cover phase use the second half (as two equal units). Cancellation
    /// is checked before, between, and within both phases.
    ///
    /// # Errors
    ///
    /// Spec validation failures are returned directly as
    /// [`ThumbnailError::EmptyBox`] / [`ThumbnailError::InvalidScale`].
    /// Renderer failures (including cancellation and out-of-range pages) are
    /// wrapped in [`ThumbnailError::Render`].
    pub fn generate(
        &self,
        renderer: &dyn Renderer,
        page: PageIndex,
        spec: &ThumbnailSpec,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Thumbnail, ThumbnailError> {
        spec.validate()?;
        cancellation.check()?;

        // Phase 1: measure at scale 1.0 so the cover scale below is expressed
        // in the renderer's own pixel units.
        let measured = {
            let mut phase_one = |report: Progress| {
                progress(Progress::new(
                    report.completed,
                    report.total.saturating_mul(2),
                ));
            };
            renderer.render_page(page, 1.0, cancellation, &mut phase_one)?
        };
        if measured.width() == 0 || measured.height() == 0 {
            return Err(ThumbnailError::ZeroSizedRaster);
        }

        let (fit_width, fit_height) = contain_dimensions(
            measured.width(),
            measured.height(),
            spec.max_width,
            spec.max_height,
        )?;

        // The cover factor makes a raster at least as large as the box in both
        // axes; the spec's scale multiplies it for optional oversampling.
        let cover = (f64::from(spec.max_width) / f64::from(measured.width()))
            .max(f64::from(spec.max_height) / f64::from(measured.height()))
            * f64::from(spec.scale);
        let render_scale = cover as f32;
        if !render_scale.is_finite() || render_scale <= 0.0 {
            return Err(ThumbnailError::InvalidScale(render_scale));
        }
        cancellation.check()?;

        // Phase 2: render large enough to cover the box, then sample down.
        let covered = {
            let mut phase_two = |report: Progress| {
                progress(Progress::new(
                    report.total.saturating_add(report.completed),
                    report.total.saturating_mul(2),
                ));
            };
            renderer.render_page(page, render_scale, cancellation, &mut phase_two)?
        };
        if covered.width() == 0 || covered.height() == 0 {
            return Err(ThumbnailError::ZeroSizedRaster);
        }

        let pixels = resize_rgba(
            covered.pixels(),
            covered.width(),
            covered.height(),
            fit_width,
            fit_height,
        )?;
        Ok(Thumbnail::new(page, fit_width, fit_height, pixels))
    }
}

/// Identifies one cached thumbnail: a page plus the exact spec that produced
/// it. `f32` is keyed by its bit pattern so `NaN` and signed zero cannot alias.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
struct CacheKey {
    page: PageIndex,
    max_width: u32,
    max_height: u32,
    scale_bits: u32,
}

impl CacheKey {
    fn new(page: PageIndex, spec: &ThumbnailSpec) -> Self {
        Self {
            page,
            max_width: spec.max_width,
            max_height: spec.max_height,
            scale_bits: spec.scale.to_bits(),
        }
    }
}

/// A least-recently-used cache of generated thumbnails.
///
/// Keys are `(page, spec)` pairs, so requesting the same page with the same
/// dimensions and scale returns the identical thumbnail without touching the
/// renderer. A successful generation counts as one use; a cache hit refreshes
/// its position as the most recently used entry.
///
/// The optional limit is the maximum number of retained entries. When a new
/// thumbnail would exceed it, the *least recently used* entry is evicted first
/// (documented LRU policy). A limit of `0` retains nothing, so every call
/// re-renders. [`Self::clear`] drops all entries and usage history.
#[derive(Debug)]
pub struct ThumbnailCache {
    max_entries: Option<usize>,
    entries: HashMap<CacheKey, Thumbnail>,
    /// Oldest (least recently used) at the front, newest at the back.
    lru: VecDeque<CacheKey>,
    hits: u64,
}

impl ThumbnailCache {
    /// Create a cache with an optional maximum number of entries.
    ///
    /// `None` means unbounded; `Some(0)` retains no entries.
    pub fn new(max_entries: Option<usize>) -> Self {
        Self {
            max_entries,
            entries: HashMap::new(),
            lru: VecDeque::new(),
            hits: 0,
        }
    }

    /// Create a cache with no entry limit.
    pub fn unbounded() -> Self {
        Self::new(None)
    }

    /// Create a cache retaining at most `limit` entries.
    ///
    /// A `limit` of `0` disables retention (every call renders).
    pub fn with_limit(limit: usize) -> Self {
        Self::new(Some(limit))
    }

    /// Return the number of cached thumbnails.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when the cache holds no thumbnails.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Return the number of successful cache hits since creation or the last
    /// [`Self::clear`].
    pub fn hits(&self) -> u64 {
        self.hits
    }

    /// Return the cached thumbnail for `page` and `spec`, if present.
    ///
    /// This is a read-only lookup: it does not refresh the entry's LRU
    /// position. Use [`Self::generate`] to generate or to mark a use.
    pub fn get(&self, page: PageIndex, spec: &ThumbnailSpec) -> Option<&Thumbnail> {
        self.entries.get(&CacheKey::new(page, spec))
    }

    /// Return `true` when an entry for `page` and `spec` is cached.
    ///
    /// Like [`Self::get`], this does not refresh the LRU position.
    pub fn contains(&self, page: PageIndex, spec: &ThumbnailSpec) -> bool {
        self.get(page, spec).is_some()
    }

    /// Return a cached thumbnail or generate and cache it.
    ///
    /// On a hit the returned thumbnail is a clone of the cached entry and the
    /// renderer is not called. On a miss the thumbnail is generated through
    /// [`ThumbnailGenerator::generate`], inserted (evicting the least recently
    /// used entry when over the limit), and returned.
    ///
    /// # Errors
    ///
    /// Propagates every [`ThumbnailError`] from generation. A failed
    /// generation leaves the cache unchanged.
    pub fn generate(
        &mut self,
        renderer: &dyn Renderer,
        page: PageIndex,
        spec: &ThumbnailSpec,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<Thumbnail, ThumbnailError> {
        spec.validate()?;
        let key = CacheKey::new(page, spec);
        if self.entries.contains_key(&key) {
            self.hits = self.hits.saturating_add(1);
            self.touch(key);
            // The map and the LRU list are kept in sync by `touch`/`insert`.
            if let Some(thumbnail) = self.entries.get(&key) {
                return Ok(thumbnail.clone());
            }
        }

        let thumbnail =
            ThumbnailGenerator.generate(renderer, page, spec, cancellation, progress)?;
        self.insert(key, thumbnail.clone());
        Ok(thumbnail)
    }

    /// Drop all cached thumbnails, usage history, and hit count.
    pub fn clear(&mut self) {
        self.entries.clear();
        self.lru.clear();
        self.hits = 0;
    }

    /// Move `key` to the most-recently-used position.
    fn touch(&mut self, key: CacheKey) {
        if let Some(position) = self.lru.iter().position(|existing| *existing == key) {
            self.lru.remove(position);
        }
        self.lru.push_back(key);
    }

    /// Insert a freshly generated thumbnail, evicting the LRU entry if needed.
    fn insert(&mut self, key: CacheKey, thumbnail: Thumbnail) {
        self.entries.insert(key, thumbnail);
        self.lru.push_back(key);
        if let Some(limit) = self.max_entries {
            while self.entries.len() > limit
                && let Some(oldest) = self.lru.pop_front()
            {
                self.entries.remove(&oldest);
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

    use super::*;
    use crate::RenderedPage;

    /// A scale-aware renderer with a deterministic colour pattern and a render
    /// counter, used to observe cache behaviour without a native backend.
    struct ScaleMock {
        base_width: u32,
        base_height: u32,
        page_count: usize,
        renders: Arc<AtomicUsize>,
        cancel_after_first_render: Option<CancellationToken>,
    }

    impl ScaleMock {
        fn new(base_width: u32, base_height: u32) -> Self {
            Self {
                base_width,
                base_height,
                page_count: 4,
                renders: Arc::new(AtomicUsize::new(0)),
                cancel_after_first_render: None,
            }
        }

        fn render_count(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
        }
    }

    impl Renderer for ScaleMock {
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

            progress(Progress::new(0, 1));
            cancellation.check()?;
            let width = ((f64::from(self.base_width) * f64::from(scale)).round() as u64)
                .clamp(1, u64::from(u32::MAX)) as u32;
            let height = ((f64::from(self.base_height) * f64::from(scale)).round() as u64)
                .clamp(1, u64::from(u32::MAX)) as u32;
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height {
                for x in 0..width {
                    pixels.push((x.wrapping_mul(37).wrapping_add(y * 11)) as u8);
                    pixels.push(y.wrapping_mul(29) as u8);
                    pixels.push(x.wrapping_mul(13) as u8);
                    pixels.push(255);
                }
            }
            progress(Progress::new(1, 1));
            if self.cancel_after_first_render.is_some() && self.render_count() == 1 {
                cancellation.cancel();
            }
            cancellation.check()?;
            Ok(RenderedPage::new(page, width, height, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    fn generate(
        renderer: &dyn Renderer,
        page: PageIndex,
        spec: &ThumbnailSpec,
    ) -> Result<Thumbnail, ThumbnailError> {
        ThumbnailGenerator.generate(renderer, page, spec, &CancellationToken::new(), &mut |_| {})
    }

    #[test]
    fn contain_fit_preserves_aspect_for_wide_tall_and_square_pages() {
        assert_eq!(contain_dimensions(200, 100, 64, 64), Ok((64, 32)));
        assert_eq!(contain_dimensions(100, 200, 64, 64), Ok((32, 64)));
        assert_eq!(contain_dimensions(100, 100, 64, 64), Ok((64, 64)));
        assert!(contains_dimensions(200, 100, 64, 64, 64, 32));
        assert!(!contains_dimensions(200, 100, 64, 64, 64, 64));
    }

    #[test]
    fn thumbnail_fit_is_aspect_preserving_for_wide_tall_and_square_pages() {
        for (base_width, base_height, expected) in [
            (200, 100, (64, 32)),
            (100, 200, (32, 64)),
            (100, 100, (64, 64)),
        ] {
            let renderer = ScaleMock::new(base_width, base_height);
            let thumbnail = generate(&renderer, PageIndex::new(0), &ThumbnailSpec::new(64, 64))
                .expect("thumbnail generation succeeds");
            assert_eq!((thumbnail.width(), thumbnail.height()), expected);
            assert!(thumbnail.width() <= 64 && thumbnail.height() <= 64);
            assert_eq!(
                thumbnail.pixels().len(),
                thumbnail.width() as usize * thumbnail.height() as usize * 4
            );
            assert!(!thumbnail.is_empty());
        }
    }

    #[test]
    fn resize_rgba_uses_nearest_neighbor_at_corners_and_replicates_on_upscale() {
        // 2x2 source with four distinct RGBA colours.
        let source = vec![
            255, 0, 0, 255, // (0,0) red
            0, 255, 0, 255, // (1,0) green
            0, 0, 255, 255, // (0,1) blue
            255, 255, 255, 255, // (1,1) white
        ];
        let resized = resize_rgba(&source, 2, 2, 4, 4).expect("downscale/2x upscale");
        assert_eq!(resized.len(), 4 * 4 * 4);
        let pixel = |x: usize, y: usize| -> &[u8] {
            let index = (y * 4 + x) * 4;
            &resized[index..index + 4]
        };
        assert_eq!(pixel(0, 0), &[255, 0, 0, 255]);
        assert_eq!(pixel(1, 1), &[255, 0, 0, 255]);
        assert_eq!(pixel(2, 0), &[0, 255, 0, 255]);
        assert_eq!(pixel(3, 3), &[255, 255, 255, 255]);
        assert_eq!(pixel(0, 3), &[0, 0, 255, 255]);

        // A 1x1 destination samples the top-left pixel only.
        let single = resize_rgba(&source, 2, 2, 1, 1).expect("single pixel");
        assert_eq!(single, vec![255, 0, 0, 255]);

        // A 1x1 source replicated to 3x3 has exactly three identical pixels per
        // row and the last column maps to the only source pixel.
        let one = [7, 8, 9, 10];
        let grown = resize_rgba(&one, 1, 1, 3, 3).expect("upscale");
        assert_eq!(grown, [7, 8, 9, 10].repeat(9));
    }

    #[test]
    fn resize_rgba_rejects_zero_dims_overflow_and_buffer_mismatch() {
        assert_eq!(
            resize_rgba(&[], 0, 2, 1, 1),
            Err(ThumbnailError::ZeroSizedRaster)
        );
        assert_eq!(
            resize_rgba(&[], 2, 0, 1, 1),
            Err(ThumbnailError::ZeroSizedRaster)
        );
        assert_eq!(
            resize_rgba(&[], 2, 2, 0, 1),
            Err(ThumbnailError::ZeroSizedRaster)
        );
        assert_eq!(
            resize_rgba(&[], 2, 2, 1, 0),
            Err(ThumbnailError::ZeroSizedRaster)
        );
        assert_eq!(
            resize_rgba(&[0; 15], 2, 2, 1, 1),
            Err(ThumbnailError::PixelBufferMismatch {
                expected: 16,
                actual: 15
            })
        );
        assert_eq!(
            contain_dimensions(0, 10, 64, 64),
            Err(ThumbnailError::ZeroSizedRaster)
        );
        assert_eq!(
            contain_dimensions(10, 10, 0, 64),
            Err(ThumbnailError::EmptyBox)
        );
    }

    #[test]
    fn zero_size_spec_is_rejected_without_rendering() {
        let renderer = ScaleMock::new(100, 100);
        assert_eq!(
            generate(&renderer, PageIndex::new(0), &ThumbnailSpec::new(0, 64)),
            Err(ThumbnailError::EmptyBox)
        );
        assert_eq!(
            generate(&renderer, PageIndex::new(0), &ThumbnailSpec::new(64, 0)),
            Err(ThumbnailError::EmptyBox)
        );
        assert_eq!(renderer.render_count(), 0);
    }

    #[test]
    fn invalid_scale_is_rejected_without_rendering() {
        let renderer = ScaleMock::new(100, 100);
        for scale in [0.0, 0.5, f32::INFINITY, f32::NAN] {
            let spec = ThumbnailSpec::new(64, 64).with_scale(scale);
            assert!(matches!(
                generate(&renderer, PageIndex::new(0), &spec),
                Err(ThumbnailError::InvalidScale(_))
            ));
        }
        assert_eq!(renderer.render_count(), 0);
    }

    #[test]
    fn out_of_range_page_error_propagates() {
        let mut renderer = ScaleMock::new(100, 100);
        renderer.page_count = 1;
        assert_eq!(
            generate(&renderer, PageIndex::new(3), &ThumbnailSpec::new(64, 64)),
            Err(ThumbnailError::Render(RenderError::PageOutOfRange(3)))
        );
    }

    #[test]
    fn cancellation_is_propagated_before_and_between_phases() {
        let renderer = ScaleMock::new(100, 100);
        let spec = ThumbnailSpec::new(64, 64);
        let token = CancellationToken::new();
        token.cancel();
        let result =
            ThumbnailGenerator.generate(&renderer, PageIndex::new(0), &spec, &token, &mut |_| {});
        assert_eq!(result, Err(ThumbnailError::Render(RenderError::Cancelled)));
        assert_eq!(
            renderer.render_count(),
            0,
            "no render after early cancellation"
        );

        let mut cancel_later = ScaleMock::new(100, 100);
        let later_token = CancellationToken::new();
        cancel_later.cancel_after_first_render = Some(later_token.clone());
        let result = ThumbnailGenerator.generate(
            &cancel_later,
            PageIndex::new(0),
            &spec,
            &later_token,
            &mut |_| {},
        );
        assert_eq!(result, Err(ThumbnailError::Render(RenderError::Cancelled)));
        assert_eq!(
            cancel_later.render_count(),
            1,
            "the second phase must not run once cancellation fires"
        );
    }

    #[test]
    fn cache_hit_avoids_rendering_again_and_is_deterministic() {
        let renderer = ScaleMock::new(160, 90);
        let spec = ThumbnailSpec::new(64, 64);
        let mut cache = ThumbnailCache::unbounded();

        let first = cache
            .generate(
                &renderer,
                PageIndex::new(0),
                &spec,
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("first generation");
        assert_eq!(renderer.render_count(), 2, "two phases render once each");
        let renders_after_first = renderer.render_count();

        let second = cache
            .generate(
                &renderer,
                PageIndex::new(0),
                &spec,
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("cached generation");
        assert_eq!(first, second);
        assert_eq!(
            renderer.render_count(),
            renders_after_first,
            "cache hit must not render"
        );
        assert_eq!(cache.hits(), 1);
        assert_eq!(cache.len(), 1);
        assert!(cache.contains(PageIndex::new(0), &spec));

        // Changing the spec is a different key and triggers a fresh render.
        let different = ThumbnailSpec::new(32, 32);
        let _ = cache
            .generate(
                &renderer,
                PageIndex::new(0),
                &different,
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("different spec generation");
        assert_eq!(renderer.render_count(), renders_after_first + 2);
        assert_eq!(cache.hits(), 1);
    }

    #[test]
    fn cache_eviction_order_is_least_recently_used() {
        let renderer = ScaleMock::new(100, 100);
        let mut cache = ThumbnailCache::with_limit(2);
        let a = ThumbnailSpec::new(64, 64);
        let b = ThumbnailSpec::new(48, 48);
        let c = ThumbnailSpec::new(32, 32);
        let page = PageIndex::new(0);
        let generate = |cache: &mut ThumbnailCache, spec: &ThumbnailSpec| {
            cache
                .generate(
                    &renderer,
                    page,
                    spec,
                    &CancellationToken::new(),
                    &mut |_| {},
                )
                .expect("generation succeeds")
        };

        let _ = generate(&mut cache, &a);
        let _ = generate(&mut cache, &b);
        let _ = generate(&mut cache, &a); // hit: A becomes most recently used.
        assert_eq!(cache.hits(), 1);
        let _ = generate(&mut cache, &c); // evicts B, the least recently used.
        assert_eq!(cache.len(), 2);
        assert!(cache.contains(page, &a));
        assert!(!cache.contains(page, &b));
        assert!(cache.contains(page, &c));

        let _ = generate(&mut cache, &b); // miss: evicts A next.
        assert!(!cache.contains(page, &a));
        assert!(cache.contains(page, &b));
        assert!(cache.contains(page, &c));
    }

    #[test]
    fn clear_drops_entries_history_and_hits() {
        let renderer = ScaleMock::new(100, 100);
        let spec = ThumbnailSpec::new(64, 64);
        let mut cache = ThumbnailCache::with_limit(1);
        for _ in 0..2 {
            let _ = cache.generate(
                &renderer,
                PageIndex::new(0),
                &spec,
                &CancellationToken::new(),
                &mut |_| {},
            );
        }
        assert_eq!(cache.hits(), 1);
        assert!(!cache.is_empty());

        cache.clear();
        assert!(cache.is_empty());
        assert_eq!(cache.len(), 0);
        assert_eq!(cache.hits(), 0);
        let renders_before = renderer.render_count();
        let _ = cache.generate(
            &renderer,
            PageIndex::new(0),
            &spec,
            &CancellationToken::new(),
            &mut |_| {},
        );
        assert_eq!(renderer.render_count(), renders_before + 2);
    }

    #[test]
    fn zero_limit_cache_never_retains_entries() {
        let renderer = ScaleMock::new(100, 100);
        let spec = ThumbnailSpec::new(64, 64);
        let mut cache = ThumbnailCache::with_limit(0);
        for _ in 0..2 {
            let _ = cache.generate(
                &renderer,
                PageIndex::new(0),
                &spec,
                &CancellationToken::new(),
                &mut |_| {},
            );
        }
        assert!(cache.is_empty());
        assert_eq!(cache.hits(), 0);
        assert_eq!(renderer.render_count(), 4);
    }

    #[test]
    fn progress_is_reported_across_both_phases() {
        let renderer = ScaleMock::new(100, 100);
        let spec = ThumbnailSpec::new(64, 64);
        let mut reports = Vec::new();
        let _ = ThumbnailGenerator.generate(
            &renderer,
            PageIndex::new(0),
            &spec,
            &CancellationToken::new(),
            &mut |report| reports.push(report),
        );
        assert!(!reports.is_empty());
        assert!(reports.iter().any(|report| report.completed < 2));
        assert!(reports.iter().any(|report| report.is_complete()));
    }

    #[test]
    fn oversampling_yields_the_same_fit_dimensions() {
        let renderer = ScaleMock::new(160, 90);
        let spec = ThumbnailSpec::new(64, 64);
        let plain = generate(&renderer, PageIndex::new(0), &spec).expect("plain spec");
        let oversampled = generate(&renderer, PageIndex::new(0), &spec.with_scale(2.0))
            .expect("oversampled spec");
        assert_eq!(
            (plain.width(), plain.height()),
            (oversampled.width(), oversampled.height())
        );
    }
}
