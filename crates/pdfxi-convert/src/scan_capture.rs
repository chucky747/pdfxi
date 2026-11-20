//! Local scanner-capture contract for "scan to PDF" (CREATE-007).
//!
//! ADR 0008 requires a local, caller-supplied equivalent instead of an OS
//! scanner API. TWAIN/WIA (or any equivalent host facility) is expected to have
//! already rasterized each page; this module accepts those rendered images as
//! [`ScanSource`] values and produces a deterministic [`ScanAssemblyPlan`]. No
//! device handle, driver, process, network, or GUI boundary is crossed here.
//!
//! The plan genuinely reuses the existing conversion primitives rather than
//! inventing a second assembly path:
//!
//! - [`crate::image::ImageFormat`] and its documented raster bounds describe
//!   the caller-supplied page images;
//! - [`crate::create::PageSize`] validates every resolved page geometry;
//! - [`crate::print_capture::build_print_capture_plan`] performs the final
//!   ordered assembly, so a scan job hands the PDF writer exactly the same
//!   [`crate::print_capture::PrintCapturePlan`] a print job would.
//!
//! Duplex handling is a pure ordering decision: sheets are grouped by their
//! caller-supplied ordinal and each side is emitted front-first or back-first
//! according to [`DuplexMode`]. Validation, ordering, and the [`ScanReport`]
//! are deterministic, so identical inputs always yield an identical plan.

use crate::create::PageSize;
use crate::image::{ImageFormat, MAX_IMAGE_DIMENSION, MAX_IMAGE_PIXELS};
use crate::print_capture::{
    CapturePage, CaptureSource, PRINT_CAPTURE_MAX_LABEL_BYTES, PRINT_CAPTURE_MAX_PAGES,
    PrintCaptureError, PrintCaptureOptions, PrintCapturePlan, build_print_capture_plan,
};
use std::collections::BTreeMap;
use std::fmt;

/// Minimum accepted scanner resolution in dots per inch.
pub const SCAN_MIN_DPI: f64 = 50.0;
/// Maximum accepted scanner resolution in dots per inch.
pub const SCAN_MAX_DPI: f64 = 1_200.0;
/// Maximum number of scanned pages accepted in one job.
///
/// This reuses the shared print-capture page budget.
pub const SCAN_MAX_PAGES: usize = PRINT_CAPTURE_MAX_PAGES;
/// Maximum length of a caller-supplied scan label, in bytes.
///
/// This reuses the shared print-capture label budget.
pub const SCAN_MAX_LABEL_BYTES: usize = PRINT_CAPTURE_MAX_LABEL_BYTES;

/// Which side of a sheet a captured image came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum ScanSide {
    /// The front (recto) side.
    Front,
    /// The back (verso) side, produced by a duplex transport.
    Back,
}

impl ScanSide {
    /// Return a stable lowercase machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Back => "back",
        }
    }
}

impl fmt::Display for ScanSide {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// How a scanner transport presents sides and how they map to PDF pages.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DuplexMode {
    /// One front image per sheet; a back image is a typed error.
    Simplex,
    /// Two-sided sheets ordered front then back.
    DuplexFrontFirst,
    /// Two-sided sheets ordered back then front (a back-first feeder).
    DuplexBackFirst,
}

impl DuplexMode {
    /// Return a stable lowercase machine-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Simplex => "simplex",
            Self::DuplexFrontFirst => "duplex-front-first",
            Self::DuplexBackFirst => "duplex-back-first",
        }
    }

    /// Return `true` when this mode requires both sides of every sheet.
    #[must_use]
    pub const fn is_duplex(self) -> bool {
        !matches!(self, Self::Simplex)
    }
}

/// One caller-supplied rendered page image from a TWAIN/WIA-equivalent device.
///
/// The host owns the actual pixel buffer; this model carries only the geometry,
/// resolution, format, and sheet/side identity needed to validate and order the
/// page. No image data is copied or decoded here.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanSource {
    sheet: usize,
    side: ScanSide,
    label: String,
    width_px: u32,
    height_px: u32,
    dpi: f64,
    format: ImageFormat,
}

impl ScanSource {
    /// Describe one captured page image.
    ///
    /// `sheet` is a one-based ordinal shared by the two sides of a duplex
    /// sheet. `label` is an opaque diagnostic tag that must be non-empty and at
    /// most [`SCAN_MAX_LABEL_BYTES`] bytes.
    #[must_use]
    pub fn new(
        sheet: usize,
        side: ScanSide,
        label: impl Into<String>,
        width_px: u32,
        height_px: u32,
        dpi: f64,
        format: ImageFormat,
    ) -> Self {
        Self {
            sheet,
            side,
            label: label.into(),
            width_px,
            height_px,
            dpi,
            format,
        }
    }

    /// Return the one-based sheet ordinal.
    #[must_use]
    pub const fn sheet(&self) -> usize {
        self.sheet
    }

    /// Return the captured side.
    #[must_use]
    pub const fn side(&self) -> ScanSide {
        self.side
    }

    /// Return the opaque diagnostic label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the raster width in pixels.
    #[must_use]
    pub const fn width_px(&self) -> u32 {
        self.width_px
    }

    /// Return the raster height in pixels.
    #[must_use]
    pub const fn height_px(&self) -> u32 {
        self.height_px
    }

    /// Return the declared resolution in dots per inch.
    #[must_use]
    pub const fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Return the image format of the caller-held raster.
    #[must_use]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }
}

/// Options controlling a scan-to-PDF assembly job.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ScanOptions {
    duplex: DuplexMode,
    max_pages: usize,
    min_dpi: f64,
    max_dpi: f64,
}

impl ScanOptions {
    /// Default options: simplex, the shared page budget, and
    /// [`SCAN_MIN_DPI`]`..=`[`SCAN_MAX_DPI`] resolution bounds.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            duplex: DuplexMode::Simplex,
            max_pages: SCAN_MAX_PAGES,
            min_dpi: SCAN_MIN_DPI,
            max_dpi: SCAN_MAX_DPI,
        }
    }

    /// Replace the duplex mode.
    #[must_use]
    pub const fn with_duplex(mut self, duplex: DuplexMode) -> Self {
        self.duplex = duplex;
        self
    }

    /// Replace the page-count budget.
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Replace the accepted DPI range.
    ///
    /// Invalid or inverted ranges are surfaced as
    /// [`ScanError::InvalidResolutionRange`] during planning.
    #[must_use]
    pub const fn with_dpi_range(mut self, min_dpi: f64, max_dpi: f64) -> Self {
        self.min_dpi = min_dpi;
        self.max_dpi = max_dpi;
        self
    }

    /// Return the configured duplex mode.
    #[must_use]
    pub const fn duplex(&self) -> DuplexMode {
        self.duplex
    }

    /// Return the page-count budget.
    #[must_use]
    pub const fn max_pages(&self) -> usize {
        self.max_pages
    }

    /// Return the minimum accepted DPI.
    #[must_use]
    pub const fn min_dpi(&self) -> f64 {
        self.min_dpi
    }

    /// Return the maximum accepted DPI.
    #[must_use]
    pub const fn max_dpi(&self) -> f64 {
        self.max_dpi
    }
}

impl Default for ScanOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A typed failure while validating or assembling a scan job.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ScanError {
    /// No scanned pages were supplied.
    #[error("scan job has no pages")]
    EmptyInput,
    /// More pages were supplied than the configured budget.
    #[error("scan job has {count} pages (limit {limit})")]
    TooManyPages {
        /// Number of supplied pages.
        count: usize,
        /// Enforced page budget.
        limit: usize,
    },
    /// The configured DPI range is not usable.
    #[error("invalid scan DPI range {min}..={max}")]
    InvalidResolutionRange {
        /// Configured lower bound.
        min: f64,
        /// Configured upper bound.
        max: f64,
    },
    /// A sheet ordinal was zero.
    #[error("sheet ordinal must be positive, got {sheet}")]
    InvalidSheet {
        /// The offending ordinal.
        sheet: usize,
    },
    /// A label was empty or exceeded the documented byte bound.
    #[error("invalid scan label for sheet {sheet} {side}: {reason}")]
    InvalidLabel {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Captured side.
        side: ScanSide,
        /// Stable explanation.
        reason: &'static str,
    },
    /// The declared resolution was not finite or fell outside the range.
    #[error("invalid scan resolution for sheet {sheet} {side}: {dpi}")]
    InvalidResolution {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Captured side.
        side: ScanSide,
        /// Declared resolution.
        dpi: f64,
    },
    /// The raster geometry exceeded the documented image bounds.
    #[error("invalid scan geometry for sheet {sheet} {side}: {width_px}x{height_px}")]
    InvalidGeometry {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Captured side.
        side: ScanSide,
        /// Declared raster width.
        width_px: u32,
        /// Declared raster height.
        height_px: u32,
    },
    /// The resolved page size was rejected by the shared geometry validator.
    #[error("invalid scan page size for sheet {sheet} {side}: {reason}")]
    InvalidPageSize {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Captured side.
        side: ScanSide,
        /// Reason from the shared validator.
        reason: String,
    },
    /// Both sides of one sheet declared the same identity.
    #[error("duplicate {side} scan for sheet {sheet}")]
    DuplicateSheetSide {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Repeated side.
        side: ScanSide,
    },
    /// A duplex job was missing a required side.
    #[error("missing {side} scan for sheet {sheet}")]
    MissingSide {
        /// Offending sheet ordinal.
        sheet: usize,
        /// Required but absent side.
        side: ScanSide,
    },
    /// A simplex job contained a back-side image.
    #[error("simplex scan has a back image for sheet {sheet}")]
    UnexpectedBackSide {
        /// Offending sheet ordinal.
        sheet: usize,
    },
    /// The reused print-capture assembly rejected a resolved page.
    #[error("print-capture assembly rejected the scan: {0}")]
    PrintCapture(String),
}

impl ScanError {
    /// Map a print-capture failure onto the scan error taxonomy.
    fn from_print_capture(error: PrintCaptureError) -> Self {
        match error {
            PrintCaptureError::TooManyPages { count, limit } => Self::TooManyPages { count, limit },
            other => Self::PrintCapture(other.to_string()),
        }
    }
}

/// One validated, ordered page in a scan assembly plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedScanPage {
    sheet: usize,
    side: ScanSide,
    label: String,
    width_px: u32,
    height_px: u32,
    dpi: f64,
    format: ImageFormat,
    page_size: PageSize,
}

impl PlannedScanPage {
    /// Return the source sheet ordinal.
    #[must_use]
    pub const fn sheet(&self) -> usize {
        self.sheet
    }

    /// Return the captured side.
    #[must_use]
    pub const fn side(&self) -> ScanSide {
        self.side
    }

    /// Return the opaque diagnostic label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the raster width in pixels.
    #[must_use]
    pub const fn width_px(&self) -> u32 {
        self.width_px
    }

    /// Return the raster height in pixels.
    #[must_use]
    pub const fn height_px(&self) -> u32 {
        self.height_px
    }

    /// Return the resolution used to derive the page size.
    #[must_use]
    pub const fn dpi(&self) -> f64 {
        self.dpi
    }

    /// Return the caller-held raster format.
    #[must_use]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Return the validated page geometry in PDF points.
    #[must_use]
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }
}

/// A deterministic report describing what a scan plan will produce.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanReport {
    page_count: usize,
    front_count: usize,
    back_count: usize,
    duplex: DuplexMode,
    summary: String,
}

impl ScanReport {
    /// Return the number of planned pages.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Return the number of front-side pages.
    #[must_use]
    pub const fn front_count(&self) -> usize {
        self.front_count
    }

    /// Return the number of back-side pages.
    #[must_use]
    pub const fn back_count(&self) -> usize {
        self.back_count
    }

    /// Return the transport mode used to build the plan.
    #[must_use]
    pub const fn duplex(&self) -> DuplexMode {
        self.duplex
    }

    /// Return a stable, human-readable summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A deterministic assembly plan for a scan job.
///
/// The plan carries both the ordered, validated scan pages and the
/// [`PrintCapturePlan`] produced by the shared assembly path. A host can hand
/// the latter to the existing PDF writers together with the caller-held raster
/// bytes described by each page.
#[derive(Debug, Clone, PartialEq)]
pub struct ScanAssemblyPlan {
    pages: Vec<PlannedScanPage>,
    capture: PrintCapturePlan,
    report: ScanReport,
}

impl ScanAssemblyPlan {
    /// Return the ordered, validated scan pages.
    #[must_use]
    pub fn pages(&self) -> &[PlannedScanPage] {
        &self.pages
    }

    /// Return the number of planned pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Return the reused print-capture assembly plan.
    #[must_use]
    pub fn capture_plan(&self) -> &PrintCapturePlan {
        &self.capture
    }

    /// Return the deterministic scan report.
    #[must_use]
    pub fn report(&self) -> &ScanReport {
        &self.report
    }

    /// Return the stable report summary.
    #[must_use]
    pub fn summary(&self) -> &str {
        self.report.summary()
    }
}

/// Validate one source against the configured bounds.
fn validate_source(source: &ScanSource, options: &ScanOptions) -> Result<(), ScanError> {
    if source.label.is_empty() || source.label.len() > SCAN_MAX_LABEL_BYTES {
        let reason = if source.label.is_empty() {
            "label must not be empty"
        } else {
            "label exceeds the byte bound"
        };
        return Err(ScanError::InvalidLabel {
            sheet: source.sheet,
            side: source.side,
            reason,
        });
    }

    let width = usize::try_from(source.width_px).map_err(|_| ScanError::InvalidGeometry {
        sheet: source.sheet,
        side: source.side,
        width_px: source.width_px,
        height_px: source.height_px,
    })?;
    let height = usize::try_from(source.height_px).map_err(|_| ScanError::InvalidGeometry {
        sheet: source.sheet,
        side: source.side,
        width_px: source.width_px,
        height_px: source.height_px,
    })?;
    let pixels = width.checked_mul(height);
    if source.width_px == 0
        || source.height_px == 0
        || source.width_px > MAX_IMAGE_DIMENSION
        || source.height_px > MAX_IMAGE_DIMENSION
        || pixels.is_none_or(|count| count > MAX_IMAGE_PIXELS)
    {
        return Err(ScanError::InvalidGeometry {
            sheet: source.sheet,
            side: source.side,
            width_px: source.width_px,
            height_px: source.height_px,
        });
    }

    if !source.dpi.is_finite() || !(options.min_dpi..=options.max_dpi).contains(&source.dpi) {
        return Err(ScanError::InvalidResolution {
            sheet: source.sheet,
            side: source.side,
            dpi: source.dpi,
        });
    }

    let width_pt = f64::from(source.width_px) / source.dpi * 72.0;
    let height_pt = f64::from(source.height_px) / source.dpi * 72.0;
    PageSize::new(width_pt, height_pt).map_err(|error| {
        let reason = match error {
            crate::CreateError::InvalidPageSize(reason) => reason,
            other => other.to_string(),
        };
        ScanError::InvalidPageSize {
            sheet: source.sheet,
            side: source.side,
            reason,
        }
    })?;
    Ok(())
}

/// Resolve one source into a validated planned page.
fn resolve_page(source: &ScanSource) -> PlannedScanPage {
    let width_pt = f64::from(source.width_px) / source.dpi * 72.0;
    let height_pt = f64::from(source.height_px) / source.dpi * 72.0;
    // `validate_source` already proved this geometry is accepted by
    // `PageSize::new`; keeping the fallback avoids a panic on future refactors.
    let page_size = PageSize::new(width_pt, height_pt).unwrap_or(PageSize::LETTER);
    PlannedScanPage {
        sheet: source.sheet,
        side: source.side,
        label: source.label.clone(),
        width_px: source.width_px,
        height_px: source.height_px,
        dpi: source.dpi,
        format: source.format,
        page_size,
    }
}

/// Build an ordered page list from validated sources grouped by sheet.
fn order_sources<'a>(
    sources: &'a [ScanSource],
    options: &ScanOptions,
) -> Result<Vec<&'a ScanSource>, ScanError> {
    let mut by_sheet: BTreeMap<usize, BTreeMap<ScanSide, &'a ScanSource>> = BTreeMap::new();
    for source in sources {
        validate_source(source, options)?;
        if source.sheet == 0 {
            return Err(ScanError::InvalidSheet { sheet: 0 });
        }
        let sides = by_sheet.entry(source.sheet).or_default();
        if sides.insert(source.side, source).is_some() {
            return Err(ScanError::DuplicateSheetSide {
                sheet: source.sheet,
                side: source.side,
            });
        }
    }

    let mut ordered: Vec<&'a ScanSource> = Vec::with_capacity(sources.len());
    for (sheet, sides) in by_sheet {
        match options.duplex {
            DuplexMode::Simplex => {
                if sides.contains_key(&ScanSide::Back) {
                    return Err(ScanError::UnexpectedBackSide { sheet });
                }
                let front = sides.get(&ScanSide::Front).ok_or(ScanError::MissingSide {
                    sheet,
                    side: ScanSide::Front,
                })?;
                ordered.push(front);
            }
            DuplexMode::DuplexFrontFirst | DuplexMode::DuplexBackFirst => {
                let front = sides.get(&ScanSide::Front).ok_or(ScanError::MissingSide {
                    sheet,
                    side: ScanSide::Front,
                })?;
                let back = sides.get(&ScanSide::Back).ok_or(ScanError::MissingSide {
                    sheet,
                    side: ScanSide::Back,
                })?;
                if options.duplex == DuplexMode::DuplexFrontFirst {
                    ordered.push(front);
                    ordered.push(back);
                } else {
                    ordered.push(back);
                    ordered.push(front);
                }
            }
        }
    }
    Ok(ordered)
}

/// Build a deterministic assembly plan from caller-supplied scanned pages.
///
/// Sources are validated, grouped by `sheet`, and ordered according to
/// `options.duplex` before being handed to
/// [`build_print_capture_plan`]. The result is independent of input ordering,
/// and repeated calls with equivalent sources produce byte-for-byte equal
/// reports.
///
/// # Errors
///
/// Returns a typed [`ScanError`] for an empty or over-budget job, malformed
/// source metadata, a duplex/simplex mismatch, or a failure from the reused
/// print-capture assembly path.
pub fn build_scan_plan(
    sources: &[ScanSource],
    options: &ScanOptions,
) -> Result<ScanAssemblyPlan, ScanError> {
    if sources.is_empty() {
        return Err(ScanError::EmptyInput);
    }
    if sources.len() > options.max_pages {
        return Err(ScanError::TooManyPages {
            count: sources.len(),
            limit: options.max_pages,
        });
    }
    if !options.min_dpi.is_finite()
        || !options.max_dpi.is_finite()
        || options.min_dpi <= 0.0
        || options.min_dpi > options.max_dpi
    {
        return Err(ScanError::InvalidResolutionRange {
            min: options.min_dpi,
            max: options.max_dpi,
        });
    }

    let ordered = order_sources(sources, options)?;
    let pages: Vec<PlannedScanPage> = ordered.iter().map(|source| resolve_page(source)).collect();

    // Reuse the print-capture assembly path so the PDF writer receives the
    // same plan shape it receives from a virtual printer.
    let capture_pages: Vec<CapturePage> = pages
        .iter()
        .enumerate()
        .map(|(index, page)| {
            CapturePage::new(
                index + 1,
                page.label.clone(),
                CaptureSource::RenderedImage {
                    width_px: page.width_px,
                    height_px: page.height_px,
                    dpi: page.dpi,
                },
            )
        })
        .collect();
    let capture = build_print_capture_plan(
        &capture_pages,
        &PrintCaptureOptions::new().with_max_pages(options.max_pages),
    )
    .map_err(ScanError::from_print_capture)?;

    let front_count = pages
        .iter()
        .filter(|page| page.side == ScanSide::Front)
        .count();
    let back_count = pages.len() - front_count;
    let summary = format!(
        "{} pages [{}] duplex={} front={} back={}",
        pages.len(),
        capture.summary(),
        options.duplex.label(),
        front_count,
        back_count
    );
    let report = ScanReport {
        page_count: pages.len(),
        front_count,
        back_count,
        duplex: options.duplex,
        summary,
    };

    Ok(ScanAssemblyPlan {
        pages,
        capture,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn source(sheet: usize, side: ScanSide, width: u32, height: u32, dpi: f64) -> ScanSource {
        ScanSource::new(
            sheet,
            side,
            format!("sheet-{sheet}-{side}"),
            width,
            height,
            dpi,
            ImageFormat::Png,
        )
    }

    fn front(sheet: usize) -> ScanSource {
        source(sheet, ScanSide::Front, 200, 100, 100.0)
    }

    fn back(sheet: usize) -> ScanSource {
        source(sheet, ScanSide::Back, 200, 100, 100.0)
    }

    #[test]
    fn simplex_plan_orders_sheets_and_rejects_back_images() {
        let sources = vec![front(2), front(1)];
        let plan = build_scan_plan(&sources, &ScanOptions::new()).expect("simplex plan");
        let order: Vec<(usize, ScanSide)> = plan
            .pages()
            .iter()
            .map(|page| (page.sheet(), page.side()))
            .collect();
        assert_eq!(order, vec![(1, ScanSide::Front), (2, ScanSide::Front)]);
        assert_eq!(plan.report().front_count(), 2);
        assert_eq!(plan.report().back_count(), 0);
        assert_eq!(plan.report().duplex(), DuplexMode::Simplex);

        let error = build_scan_plan(&[front(1), back(1)], &ScanOptions::new())
            .expect_err("a simplex job must reject a back image");
        assert_eq!(error, ScanError::UnexpectedBackSide { sheet: 1 });
    }

    #[test]
    fn duplex_front_first_page_order_is_sheet_then_front_back() {
        let sources = vec![back(2), front(1), back(1), front(2)];
        let options = ScanOptions::new().with_duplex(DuplexMode::DuplexFrontFirst);
        let plan = build_scan_plan(&sources, &options).expect("duplex plan");
        let order: Vec<(usize, ScanSide)> = plan
            .pages()
            .iter()
            .map(|page| (page.sheet(), page.side()))
            .collect();
        assert_eq!(
            order,
            vec![
                (1, ScanSide::Front),
                (1, ScanSide::Back),
                (2, ScanSide::Front),
                (2, ScanSide::Back),
            ]
        );
        assert_eq!(plan.report().front_count(), 2);
        assert_eq!(plan.report().back_count(), 2);
    }

    #[test]
    fn duplex_back_first_page_order_is_sheet_then_back_front() {
        let sources = vec![front(1), back(1), front(2), back(2)];
        let options = ScanOptions::new().with_duplex(DuplexMode::DuplexBackFirst);
        let plan = build_scan_plan(&sources, &options).expect("duplex plan");
        let order: Vec<(usize, ScanSide)> = plan
            .pages()
            .iter()
            .map(|page| (page.sheet(), page.side()))
            .collect();
        assert_eq!(
            order,
            vec![
                (1, ScanSide::Back),
                (1, ScanSide::Front),
                (2, ScanSide::Back),
                (2, ScanSide::Front),
            ]
        );
        assert!(options.duplex().is_duplex());
    }

    #[test]
    fn invalid_dpi_and_page_limits_are_rejected() {
        let low = build_scan_plan(
            &[source(1, ScanSide::Front, 100, 100, 1.0)],
            &ScanOptions::new(),
        )
        .expect_err("DPI below the minimum must be rejected");
        assert!(matches!(low, ScanError::InvalidResolution { sheet: 1, .. }));

        let nan = build_scan_plan(
            &[source(1, ScanSide::Front, 100, 100, f64::NAN)],
            &ScanOptions::new(),
        )
        .expect_err("non-finite DPI must be rejected");
        assert!(matches!(nan, ScanError::InvalidResolution { .. }));

        let options = ScanOptions::new().with_max_pages(1);
        let error = build_scan_plan(&[front(1), front(2)], &options)
            .expect_err("over-budget scan must be rejected");
        assert!(matches!(
            error,
            ScanError::TooManyPages { count: 2, limit: 1 }
        ));

        let inverted =
            build_scan_plan(&[front(1)], &ScanOptions::new().with_dpi_range(200.0, 50.0))
                .expect_err("an inverted DPI range must be rejected");
        assert!(matches!(inverted, ScanError::InvalidResolutionRange { .. }));
    }

    #[test]
    fn label_sheet_and_geometry_validation_reuses_image_bounds() {
        let blank = ScanSource::new(1, ScanSide::Front, "", 10, 10, 100.0, ImageFormat::Png);
        let error = build_scan_plan(&[blank], &ScanOptions::new())
            .expect_err("blank label must be rejected");
        assert!(matches!(error, ScanError::InvalidLabel { .. }));

        let zero_sheet = source(0, ScanSide::Front, 10, 10, 100.0);
        assert_eq!(
            build_scan_plan(&[zero_sheet], &ScanOptions::new()).expect_err("sheet 0"),
            ScanError::InvalidSheet { sheet: 0 }
        );

        let oversized = source(1, ScanSide::Front, MAX_IMAGE_DIMENSION + 1, 1, 100.0);
        let error = build_scan_plan(&[oversized], &ScanOptions::new())
            .expect_err("image bounds must be reused");
        assert!(matches!(error, ScanError::InvalidGeometry { .. }));
    }

    #[test]
    fn empty_duplicate_and_missing_sides_have_typed_errors() {
        assert_eq!(
            build_scan_plan(&[], &ScanOptions::new()).expect_err("empty job"),
            ScanError::EmptyInput
        );

        let error = build_scan_plan(&[front(1), front(1)], &ScanOptions::new())
            .expect_err("duplicate side must be rejected");
        assert_eq!(
            error,
            ScanError::DuplicateSheetSide {
                sheet: 1,
                side: ScanSide::Front
            }
        );

        let options = ScanOptions::new().with_duplex(DuplexMode::DuplexFrontFirst);
        let error = build_scan_plan(&[front(1)], &options)
            .expect_err("a duplex job must require both sides");
        assert_eq!(
            error,
            ScanError::MissingSide {
                sheet: 1,
                side: ScanSide::Back
            }
        );
    }

    #[test]
    fn assembly_is_deterministic_and_reuses_print_capture_plan() {
        let sources = vec![back(2), front(1), back(1), front(2)];
        let options = ScanOptions::new().with_duplex(DuplexMode::DuplexFrontFirst);
        let reordered = vec![front(2), back(1), front(1), back(2)];
        let first = build_scan_plan(&sources, &options).expect("first plan");
        let second = build_scan_plan(&reordered, &options).expect("second plan");
        assert_eq!(first, second);
        assert_eq!(first.summary(), second.summary());
        assert_eq!(first.page_count(), first.capture_plan().page_count());
        assert_eq!(
            first.capture_plan().pages()[0].source_kind(),
            "rendered-image"
        );
    }

    #[test]
    fn resolved_page_size_is_derived_from_pixels_and_dpi() {
        let plan = build_scan_plan(&[front(1)], &ScanOptions::new()).expect("plan");
        let size = plan.pages()[0].page_size();
        assert!(
            (size.width - 144.0).abs() < 1e-9,
            "200 px at 100 dpi = 144 pt"
        );
        assert!(
            (size.height - 72.0).abs() < 1e-9,
            "100 px at 100 dpi = 72 pt"
        );
        assert_eq!(plan.pages()[0].format(), ImageFormat::Png);
        assert!(plan.summary().contains("duplex=simplex"));
    }

    #[test]
    fn error_messages_are_actionable() {
        assert!(ScanError::EmptyInput.to_string().contains("no pages"));
        assert!(
            ScanError::UnexpectedBackSide { sheet: 3 }
                .to_string()
                .contains("sheet 3")
        );
        assert_eq!(ScanSide::Front.to_string(), "front");
        assert_eq!(DuplexMode::DuplexBackFirst.label(), "duplex-back-first");
    }
}
