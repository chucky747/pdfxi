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
//!
//! The optional `pdfium` feature adds `PdfiumRenderer`, a native backend that
//! loads a document and produces tightly packed RGBA rasters. Default builds do
//! not link PDFium.
//!
//! The [`thumbnail`] module builds aspect-preserving page thumbnails on top of
//! any [`Renderer`], with a pure-Rust nearest-neighbor resizer and an optional
//! least-recently-used cache. It adds no dependencies and is available in every
//! build.
//!
//! With the `pdfium` feature, `PdfiumRenderer` also implements
//! `pdfxi_text::TextExtractor`, recovering real text from the same PDFium
//! instance with the same process-wide mutex discipline as rasterization. The
//! conversion helpers live in a private, native-free module so they can be
//! exercised with synthetic characters in default builds.
//!
//! The [`mod@print`] module turns any [`Renderer`] into an ordered list of print
//! rasters. It validates job settings (page selection, copies, scale, paper,
//! margins), parses 1-based page ranges such as `1-3,5,7-`, honours
//! cancellation, reports progress, and emits a stable [`SpoolSummary`]. It is
//! pure Rust and produces rasters only; sending them to a platform printer is a
//! later adapter.
//!
//! The [`mod@color`] module adds bounded ICC profile parsing, output-intent
//! validation, and a deterministic colour-conversion contract. Device colours
//! carry an explicit colour space, and analytic matrix/TRC and gray-gamma plans
//! are derived from profiles; full ICC/CMM rendering is delegated to the native
//! renderer backend.
//!
//! The [`mod@preview`] module models output preview, separations, ink
//! management, and overprint policy. It produces a deterministic [`PreviewPlan`]
//! a backend can consume, derives process plates from the colour structures in
//! [`mod@color`], and reports unsupported conversions honestly.
//!
//! The [`mod@trapping`] module models trapping presets and transparency
//! flattening (feature PRINT-006). It validates widths, colours, join/sliding
//! choices and flatten resolution, then emits a deterministic [`TrapPlan`]
//! whose backend requirements are stated explicitly.
//!
//! The [`mod@droplet`] module models droplet and hot-folder batch preflight
//! planning (feature PRINT-008). It validates watch paths lexically at the host
//! boundary, selects a preflight profile, and turns per-file facts into a
//! deterministic [`BatchPlan`] and [`BatchSummary`] without any filesystem I/O.
//!
//! The [`mod@blend`] module extends the raster/color contract with blend-mode
//! and group-isolation descriptors (feature STD-005). It produces a
//! deterministic [`BlendPlan`] and honestly reports which compositing steps a
//! native renderer must perform.
//!
//! The [`mod@three_d`] module models a PDF 3D annotation (feature MEDIA-003):
//! its `/3D` stream reference, named views and default view, activation policy,
//! animation flag, and a typed plan whose backend requirements are stated
//! honestly. It does not decode or display 3D geometry.
//!
//! The [`mod@geospatial`] module models geospatial PDF data (feature MEDIA-004):
//! `/VP` viewports, `/Measure` scale and units, `/GPTS`/`/LPTS`/`/GCS`
//! georeferences, bounds validation, and deterministic affine coordinate
//! mapping for geographic systems. Unsupported projections are reported rather
//! than silently mis-mapped.
//!
//! The [`mod@media`] module models `/Sound`, `/Movie` and `/Screen` multimedia
//! annotations (feature MEDIA-001): local/embedded sources, poster and
//! rendition references, playback controls, activation triggers, and a
//! deterministic playback plan for a host player. No codec is decoded and no OS
//! playback is started.
//!
//! The [`mod@html5_media`] module models a local HTML5 replacement envelope
//! (feature MEDIA-002). It validates that every media source is local or
//! embedded, rejects remote/unsupported sources with typed errors, and emits a
//! deterministic descriptor and inert player markup. It never fetches a network
//! resource or executes script.
//!
//! The [`mod@actions`] module models annotation `/AA` interactive actions
//! (feature MEDIA-005): a typed whitelist of trigger events (`E`/`X`/`D`/`U`/
//! `Fo`/`Po`/`PV`/`PI`), deterministic trigger ordering, and rejection of
//! dangerous or unsupported actions. No script is executed.

#![deny(missing_docs)]
#![deny(unsafe_code)]

/// Native PDFium renderer, available only with the `pdfium` Cargo feature.
#[cfg(feature = "pdfium")]
pub mod pdfium_backend;

#[cfg(feature = "pdfium")]
pub use pdfium_backend::{DocumentMeta, PdfiumRenderer};

/// Dependency-free page-thumbnail generation and LRU caching on top of the
/// [`Renderer`] contract.
pub mod thumbnail;

/// Dependency-free print raster pipeline: page selection, job settings, page
/// ranges, and cancellation-aware raster spooling. Produces rasters only; a
/// Windows device adapter lives in [`win_print`].
pub mod print;

/// Windows GDI print adapter: native print dialog, printer DPI, and
/// `StartDoc`/`StretchDIBits` page output. Non-Windows builds expose the same
/// API and return [`win_print::PrintBackendError::Unsupported`].
///
/// This is the only module in the crate allowed to contain `unsafe` code; the
/// crate-level lint stays `deny` and this module opts in explicitly.
pub mod win_print;

/// Dependency-free page-image export: PNG, baseline JPEG, and uncompressed
/// multi-page TIFF with DPI and page-fit controls. Reuses the print pipeline for
/// page selection and cancellation, and adds pure-Rust encoders.
pub mod export;

pub use export::{
    AlphaPolicy, ColorMode, DEFAULT_EXPORT_DPI, ExportError, ExportOptions, ExportedImage, FitMode,
    ImageFormat, MAX_EXPORT_DPI, encode_image, export_pages,
};

/// Dependency-light ICC profile parsing, output-intent validation, and a
/// deterministic colour-conversion contract. Full ICC/CMM rendering is
/// delegated to the renderer backend.
pub mod color;

/// Output preview, separations, ink management, and overprint policy.
///
/// Produces a deterministic [`PreviewPlan`] for a backend to consume, derives
/// process plates from the colour structures in [`mod@color`], and reports
/// unsupported conversions honestly.
pub mod preview;

/// Trapping presets and transparency-flattening policy (feature PRINT-006).
///
/// Validates trap widths, black trap colour, join and sliding choices, and
/// flatten resolution/balance/overprint, then emits a deterministic [`TrapPlan`]
/// with explicit backend requirements.
pub mod trapping;

/// Droplet and hot-folder batch-preflight planning (feature PRINT-008).
///
/// Validates watch paths lexically, selects a preflight profile, and builds a
/// deterministic [`BatchPlan`] and [`BatchSummary`] from in-memory per-file
/// facts. Performs no filesystem I/O.
pub mod droplet;

/// Blend-mode and transparency-group isolation descriptors (feature STD-005).
///
/// Extends the raster/color contract with deterministic compositing plans and
/// honest backend delegation for non-separable modes and isolated groups.
pub mod blend;

/// 3D annotation model (feature MEDIA-003).
///
/// Parses and validates the `/3D` stream reference, named views and default,
/// activation policy, and animation flag, then produces a typed backend plan.
/// It never decodes or displays 3D geometry.
pub mod three_d;

/// Geospatial PDF model (feature MEDIA-004).
///
/// Parses and validates `/VP` viewports, `/Measure` scale/units, and the
/// `/GPTS`/`/LPTS`/`/GCS` georeferences, with deterministic affine coordinate
/// mapping and honest reporting of unsupported projections.
pub mod geospatial;

/// Audio and video annotation model (feature MEDIA-001).
///
/// Parses and validates `/Sound`, `/Movie` and `/Screen` annotations ΓÇö their
/// media sources, poster and rendition references, playback controls and
/// activation triggers ΓÇö and produces a deterministic playback plan for a host
/// player. It decodes no codec and starts no OS playback.
pub mod media;

/// Local HTML5 media replacement envelope (feature MEDIA-002).
///
/// Validates that every media source is local or embedded, rejects
/// remote/unsupported sources with typed errors, and produces a deterministic
/// descriptor and inert HTML5 player markup. It performs no network access and
/// executes no script.
pub mod html5_media;

/// Interactive `/AA` actions and triggers (feature MEDIA-005).
///
/// Parses additional-action events into a typed whitelist, orders triggers
/// deterministically, and rejects dangerous or unsupported actions. No script
/// is executed and no external application is launched.
pub mod actions;

pub use color::{
    ColorError, ConversionPlan, DeviceClass, DeviceColor, DeviceColorSpace, ICC_HEADER_SIZE,
    IccColorSpace, IccHeader, IccProfile, MAX_ICC_PROFILE_BYTES, MAX_ICC_TAG_COUNT,
    MIN_ICC_PROFILE_BYTES, OutputIntent, OutputIntentIssue, OutputIntentSet, OutputIntentSubtype,
    Pcs, RawOutputIntent, RenderingIntent, RgbMatrix, RgbMatrixTrc, TagEntry, Trc, Xyz,
};

pub use preview::{
    InkEntry, InkKind, InkManager, InkSummary, MAX_INK_NAME_BYTES, MAX_INKS, OverprintPolicy,
    PreviewConfig, PreviewError, PreviewMode, PreviewPlan, SoftProofIntent,
};

pub use trapping::{
    DEFAULT_BLACK_WIDTH_PT, DEFAULT_FLATTEN_DEPTH, DEFAULT_FLATTEN_DPI, DEFAULT_TRAP_WIDTH_PT,
    FlattenOverprint, FlattenPolicy, MAX_BLACK_WIDTH_PT, MAX_FLATTEN_DEPTH, MAX_FLATTEN_DPI,
    MAX_PRESET_NAME_BYTES, MAX_SLIDE_PT, MAX_TRAP_WIDTH_PT, MIN_FLATTEN_DPI, TrapColor, TrapError,
    TrapJoinStyle, TrapPlan, TrapSliding, TrappingPreset, VectorRasterBalance,
};

pub use droplet::{
    BatchPlan, BatchSummary, DEFAULT_POLL_INTERVAL_MS, DropletError, FileOutcomePlan,
    FilePreflightInput, FolderWatchConfig, MAX_BATCH_FILES, MAX_LOGICAL_NAME_BYTES, MAX_NAME_BYTES,
    MAX_PATH_BYTES, MAX_POLL_INTERVAL_MS, MIN_POLL_INTERVAL_MS, PreflightProfile, PreflightResult,
    WatchedPath,
};

pub use blend::{
    BlendCategory, BlendError, BlendMode, BlendPlan, BlendSpec, GroupIsolation, GroupKnockout,
    MAX_GROUP_NAME_BYTES, TransparencyGroup,
};

pub use three_d::{
    ActivationEvent, ActivationState, BackendRequirement, MAX_NAME_BYTES as MAX_3D_NAME_BYTES,
    MAX_VIEW_COUNT, ThreeDAnnotation, ThreeDDictionary, ThreeDError, ThreeDFormat, ThreeDPlan,
    ThreeDStreamRef, ThreeDView, ViewSelection,
};

pub use geospatial::{
    EDGE_TOLERANCE, GeoBounds, GeodeticCr, GeospatialDictionary, GeospatialDocument,
    GeospatialError, GeospatialPlan, LocalPoints, MAX_COORD_ABS,
    MAX_NAME_BYTES as MAX_GEO_NAME_BYTES, MAX_VIEWPORTS, Measure, MeasureSubtype, PdfRect,
    PlanNote, Projection, Unit, Viewport, ViewportInput,
};

pub use media::{
    ActivationTrigger, HostRequirement, MAX_MIME_BYTES as MAX_MEDIA_MIME_BYTES,
    MAX_NAME_BYTES as MAX_MEDIA_NAME_BYTES, MAX_RENDITIONS, MediaAnnotation, MediaDictionary,
    MediaError, MediaKind, MediaSource, PlaybackControl, PlaybackPlan, Rendition, RenditionInput,
    RenditionSelection,
};

pub use html5_media::{
    Html5Descriptor, Html5EnvelopeInput, Html5MediaError, Html5Source, Html5SourceInput,
    LocalMediaRef, MAX_DIMENSION, MAX_MIME_BYTES as MAX_HTML5_MIME_BYTES, MAX_SOURCE_BYTES,
    MAX_SOURCES, MediaCategory,
};

pub use actions::{
    AaActionSet, AaEvent, ActionError, ActionPlan, AdditionalActionInput, MAX_ACTION_PAGE,
    MAX_ACTIONS, MAX_NAME_BYTES as MAX_ACTION_NAME_BYTES, PlannedAction, SafeAction,
};

pub use print::{
    Duplex, Margins, Orientation, PageScaleMode, PageSelection, Paper, PrintError, PrintJob,
    PrintPage, PrintSettings, PrintSpool, PrintSpooler, SpoolSummary, parse_page_ranges,
    selection_from_ranges,
};

/// Backend-agnostic conversion of per-character text runs into `pdfxi_text`
/// pages. Private to this crate: the PDFium backend consumes it, and its mock
/// character tests run in default builds without any native dependency.
// In default builds nothing calls these helpers outside their own tests, so
// silence dead_code there; with `pdfium` the native backend uses every item.
#[cfg_attr(not(feature = "pdfium"), allow(dead_code))]
mod text_extract;

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

/// Maximum pixels (width ├ù height) a single page raster may allocate.
///
/// 16 Mpx is 64 MiB of RGBA pixels. Renderers clamp their scale so a hostile or
/// very large page cannot request an unbounded allocation.
pub const MAX_RASTER_PIXELS: u64 = 16 * 1024 * 1024;

/// Smallest scale a clamped render falls back to.
pub const MIN_RASTER_SCALE: f32 = 0.05;

/// Clamp `scale` so `width_pt ├ù height_pt ├ù scale┬▓` stays within `max_pixels`.
///
/// Returns `scale` unchanged when it is invalid or already within budget, and
/// otherwise the largest scale that fits, floored at [`MIN_RASTER_SCALE`]. The
/// helper is pure so callers can validate the policy without a renderer.
#[must_use]
pub fn clamp_render_scale(width_pt: f32, height_pt: f32, scale: f32, max_pixels: u64) -> f32 {
    if !scale.is_finite() || scale <= 0.0 || max_pixels == 0 {
        return scale;
    }
    if !width_pt.is_finite() || !height_pt.is_finite() || width_pt <= 0.0 || height_pt <= 0.0 {
        return scale;
    }
    let points = f64::from(width_pt) * f64::from(height_pt);
    let requested = points * f64::from(scale) * f64::from(scale);
    if requested <= max_pixels as f64 {
        return scale;
    }
    let allowed = (max_pixels as f64 / points).sqrt() * (1.0 - 1e-6);
    if !allowed.is_finite() || allowed <= 0.0 {
        return MIN_RASTER_SCALE;
    }
    (allowed as f32).clamp(MIN_RASTER_SCALE, scale)
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

    /// Return a page's size in points without rasterizing it.
    ///
    /// Backends that cannot report geometry return [`RenderError::Backend`];
    /// the PDFium backend reports the document's real page box. The default
    /// keeps existing renderers source-compatible.
    fn page_size(&self, page: PageIndex) -> Result<PageDimensions, RenderError> {
        Err(RenderError::Backend(format!(
            "page size is not available for page {}",
            page.get()
        )))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn clamp_render_scale_keeps_requests_within_the_pixel_budget() {
        // 1000x1000 points at scale 2 is 4 Mpx, under the 16 Mpx budget.
        assert_eq!(
            clamp_render_scale(1000.0, 1000.0, 2.0, MAX_RASTER_PIXELS),
            2.0
        );
        // At scale 8 the same page would be 64 Mpx, so it clamps to
        // sqrt(16_777_216 / 1_000_000) = 4.096.
        let clamped = clamp_render_scale(1000.0, 1000.0, 8.0, MAX_RASTER_PIXELS);
        assert!((clamped - 4.096).abs() < 1e-4, "clamped scale: {clamped}");
        // The clamped scale fits the budget exactly.
        let pixels = 1000.0 * 1000.0 * f64::from(clamped) * f64::from(clamped);
        assert!(pixels <= MAX_RASTER_PIXELS as f64 + 1.0);
    }

    #[test]
    fn clamp_render_scale_floors_pathological_pages() {
        // A 100000x100000 point page cannot fit even at very small scales.
        let clamped = clamp_render_scale(100_000.0, 100_000.0, 1.0, MAX_RASTER_PIXELS);
        assert_eq!(clamped, MIN_RASTER_SCALE);
    }

    #[test]
    fn clamp_render_scale_passes_invalid_input_through() {
        assert!(clamp_render_scale(100.0, 100.0, f32::NAN, MAX_RASTER_PIXELS).is_nan());
        assert_eq!(clamp_render_scale(100.0, 100.0, 1.0, 0), 1.0);
        assert_eq!(clamp_render_scale(0.0, 100.0, 4.0, MAX_RASTER_PIXELS), 4.0);
        assert_eq!(clamp_render_scale(-1.0, 100.0, 4.0, MAX_RASTER_PIXELS), 4.0);
        assert_eq!(
            clamp_render_scale(100.0, 100.0, 0.5, MAX_RASTER_PIXELS),
            0.5
        );
    }

    #[test]
    fn clamped_scale_is_never_above_the_requested_scale() {
        for (width, height, scale) in [
            (612.0, 792.0, 8.0),
            (2384.0, 3370.0, 1.0),
            (2384.0, 3370.0, 8.0),
            (10.0, 10.0, 0.1),
        ] {
            let clamped = clamp_render_scale(width, height, scale, MAX_RASTER_PIXELS);
            assert!(clamped <= scale, "{width}x{height} at {scale} -> {clamped}");
            assert!(clamped > 0.0);
            assert!(clamped.is_finite());
        }
    }

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

/// A page's physical size in PDF points (1/72 inch).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageDimensions {
    /// Page width in points.
    pub width_pt: f32,
    /// Page height in points.
    pub height_pt: f32,
}
