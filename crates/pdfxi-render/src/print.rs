//! Print raster pipeline: page selection, job settings, and raster spooling.
//!
//! This module turns a [`crate::Renderer`] into an ordered list of ready-to-send
//! page rasters. It is deliberately **backend- and platform-agnostic**: it
//! produces [`PrintPage`] rasters plus a stable [`SpoolSummary`], but it does not
//! talk to a platform printer. Handing the rasters to a Windows/macOS/Linux
//! print driver is a separate, later adapter that consumes this module's output.
//!
//! # Page numbers
//!
//! All user-facing page numbers are **1-based** (the first page is `1`), while
//! the resolved page indices fed to the renderer and stored in [`PrintPage::page`]
//! are the crate's zero-based [`PageIndex`]. [`parse_page_ranges`] converts
//! between the two. Range expressions such as `"1-3,5,7-"` accept inclusive
//! closed spans, open spans (`"7-"` extends to the last page), and singles;
//! duplicates are suppressed and results come back sorted.
//!
//! # Scaling
//!
//! A renderer's scale of `1.0` denotes 72 DPI (one pixel per PDF point). The
//! target [`PrintSettings::dpi`] therefore multiplies that base scale:
//!
//! * [`PageScaleMode::Actual`] renders at the target DPI with no size change.
//! * [`PageScaleMode::CustomPercent`] additionally multiplies by `percent / 100`.
//! * [`PageScaleMode::Fit`] first measures the page at scale `1.0`, then chooses
//!   a factor that fits it inside the printable area (paper minus margins,
//!   swapped for [`Orientation::Landscape`]) while preserving aspect ratio.
//!
//! # Cancellation and progress
//!
//! [`PrintSpooler::spool`] checks cancellation before the first page and between
//! every page, returning [`PrintError::Cancelled`] promptly. Progress is
//! reported at whole-raster granularity, so a job of `N` rasters reports
//! `Progress::new(completed, N)` for `completed` in `0..=N`.
//!
//! # Determinism
//!
//! For a fixed renderer, settings, and job, the spooler emits rasters in a
//! deterministic order (ascending page number, then copies) and the summary is
//! byte-for-byte reproducible. All arithmetic uses deterministic `f64` and
//! checked integer operations; there is no randomness and no unsafe code.

use crate::{CancellationToken, PageIndex, Progress, RenderError, RenderedPage, Renderer};

/// Number of PDF points per inch. A renderer scale of `1.0` equals 72 DPI.
const POINTS_PER_INCH: f64 = 72.0;

/// Structured errors produced while configuring or spooling a print job.
///
/// Renderer failures are preserved in [`PrintError::Render`]; cancellation
/// detected by the spooler itself is reported as [`PrintError::Cancelled`].
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum PrintError {
    /// The selection resolved to no pages (an empty document or empty list).
    #[error("no pages were selected for printing")]
    EmptySelection,
    /// A page number was less than one; page numbers are 1-based.
    #[error("invalid page number {0}; pages are numbered from 1")]
    InvalidPage(u32),
    /// A selected page was beyond the document's page count.
    #[error("page {0} is out of range for a document with {1} page(s)")]
    PageOutOfRange(u32, usize),
    /// A token in a page-range expression was malformed.
    #[error("invalid page range entry '{0}'")]
    InvalidRange(String),
    /// A range started after it ended.
    #[error("page range start {start} is greater than end {end}")]
    ReversedRange {
        /// The (1-based) start of the invalid range.
        start: u32,
        /// The (1-based) end of the invalid range.
        end: u32,
    },
    /// The copies count was zero.
    #[error("copies must be at least 1, got {0}")]
    InvalidCopies(u32),
    /// Expanding copies overflowed the addressable page count.
    #[error("copy expansion overflowed the page count")]
    CopyOverflow,
    /// The target DPI was zero.
    #[error("DPI must be at least 1, got {0}")]
    InvalidDpi(u32),
    /// A custom scale percentage was not finite or not positive.
    #[error("custom scale percent must be finite and positive, got {0}")]
    InvalidScalePercent(f32),
    /// A computed render scale was not usable.
    #[error("computed render scale {0} is not finite and positive")]
    InvalidScale(f32),
    /// Paper dimensions were not finite and positive.
    #[error("paper '{name}' must have finite, positive dimensions")]
    InvalidPaper {
        /// Name of the offending paper preset.
        name: String,
    },
    /// A margin was negative, non-finite, or larger than the paper.
    #[error("margins must be finite, non-negative, and smaller than the paper")]
    InvalidMargins,
    /// Margins consumed the whole paper along at least one axis.
    #[error("the printable area is empty after subtracting margins")]
    EmptyPrintableArea,
    /// A fit-scale measurement returned a zero-sized raster.
    #[error("cannot fit a zero-sized page")]
    ZeroSizedPage,
    /// The operation was cancelled.
    #[error("printing was cancelled")]
    Cancelled,
    /// The renderer failed; the original error is preserved.
    #[error(transparent)]
    Render(#[from] RenderError),
}

/// The set of document pages selected for printing.
///
/// Every stored page number is 1-based. Use [`PageSelection::parse`] to build a
/// selection from a range expression such as `"1-3,5,7-"`, or build a variant
/// directly. A selection is resolved to zero-based, sorted, deduplicated page
/// indices by [`PrintJob::new`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PageSelection {
    /// Every page in the document.
    #[default]
    All,
    /// One 1-based page number.
    Single(u32),
    /// An inclusive 1-based span. `to == None` extends to the last page.
    Range {
        /// First 1-based page of the span.
        from: u32,
        /// Last 1-based page of the span, or `None` for the last page.
        to: Option<u32>,
    },
    /// An explicit list of 1-based page numbers; duplicates are removed on resolve.
    List(Vec<u32>),
}

impl PageSelection {
    /// Parse a 1-based page-range expression into an explicit [`Self::List`].
    ///
    /// The expression is resolved eagerly against `page_count`, so open spans
    /// such as `"7-"` become concrete page numbers. See [`parse_page_ranges`]
    /// for the exact grammar and error behaviour.
    ///
    /// # Errors
    ///
    /// Propagates every [`PrintError`] from [`parse_page_ranges`].
    pub fn parse(input: &str, page_count: usize) -> Result<Self, PrintError> {
        let pages = parse_page_ranges(input, page_count)?;
        let mut one_based = Vec::with_capacity(pages.len());
        for page in pages {
            let number = u32::try_from(page.saturating_add(1))
                .map_err(|_| PrintError::PageOutOfRange(page as u32, page_count))?;
            one_based.push(number);
        }
        Ok(Self::List(one_based))
    }
}

/// How a page is scaled onto the raster.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub enum PageScaleMode {
    /// Render at the target DPI without resizing (100% / actual size).
    #[default]
    Actual,
    /// Fit the whole page inside the printable area, preserving aspect ratio.
    Fit,
    /// Multiply the actual-size raster by `percent / 100`.
    CustomPercent(f32),
}

/// Page orientation, which swaps the paper's width and height for fitting.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Orientation {
    /// Portrait: use the paper's declared width and height.
    #[default]
    Portrait,
    /// Landscape: swap the paper's declared width and height.
    Landscape,
}

/// Duplex (two-sided) printing mode.
///
/// This is recorded in the job model and passed through to the summary; the
/// raster pipeline itself emits one raster per selected page regardless of the
/// duplex mode. A platform adapter consumes this field when collating sheets.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum Duplex {
    /// Single-sided printing.
    #[default]
    None,
    /// Flip on the long edge of the paper.
    LongEdge,
    /// Flip on the short edge of the paper.
    ShortEdge,
}

/// A named paper size expressed in PDF points (1 inch = 72 points).
#[derive(Clone, Debug, PartialEq)]
pub struct Paper {
    /// Human-readable paper name, for example `"Letter"` or `"A4"`.
    pub name: String,
    /// Paper width in points (before any orientation swap).
    pub width_pt: f32,
    /// Paper height in points (before any orientation swap).
    pub height_pt: f32,
}

impl Paper {
    /// Create a paper preset with a name and dimensions in points.
    pub fn new(name: impl Into<String>, width_pt: f32, height_pt: f32) -> Self {
        Self {
            name: name.into(),
            width_pt,
            height_pt,
        }
    }

    /// US Letter, 8.5 x 11 inches (`612 x 792` points).
    pub fn letter() -> Self {
        Self::new("Letter", 612.0, 792.0)
    }

    /// ISO A4, 210 x 297 mm (approximately `595.28 x 841.89` points).
    pub fn a4() -> Self {
        Self::new("A4", 595.2756, 841.8898)
    }
}

impl Default for Paper {
    fn default() -> Self {
        Self::letter()
    }
}

/// Four page margins expressed in PDF points.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Margins {
    /// Top margin in points.
    pub top_pt: f32,
    /// Right margin in points.
    pub right_pt: f32,
    /// Bottom margin in points.
    pub bottom_pt: f32,
    /// Left margin in points.
    pub left_pt: f32,
}

impl Margins {
    /// Zero margins on every side.
    pub const NONE: Self = Self {
        top_pt: 0.0,
        right_pt: 0.0,
        bottom_pt: 0.0,
        left_pt: 0.0,
    };

    /// Create margins from the four sides, in points.
    pub const fn new(top_pt: f32, right_pt: f32, bottom_pt: f32, left_pt: f32) -> Self {
        Self {
            top_pt,
            right_pt,
            bottom_pt,
            left_pt,
        }
    }
}

/// All job-level settings that affect raster generation.
///
/// Defaults select every page, one copy, actual size at 300 DPI, portrait,
/// single-sided, US Letter paper, and no margins. Call [`Self::validate`]
/// (done automatically by [`PrintJob::new`]) before relying on the settings.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintSettings {
    /// Which pages to print.
    pub selection: PageSelection,
    /// Number of copies of each selected page.
    pub copies: u32,
    /// How each page is scaled onto its raster.
    pub scale: PageScaleMode,
    /// Portrait or landscape orientation.
    pub orientation: Orientation,
    /// Duplex mode recorded for a later platform adapter.
    pub duplex: Duplex,
    /// Paper preset used for fitting and recorded in the job.
    pub paper: Paper,
    /// Page margins in points.
    pub margins: Margins,
    /// Target raster resolution in dots per inch.
    pub dpi: u32,
}

impl Default for PrintSettings {
    fn default() -> Self {
        Self {
            selection: PageSelection::All,
            copies: 1,
            scale: PageScaleMode::Actual,
            orientation: Orientation::Portrait,
            duplex: Duplex::None,
            paper: Paper::default(),
            margins: Margins::NONE,
            dpi: 300,
        }
    }
}

impl PrintSettings {
    /// Validate settings that do not depend on the document's page count.
    ///
    /// Checks the copies and DPI counters, a custom scale percentage, positive
    /// paper dimensions, non-negative margins, and that margins leave a
    /// positive printable area. Page-count-dependent checks (out-of-range page
    /// numbers) happen during [`PrintJob::new`].
    ///
    /// # Errors
    ///
    /// Returns the matching [`PrintError`] variant for the first invalid field.
    pub fn validate(&self) -> Result<(), PrintError> {
        if self.copies == 0 {
            return Err(PrintError::InvalidCopies(self.copies));
        }
        if self.dpi == 0 {
            return Err(PrintError::InvalidDpi(self.dpi));
        }
        if let PageScaleMode::CustomPercent(percent) = self.scale
            && (!percent.is_finite() || percent <= 0.0)
        {
            return Err(PrintError::InvalidScalePercent(percent));
        }
        if !self.paper.width_pt.is_finite()
            || self.paper.width_pt <= 0.0
            || !self.paper.height_pt.is_finite()
            || self.paper.height_pt <= 0.0
        {
            return Err(PrintError::InvalidPaper {
                name: self.paper.name.clone(),
            });
        }
        let margins = [
            self.margins.top_pt,
            self.margins.right_pt,
            self.margins.bottom_pt,
            self.margins.left_pt,
        ];
        if margins
            .iter()
            .any(|value| !value.is_finite() || *value < 0.0)
        {
            return Err(PrintError::InvalidMargins);
        }
        let (width, height) = self.printable_area();
        if width <= 0.0 || height <= 0.0 {
            return Err(PrintError::EmptyPrintableArea);
        }
        Ok(())
    }

    /// Return `(width, height)` of the printable area in points.
    ///
    /// [`Orientation::Landscape`] swaps the paper's width and height before the
    /// left/right and top/bottom margins are subtracted. The caller validates
    /// settings first, so both returned values are positive for valid settings.
    pub fn printable_area(&self) -> (f32, f32) {
        let (width, height) = match self.orientation {
            Orientation::Portrait => (self.paper.width_pt, self.paper.height_pt),
            Orientation::Landscape => (self.paper.height_pt, self.paper.width_pt),
        };
        (
            width - self.margins.left_pt - self.margins.right_pt,
            height - self.margins.top_pt - self.margins.bottom_pt,
        )
    }
}

/// Parse a 1-based page-range expression into sorted, deduplicated, zero-based
/// page indices.
///
/// # Grammar
///
/// The input is a comma-separated list of tokens. Each token is either a
/// single 1-based page (`"5"`) or an inclusive span (`"1-3"`). A missing span
/// start (`"-3"`) means the first page; a missing span end (`"7-"`) means the
/// last page, resolved from `page_count`. Whitespace around tokens and around
/// the hyphen is ignored. An empty (or all-whitespace) input selects every
/// page, equivalent to [`PageSelection::All`].
///
/// # Guarantees
///
/// * All input numbers are 1-based; all returned indices are 0-based.
/// * Duplicate pages are suppressed and the result is sorted ascending.
/// * Any page below `1` or above `page_count` is rejected, as is a reversed
///   span, a malformed token, or a number that does not fit in `u32`.
///
/// # Errors
///
/// Returns [`PrintError::InvalidRange`], [`PrintError::ReversedRange`],
/// [`PrintError::InvalidPage`], or [`PrintError::PageOutOfRange`] for the
/// respective invalid inputs.
pub fn parse_page_ranges(input: &str, page_count: usize) -> Result<Vec<usize>, PrintError> {
    let trimmed = input.trim();
    if trimmed.is_empty() {
        return Ok((0..page_count).collect());
    }

    let mut pages = Vec::new();
    for raw_token in trimmed.split(',') {
        let token = raw_token.trim();
        if token.is_empty() {
            return Err(PrintError::InvalidRange(token.to_owned()));
        }
        if let Some((start_text, end_text)) = token.split_once('-') {
            let start = parse_page_number(start_text.trim(), true, page_count)?;
            let end = parse_page_number(end_text.trim(), false, page_count)?;
            if start > end {
                return Err(PrintError::ReversedRange { start, end });
            }
            for number in start..=end {
                pages.push(push_page(number, page_count)?);
            }
        } else {
            let number = parse_page_number(token, false, page_count)?;
            pages.push(push_page(number, page_count)?);
        }
    }
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

/// Parse one 1-based page number, allowing an omitted bound when `allow_omitted`.
fn parse_page_number(
    text: &str,
    allow_omitted: bool,
    page_count: usize,
) -> Result<u32, PrintError> {
    if text.is_empty() {
        if allow_omitted {
            return Ok(1);
        }
        // An empty upper bound is the open-ended "N-" form.
        return u32::try_from(page_count).map_err(|_| PrintError::CopyOverflow);
    }
    let number = text
        .parse::<u32>()
        .map_err(|_| PrintError::InvalidRange(text.to_owned()))?;
    if number == 0 {
        return Err(PrintError::InvalidPage(number));
    }
    Ok(number)
}

/// Convert a validated 1-based page number to a checked 0-based index.
fn push_page(number: u32, page_count: usize) -> Result<usize, PrintError> {
    if number == 0 {
        return Err(PrintError::InvalidPage(number));
    }
    if number as usize > page_count {
        return Err(PrintError::PageOutOfRange(number, page_count));
    }
    Ok(number as usize - 1)
}

/// Resolve a [`PageSelection`] into sorted, deduplicated, zero-based indices.
fn resolve_selection(
    selection: &PageSelection,
    page_count: usize,
) -> Result<Vec<usize>, PrintError> {
    let mut pages = match selection {
        PageSelection::All => (0..page_count).collect(),
        PageSelection::Single(number) => vec![push_page(*number, page_count)?],
        PageSelection::Range { from, to } => {
            let end = match to {
                Some(value) => *value,
                None => u32::try_from(page_count).map_err(|_| PrintError::CopyOverflow)?,
            };
            if *from > end {
                return Err(PrintError::ReversedRange { start: *from, end });
            }
            let mut span = Vec::with_capacity((end - *from + 1) as usize);
            for number in *from..=end {
                span.push(push_page(number, page_count)?);
            }
            span
        }
        PageSelection::List(list) => {
            let mut resolved = Vec::with_capacity(list.len());
            for &number in list {
                resolved.push(push_page(number, page_count)?);
            }
            resolved
        }
    };
    pages.sort_unstable();
    pages.dedup();
    Ok(pages)
}

/// Resolve a selection expression directly into a settings value.
///
/// Convenience wrapper around [`PageSelection::parse`] for callers that already
/// have a [`PrintSettings`] value and want to replace its selection.
pub fn selection_from_ranges(input: &str, page_count: usize) -> Result<PageSelection, PrintError> {
    PageSelection::parse(input, page_count)
}

/// Expand a resolved page list with the copies count (page-major order).
///
/// For each page, `copies` consecutive rasters are emitted before advancing to
/// the next page. Copies are therefore *uncollated*: pages `[1, 2]` with two
/// copies yield `[1, 1, 2, 2]`.
fn expand_copies(pages: &[usize], copies: u32) -> Result<Vec<usize>, PrintError> {
    let capacity = pages
        .len()
        .checked_mul(copies as usize)
        .ok_or(PrintError::CopyOverflow)?;
    let mut expanded = Vec::with_capacity(capacity);
    for &page in pages {
        for _ in 0..copies {
            expanded.push(page);
        }
    }
    Ok(expanded)
}

/// An immutable print job: validated settings plus the ordered page list.
///
/// Build one with [`Self::new`] (resolving a selection against a page count) or
/// [`Self::from_pages`] (from an already-resolved zero-based page list, for
/// example the output of [`parse_page_ranges`]).
#[derive(Clone, Debug, PartialEq)]
pub struct PrintJob {
    settings: PrintSettings,
    pages: Vec<usize>,
}

impl PrintJob {
    /// Validate `settings`, resolve its selection against `page_count`, and
    /// expand copies into the final ordered page list.
    ///
    /// # Errors
    ///
    /// Returns [`PrintError::EmptySelection`] when the selection resolves to no
    /// pages, and propagates validation and range errors otherwise.
    pub fn new(settings: &PrintSettings, page_count: usize) -> Result<Self, PrintError> {
        settings.validate()?;
        let resolved = resolve_selection(&settings.selection, page_count)?;
        Self::from_resolved(settings, resolved)
    }

    /// Build a job from an explicit zero-based page list.
    ///
    /// The list is sorted and deduplicated, then copies are expanded. Page
    /// numbers are not checked against a document here; the renderer validates
    /// them during spooling. Settings are still validated.
    ///
    /// # Errors
    ///
    /// Returns [`PrintError::EmptySelection`] for an empty list, and propagates
    /// settings validation and copy-overflow errors.
    pub fn from_pages(settings: &PrintSettings, pages: &[usize]) -> Result<Self, PrintError> {
        settings.validate()?;
        let mut resolved = pages.to_vec();
        resolved.sort_unstable();
        resolved.dedup();
        Self::from_resolved(settings, resolved)
    }

    /// Shared constructor for already-resolved, sorted page lists.
    fn from_resolved(settings: &PrintSettings, resolved: Vec<usize>) -> Result<Self, PrintError> {
        if resolved.is_empty() {
            return Err(PrintError::EmptySelection);
        }
        let pages = expand_copies(&resolved, settings.copies)?;
        Ok(Self {
            settings: settings.clone(),
            pages,
        })
    }

    /// Return the settings this job was built from.
    pub fn settings(&self) -> &PrintSettings {
        &self.settings
    }

    /// Return the ordered zero-based page list, including copies.
    pub fn pages(&self) -> &[usize] {
        &self.pages
    }

    /// Return the number of rasters this job will produce.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Return `true` when the job produces no rasters.
    ///
    /// A successfully built job is never empty; this exists for symmetry with
    /// collection APIs.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

/// One raster ready to be handed to a platform printer adapter.
///
/// Pixels are tightly packed 8-bit RGBA rows from the top left, matching
/// [`RenderedPage`]. `dpi` records the resolution the raster was produced at.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintPage {
    /// Zero-based source page this raster depicts.
    pub page: PageIndex,
    /// Raster width in pixels.
    pub width_px: u32,
    /// Raster height in pixels.
    pub height_px: u32,
    /// Tightly packed RGBA pixel buffer.
    pub rgba: Vec<u8>,
    /// Resolution of this raster in dots per inch.
    pub dpi: u32,
}

impl PrintPage {
    /// Return the 1-based source page number for display.
    pub fn page_number(&self) -> u32 {
        (self.page.get() as u32).saturating_add(1)
    }

    /// Return `width_px * height_px` as a checked 64-bit pixel count.
    pub fn pixel_count(&self) -> u64 {
        u64::from(self.width_px) * u64::from(self.height_px)
    }
}

/// A stable summary of a completed spool operation.
///
/// `pages` counts emitted rasters (copies included). `first_page` and
/// `last_page` are zero-based source page indices — `None` only for an empty
/// spool. `total_pixels` is the sum of every raster's pixel count in `u64`, so
/// it cannot overflow for realistic jobs. Two spools of the same job yield
/// identical summaries.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SpoolSummary {
    /// Number of rasters emitted.
    pub pages: usize,
    /// Zero-based index of the first source page, if any.
    pub first_page: Option<usize>,
    /// Zero-based index of the last source page, if any.
    pub last_page: Option<usize>,
    /// Total pixels across all emitted rasters.
    pub total_pixels: u64,
}

/// Rasters from a spool operation together with its stable summary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PrintSpool {
    /// Ordered rasters, one per job page (copies included).
    pub pages: Vec<PrintPage>,
    /// Summary of the operation.
    pub summary: SpoolSummary,
}

/// Stateless spooler that renders a [`PrintJob`] into ready-to-send rasters.
///
/// The spooler produces rasters only. It never opens or talks to a platform
/// printer; a later adapter consumes [`PrintSpool::pages`] to do that.
#[derive(Clone, Copy, Debug, Default)]
pub struct PrintSpooler;

impl PrintSpooler {
    /// Render every page of `job` through `renderer` at the settings' scale.
    ///
    /// Cancellation is checked before the first page and between pages.
    /// Progress is reported as `Progress::new(completed, job.len())` before and
    /// after each page, so reports are monotone from `0` to `N`. For
    /// [`PageScaleMode::Fit`] each page is measured once at scale `1.0` before
    /// the final render; other modes render each page exactly once.
    ///
    /// # Errors
    ///
    /// Returns [`PrintError::Cancelled`] when cancellation fires,
    /// [`PrintError::ZeroSizedPage`] for an unfit-able measurement, and wraps
    /// renderer failures in [`PrintError::Render`] (normalising a renderer's
    /// own `RenderError::Cancelled` to [`PrintError::Cancelled`]).
    pub fn spool(
        &self,
        renderer: &dyn Renderer,
        job: &PrintJob,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<PrintSpool, PrintError> {
        cancellation.check().map_err(|_| PrintError::Cancelled)?;
        let total = job.len();
        let settings = job.settings();
        let mut pages = Vec::with_capacity(total);
        let mut total_pixels = 0u64;
        let mut first_page = None;
        let mut last_page = None;

        for (index, &page_index) in job.pages().iter().enumerate() {
            cancellation.check().map_err(|_| PrintError::Cancelled)?;
            progress(Progress::new(index, total));
            let page = PageIndex::new(page_index);

            let scale = match settings.scale {
                PageScaleMode::Fit => {
                    let measured = render(renderer, page, 1.0, cancellation)?;
                    if measured.width() == 0 || measured.height() == 0 {
                        return Err(PrintError::ZeroSizedPage);
                    }
                    cancellation.check().map_err(|_| PrintError::Cancelled)?;
                    fit_scale(settings, measured.width(), measured.height())?
                }
                mode => fixed_scale(settings, mode)?,
            };

            let rendered = render(renderer, page, scale, cancellation)?;
            let width_px = rendered.width();
            let height_px = rendered.height();
            total_pixels = total_pixels.saturating_add(u64::from(width_px) * u64::from(height_px));
            if first_page.is_none() {
                first_page = Some(page_index);
            }
            last_page = Some(page_index);
            pages.push(PrintPage {
                page,
                width_px,
                height_px,
                rgba: rendered.pixels().to_vec(),
                dpi: settings.dpi,
            });
            progress(Progress::new(index + 1, total));
        }

        Ok(PrintSpool {
            pages,
            summary: SpoolSummary {
                pages: total,
                first_page,
                last_page,
                total_pixels,
            },
        })
    }
}

/// Render one page, normalising renderer cancellation to [`PrintError::Cancelled`].
fn render(
    renderer: &dyn Renderer,
    page: PageIndex,
    scale: f32,
    cancellation: &CancellationToken,
) -> Result<RenderedPage, PrintError> {
    match renderer.render_page(page, scale, cancellation, &mut |_| {}) {
        Ok(rendered) => Ok(rendered),
        Err(RenderError::Cancelled) => Err(PrintError::Cancelled),
        Err(error) => Err(PrintError::Render(error)),
    }
}

/// Compute the render scale for a non-fitting scale mode.
fn fixed_scale(settings: &PrintSettings, mode: PageScaleMode) -> Result<f32, PrintError> {
    let factor = match mode {
        PageScaleMode::Actual => 1.0,
        PageScaleMode::CustomPercent(percent) => {
            if !percent.is_finite() || percent <= 0.0 {
                return Err(PrintError::InvalidScalePercent(percent));
            }
            f64::from(percent) / 100.0
        }
        PageScaleMode::Fit => return Err(PrintError::InvalidScale(f32::NAN)),
    };
    let scale = (f64::from(settings.dpi) / POINTS_PER_INCH) * factor;
    if !scale.is_finite() || scale <= 0.0 || scale > f64::from(f32::MAX) {
        return Err(PrintError::InvalidScale(scale as f32));
    }
    Ok(scale as f32)
}

/// Compute the render scale that fits a measured page in the printable area.
fn fit_scale(
    settings: &PrintSettings,
    base_width: u32,
    base_height: u32,
) -> Result<f32, PrintError> {
    if base_width == 0 || base_height == 0 {
        return Err(PrintError::ZeroSizedPage);
    }
    let (printable_width, printable_height) = settings.printable_area();
    if printable_width <= 0.0 || printable_height <= 0.0 {
        return Err(PrintError::EmptyPrintableArea);
    }
    let ratio = (f64::from(printable_width) / f64::from(base_width))
        .min(f64::from(printable_height) / f64::from(base_height));
    let scale = ratio * (f64::from(settings.dpi) / POINTS_PER_INCH);
    if !scale.is_finite() || scale <= 0.0 || scale > f64::from(f32::MAX) {
        return Err(PrintError::InvalidScale(scale as f32));
    }
    Ok(scale as f32)
}

#[cfg(test)]
mod tests {
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    use super::*;
    use crate::RenderError;

    /// A deterministic, scale-aware renderer with a call counter.
    ///
    /// At scale `s` it emits a `(base_width * s) x (base_height * s)` RGBA page
    /// with a position-derived colour pattern, so raster dimensions reveal the
    /// exact scale chosen by the spooler.
    struct ScaleMock {
        base_width: u32,
        base_height: u32,
        page_count: usize,
        renders: Arc<AtomicUsize>,
    }

    impl ScaleMock {
        fn new(base_width: u32, base_height: u32, page_count: usize) -> Self {
            Self {
                base_width,
                base_height,
                page_count,
                renders: Arc::new(AtomicUsize::new(0)),
            }
        }

        fn render_count(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
        }
    }

    fn scaled_dimension(base: u32, scale: f32) -> u32 {
        let value = (f64::from(base) * f64::from(scale)).round();
        (value as u64).min(u64::from(u32::MAX)) as u32
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
            let width = scaled_dimension(self.base_width, scale);
            let height = scaled_dimension(self.base_height, scale);
            let mut pixels = Vec::with_capacity(width as usize * height as usize * 4);
            for y in 0..height {
                for x in 0..width {
                    pixels.push((x % 251) as u8);
                    pixels.push((y % 251) as u8);
                    pixels.push((page.get() % 251) as u8);
                    pixels.push(255);
                }
            }
            progress(Progress::new(1, 1));
            cancellation.check()?;
            Ok(RenderedPage::new(page, width, height, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    fn spool_with(settings: &PrintSettings, page_count: usize) -> Result<PrintSpool, PrintError> {
        let renderer = ScaleMock::new(200, 100, page_count);
        let job = PrintJob::new(settings, page_count)?;
        PrintSpooler.spool(&renderer, &job, &CancellationToken::new(), &mut |_| {})
    }

    #[test]
    fn parse_handles_singles_closed_ranges_and_open_ranges() {
        // 8 pages: indices 0..=7; input is 1-based.
        assert_eq!(parse_page_ranges("1", 8), Ok(vec![0]));
        assert_eq!(parse_page_ranges("3", 8), Ok(vec![2]));
        assert_eq!(parse_page_ranges("1-3", 8), Ok(vec![0, 1, 2]));
        assert_eq!(parse_page_ranges("7-", 8), Ok(vec![6, 7]));
        assert_eq!(parse_page_ranges("-2", 8), Ok(vec![0, 1]));
        assert_eq!(parse_page_ranges("2 - 4", 8), Ok(vec![1, 2, 3]));
        assert_eq!(parse_page_ranges("", 3), Ok(vec![0, 1, 2]));
        assert_eq!(parse_page_ranges("  ", 3), Ok(vec![0, 1, 2]));
    }

    #[test]
    fn parse_suppresses_duplicates_and_sorts() {
        assert_eq!(parse_page_ranges("5,1-3,2,3", 8), Ok(vec![0, 1, 2, 4]));
        assert_eq!(
            parse_page_ranges("4-2", 8),
            Err(PrintError::ReversedRange { start: 4, end: 2 })
        );
        assert_eq!(
            parse_page_ranges("1,,3", 8),
            Err(PrintError::InvalidRange(String::new()))
        );
        assert_eq!(
            parse_page_ranges("x", 8),
            Err(PrintError::InvalidRange("x".to_owned()))
        );
    }

    #[test]
    fn parse_rejects_out_of_range_and_zero() {
        assert_eq!(parse_page_ranges("0", 8), Err(PrintError::InvalidPage(0)));
        assert_eq!(
            parse_page_ranges("9", 8),
            Err(PrintError::PageOutOfRange(9, 8))
        );
        assert_eq!(
            parse_page_ranges("7-9", 8),
            Err(PrintError::PageOutOfRange(9, 8))
        );
        assert_eq!(
            parse_page_ranges("1-", 0),
            Err(PrintError::ReversedRange { start: 1, end: 0 })
        );
    }

    #[test]
    fn settings_validation_rejects_bad_fields() {
        let mut settings = PrintSettings::default();
        assert_eq!(settings.validate(), Ok(()));

        settings.copies = 0;
        assert_eq!(settings.validate(), Err(PrintError::InvalidCopies(0)));
        settings = PrintSettings::default();

        settings.dpi = 0;
        assert_eq!(settings.validate(), Err(PrintError::InvalidDpi(0)));
        settings = PrintSettings::default();

        settings.scale = PageScaleMode::CustomPercent(f32::NAN);
        assert!(matches!(
            settings.validate(),
            Err(PrintError::InvalidScalePercent(_))
        ));
        settings.scale = PageScaleMode::CustomPercent(-1.0);
        assert!(matches!(
            settings.validate(),
            Err(PrintError::InvalidScalePercent(_))
        ));
        settings = PrintSettings::default();

        settings.paper = Paper::new("bad", 0.0, 10.0);
        assert!(matches!(
            settings.validate(),
            Err(PrintError::InvalidPaper { .. })
        ));
        settings = PrintSettings::default();

        settings.margins = Margins::new(-1.0, 0.0, 0.0, 0.0);
        assert_eq!(settings.validate(), Err(PrintError::InvalidMargins));
        settings.margins = Margins::new(400.0, 0.0, 400.0, 0.0);
        assert_eq!(settings.validate(), Err(PrintError::EmptyPrintableArea));
    }

    #[test]
    fn out_of_range_selection_is_rejected() {
        let settings = PrintSettings {
            selection: PageSelection::Single(99),
            ..PrintSettings::default()
        };
        assert_eq!(
            PrintJob::new(&settings, 3),
            Err(PrintError::PageOutOfRange(99, 3))
        );
        assert_eq!(
            PrintJob::new(
                &PrintSettings {
                    selection: PageSelection::List(vec![1, 0, 2]),
                    ..PrintSettings::default()
                },
                3
            ),
            Err(PrintError::InvalidPage(0))
        );
    }

    #[test]
    fn empty_selection_is_rejected() {
        let settings = PrintSettings {
            selection: PageSelection::List(Vec::new()),
            ..PrintSettings::default()
        };
        assert_eq!(PrintJob::new(&settings, 3), Err(PrintError::EmptySelection));
        assert_eq!(
            PrintJob::new(&PrintSettings::default(), 0),
            Err(PrintError::EmptySelection)
        );
        assert_eq!(
            PrintJob::from_pages(&PrintSettings::default(), &[]),
            Err(PrintError::EmptySelection)
        );
    }

    #[test]
    fn explicit_list_is_sorted_deduplicated_and_page_major_with_copies() {
        let settings = PrintSettings {
            selection: PageSelection::List(vec![3, 1, 2, 1]),
            copies: 2,
            ..PrintSettings::default()
        };
        let job = PrintJob::new(&settings, 4).expect("valid job");
        assert_eq!(job.pages(), &[0, 0, 1, 1, 2, 2]);
        assert_eq!(job.len(), 6);
        assert!(!job.is_empty());
    }

    #[test]
    fn range_and_all_selections_resolve_to_zero_based_pages() {
        let all = PrintJob::new(&PrintSettings::default(), 3).expect("all job");
        assert_eq!(all.pages(), &[0, 1, 2]);

        let range = PrintSettings {
            selection: PageSelection::Range { from: 2, to: None },
            ..PrintSettings::default()
        };
        let job = PrintJob::new(&range, 5).expect("range job");
        assert_eq!(job.pages(), &[1, 2, 3, 4]);

        let parsed = PageSelection::parse("1-3,5", 5).expect("parsed selection");
        assert_eq!(parsed, PageSelection::List(vec![1, 2, 3, 5]));
    }

    #[test]
    fn actual_percent_and_fit_scaling_produce_expected_dimensions() {
        // Actual: 200x100 at 72 DPI stays 200x100.
        let actual = spool_with(
            &PrintSettings {
                dpi: 72,
                ..PrintSettings::default()
            },
            1,
        )
        .expect("actual spool");
        assert_eq!(
            (actual.pages[0].width_px, actual.pages[0].height_px),
            (200, 100)
        );
        assert_eq!(actual.pages[0].dpi, 72);

        // Percent 50 halves and 200 doubles.
        let half = spool_with(
            &PrintSettings {
                dpi: 72,
                scale: PageScaleMode::CustomPercent(50.0),
                ..PrintSettings::default()
            },
            1,
        )
        .expect("half spool");
        assert_eq!((half.pages[0].width_px, half.pages[0].height_px), (100, 50));

        let double = spool_with(
            &PrintSettings {
                dpi: 72,
                scale: PageScaleMode::CustomPercent(200.0),
                ..PrintSettings::default()
            },
            1,
        )
        .expect("double spool");
        assert_eq!(
            (double.pages[0].width_px, double.pages[0].height_px),
            (400, 200)
        );

        // Fit into a 400x200 pt sheet at 72 DPI doubles the 200x100 page.
        let fit = spool_with(
            &PrintSettings {
                dpi: 72,
                scale: PageScaleMode::Fit,
                paper: Paper::new("wide", 400.0, 200.0),
                ..PrintSettings::default()
            },
            1,
        )
        .expect("fit spool");
        assert_eq!((fit.pages[0].width_px, fit.pages[0].height_px), (400, 200));
    }

    #[test]
    fn orientation_and_margins_change_fit_dimensions() {
        let base = PrintSettings {
            dpi: 72,
            scale: PageScaleMode::Fit,
            paper: Paper::new("sheet", 200.0, 100.0),
            margins: Margins::new(10.0, 20.0, 10.0, 20.0),
            ..PrintSettings::default()
        };

        // Portrait printable area is 160x80 pt -> factor 0.8 -> 160x80 px.
        let portrait = spool_with(&base, 1).expect("portrait spool");
        assert_eq!(
            (portrait.pages[0].width_px, portrait.pages[0].height_px),
            (160, 80)
        );

        // Landscape swaps to 100x200 pt; printable is 60x180 -> factor 0.3.
        let landscape = spool_with(
            &PrintSettings {
                orientation: Orientation::Landscape,
                ..base.clone()
            },
            1,
        )
        .expect("landscape spool");
        assert_eq!(
            (landscape.pages[0].width_px, landscape.pages[0].height_px),
            (60, 30)
        );
        assert_eq!(base.printable_area(), (160.0, 80.0));
    }

    #[test]
    fn dpi_scales_raster_dimensions() {
        let settings = PrintSettings {
            dpi: 144,
            ..PrintSettings::default()
        };
        let spool = spool_with(&settings, 1).expect("144 dpi spool");
        assert_eq!(
            (spool.pages[0].width_px, spool.pages[0].height_px),
            (400, 200)
        );
        assert_eq!(spool.pages[0].dpi, 144);
        assert_eq!(spool.pages[0].rgba.len(), 400 * 200 * 4);
        assert_eq!(spool.pages[0].page_number(), 1);
        assert_eq!(spool.pages[0].pixel_count(), 400 * 200);
    }

    #[test]
    fn cancellation_before_spool_does_not_render() {
        let renderer = ScaleMock::new(200, 100, 3);
        let job = PrintJob::new(&PrintSettings::default(), 3).expect("job");
        let token = CancellationToken::new();
        token.cancel();
        let mut reports = Vec::new();
        let result = PrintSpooler.spool(&renderer, &job, &token, &mut |p| reports.push(p));
        assert_eq!(result, Err(PrintError::Cancelled));
        assert_eq!(renderer.render_count(), 0);
        assert!(reports.is_empty());
    }

    #[test]
    fn cancellation_between_pages_stops_after_first_page() {
        let renderer = ScaleMock::new(200, 100, 3);
        let job = PrintJob::new(&PrintSettings::default(), 3).expect("job");
        let token = CancellationToken::new();
        let trigger = token.clone();
        let result = PrintSpooler.spool(&renderer, &job, &token, &mut |p| {
            if p.completed == 1 {
                trigger.cancel();
            }
        });
        assert_eq!(result, Err(PrintError::Cancelled));
        assert_eq!(
            renderer.render_count(),
            1,
            "the second page must not render once cancellation fires"
        );
    }

    #[test]
    fn progress_is_monotone_and_complete() {
        let renderer = ScaleMock::new(200, 100, 3);
        let job = PrintJob::new(&PrintSettings::default(), 3).expect("job");
        let mut reports = Vec::new();
        let _ = PrintSpooler
            .spool(&renderer, &job, &CancellationToken::new(), &mut |p| {
                reports.push(p)
            })
            .expect("spool");
        assert_eq!(reports.first(), Some(&Progress::new(0, 3)));
        assert_eq!(reports.last(), Some(&Progress::new(3, 3)));
        assert!(reports.last().is_some_and(Progress::is_complete));
        assert!(
            reports
                .windows(2)
                .all(|pair| pair[0].completed <= pair[1].completed)
        );
    }

    #[test]
    fn summary_is_stable_and_counts_copies_and_pixels() {
        let settings = PrintSettings {
            copies: 2,
            dpi: 72,
            ..PrintSettings::default()
        };
        let first = spool_with(&settings, 3).expect("first spool");
        let second = spool_with(&settings, 3).expect("second spool");
        assert_eq!(first.summary, second.summary);
        assert_eq!(
            first.summary,
            SpoolSummary {
                pages: 6,
                first_page: Some(0),
                last_page: Some(2),
                total_pixels: 6 * 200 * 100,
            }
        );
        assert_eq!(first.pages, second.pages);
    }

    #[test]
    fn deterministic_order_and_copy_expansion_are_byte_stable() {
        let settings = PrintSettings {
            selection: PageSelection::List(vec![2, 1]),
            copies: 2,
            ..PrintSettings::default()
        };
        let a = spool_with(&settings, 3).expect("spool a");
        let b = spool_with(&settings, 3).expect("spool b");
        assert_eq!(
            a.pages.iter().map(|p| p.page).collect::<Vec<_>>(),
            vec![
                PageIndex::new(0),
                PageIndex::new(0),
                PageIndex::new(1),
                PageIndex::new(1)
            ]
        );
        assert_eq!(a.pages, b.pages);
    }

    #[test]
    fn zero_sized_fit_measurement_is_rejected() {
        let renderer = ScaleMock::new(0, 100, 1);
        let settings = PrintSettings {
            scale: PageScaleMode::Fit,
            ..PrintSettings::default()
        };
        let job = PrintJob::new(&settings, 1).expect("job");
        let result = PrintSpooler.spool(&renderer, &job, &CancellationToken::new(), &mut |_| {});
        assert_eq!(result, Err(PrintError::ZeroSizedPage));
    }
}
