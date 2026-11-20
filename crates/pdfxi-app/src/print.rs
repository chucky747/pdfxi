//! Window-free print model and deterministic print plans.
//!
//! This module is the app-level front end to
//! [`pdfxi_render::print`]. It models the choices a user
//! makes in a print dialog: page range (all, current, selection, or a custom
//! 1-based expression), copies, orientation, fit/actual/custom scale, duplex,
//! and paper size. It validates them with typed errors and resolves them into a
//! [`PrintPlan`]. A plan carries the zero-based page list, the exact
//! [`PrintSettings`] to hand to `pdfxi_render::print::PrintSpooler`, and a stable
//! summary string.
//!
//! # Host boundary
//!
//! Nothing in this module opens or talks to a platform printer. Building a plan
//! only resolves data and never performs I/O. A host adapter consumes
//! [`PrintPlan::settings`] and [`PrintPlan::pages`] through
//! `pdfxi_render::print::PrintSpooler` to produce rasters, then forwards those
//! rasters to a driver. This split keeps dialog state testable without a window
//! and is why real printing remains a later adapter.

use pdfxi_render::{
    Duplex, Margins, Orientation, PageScaleMode, PageSelection, Paper, PrintError, PrintJob,
    PrintSettings, parse_page_ranges,
};

/// Default raster resolution used by generated print plans (dots per inch).
pub const DEFAULT_PRINT_DPI: u32 = 300;

/// Which document pages a print job should include.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub enum PrintPageRange {
    /// Every page in the document.
    #[default]
    All,
    /// Only the active tab's current page.
    Current,
    /// A caller-supplied set of zero-based pages (for example a selection).
    Selection,
    /// A validated 1-based range expression such as "1-3,5,7-".
    Custom(String),
}

/// A named paper size with deterministic points-based geometry.
#[derive(Clone, Debug, Default, PartialEq)]
pub enum PaperSize {
    /// US Letter, 8.5 by 11 inches.
    #[default]
    Letter,
    /// ISO A4, 210 by 297 millimetres.
    A4,
    /// US Legal, 8.5 by 14 inches.
    Legal,
    /// A caller-defined paper with a name and dimensions in points.
    Custom {
        /// Human-readable paper name.
        name: String,
        /// Width in points.
        width_pt: f32,
        /// Height in points.
        height_pt: f32,
    },
}

impl PaperSize {
    /// Stable human-readable name of this paper.
    pub fn name(&self) -> &str {
        match self {
            Self::Letter => "Letter",
            Self::A4 => "A4",
            Self::Legal => "Legal",
            Self::Custom { name, .. } => name,
        }
    }

    /// Return `true` when the paper has finite, positive dimensions.
    pub fn is_valid(&self) -> bool {
        match self {
            Self::Letter | Self::A4 | Self::Legal => true,
            Self::Custom {
                width_pt,
                height_pt,
                ..
            } => {
                width_pt.is_finite() && *width_pt > 0.0 && height_pt.is_finite() && *height_pt > 0.0
            }
        }
    }

    /// Convert to the renderer's points-based [`Paper`] value.
    pub fn to_render_paper(&self) -> Paper {
        match self {
            Self::Letter => Paper::letter(),
            Self::A4 => Paper::a4(),
            Self::Legal => Paper::new("Legal", 612.0, 1008.0),
            Self::Custom {
                name,
                width_pt,
                height_pt,
            } => Paper::new(name.clone(), *width_pt, *height_pt),
        }
    }
}

/// The user-editable print choices, independent of any window or document.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintModel {
    /// Pages to include.
    pub range: PrintPageRange,
    /// Number of copies of each selected page.
    pub copies: u32,
    /// Portrait or landscape orientation.
    pub orientation: Orientation,
    /// Fit, actual-size, or custom-percent scaling.
    pub scale: PageScaleMode,
    /// Duplex mode recorded for the host adapter.
    pub duplex: Duplex,
    /// Paper size used for fitting and recorded in the plan.
    pub paper: PaperSize,
}

impl Default for PrintModel {
    fn default() -> Self {
        Self {
            range: PrintPageRange::All,
            copies: 1,
            orientation: Orientation::Portrait,
            scale: PageScaleMode::Actual,
            duplex: Duplex::None,
            paper: PaperSize::default(),
        }
    }
}

impl PrintModel {
    /// Validate settings that do not depend on a document.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PrintModelError`] for zero copies, a non-finite or
    /// non-positive custom scale, or an invalid custom paper size.
    pub fn validate(&self) -> Result<(), PrintModelError> {
        if self.copies == 0 {
            return Err(PrintModelError::InvalidCopies(self.copies));
        }
        if let PageScaleMode::CustomPercent(percent) = self.scale
            && (!percent.is_finite() || percent <= 0.0)
        {
            return Err(PrintModelError::InvalidScale(percent));
        }
        if !self.paper.is_valid() {
            return Err(PrintModelError::InvalidPaper);
        }
        Ok(())
    }

    /// Resolve these choices against `context` into a deterministic [`PrintPlan`].
    ///
    /// The page range resolves to zero-based pages, sorted and deduplicated,
    /// then expanded page-major by `pdfxi_render::print::PrintJob`. The settings
    /// are fully populated so a host can spool the job without interpretation.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PrintModelError`] for invalid settings, an empty
    /// document or selection, out-of-range current/selection pages, or a
    /// malformed custom range expression.
    pub fn build_plan(&self, context: &PrintContext) -> Result<PrintPlan, PrintModelError> {
        self.validate()?;
        if context.page_count == 0 {
            return Err(PrintModelError::EmptySelection);
        }

        let mut pages = match &self.range {
            PrintPageRange::All => (0..context.page_count).collect::<Vec<_>>(),
            PrintPageRange::Current => {
                if context.current_page >= context.page_count {
                    return Err(PrintModelError::CurrentPageOutOfRange(
                        context.current_page,
                        context.page_count,
                    ));
                }
                vec![context.current_page]
            }
            PrintPageRange::Selection => {
                for &page in &context.selected_pages {
                    if page >= context.page_count {
                        return Err(PrintModelError::SelectionOutOfRange(
                            page,
                            context.page_count,
                        ));
                    }
                }
                context.selected_pages.clone()
            }
            PrintPageRange::Custom(expression) => {
                parse_page_ranges(expression, context.page_count).map_err(map_print_error)?
            }
        };
        pages.sort_unstable();
        pages.dedup();
        if pages.is_empty() {
            return Err(PrintModelError::EmptySelection);
        }

        let mut one_based = Vec::with_capacity(pages.len());
        for &page in &pages {
            let number = u32::try_from(page.saturating_add(1))
                .map_err(|_| PrintModelError::Settings("page number is too large".to_owned()))?;
            one_based.push(number);
        }

        let settings = PrintSettings {
            selection: PageSelection::List(one_based),
            copies: self.copies,
            scale: self.scale,
            orientation: self.orientation,
            duplex: self.duplex,
            paper: self.paper.to_render_paper(),
            margins: Margins::NONE,
            dpi: DEFAULT_PRINT_DPI,
        };
        let job = PrintJob::new(&settings, context.page_count).map_err(map_print_error)?;
        let resolved = job.pages().to_vec();
        let summary = format!(
            "{} page(s); {} copy/copies; {} scale; {:?} orientation; {:?} duplex; {} paper",
            resolved.len(),
            self.copies,
            scale_label(self.scale),
            self.orientation,
            self.duplex,
            self.paper.name()
        );
        Ok(PrintPlan {
            pages: resolved,
            settings,
            summary,
        })
    }
}

/// Stable text label for a scale mode, used in plan summaries.
fn scale_label(scale: PageScaleMode) -> String {
    match scale {
        PageScaleMode::Actual => "actual".to_owned(),
        PageScaleMode::Fit => "fit".to_owned(),
        PageScaleMode::CustomPercent(percent) => format!("{percent}%"),
    }
}
/// Typed errors produced while validating or resolving a [`PrintModel`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PrintModelError {
    /// A custom page-range expression was malformed or out of range.
    #[error("invalid page range: {0}")]
    InvalidRange(String),
    /// The resolved selection was empty.
    #[error("no pages were selected for printing")]
    EmptySelection,
    /// The copies count was zero.
    #[error("copies must be at least 1, got {0}")]
    InvalidCopies(u32),
    /// A custom scale percentage was not finite or not positive.
    #[error("custom scale percent must be finite and positive, got {0}")]
    InvalidScale(f32),
    /// The current page was beyond the document's page count.
    #[error("current page {0} is out of range for {1} page(s)")]
    CurrentPageOutOfRange(usize, usize),
    /// A selected page was beyond the document's page count.
    #[error("selected page {0} is out of range for {1} page(s)")]
    SelectionOutOfRange(usize, usize),
    /// A custom paper had invalid dimensions.
    #[error("paper size is invalid")]
    InvalidPaper,
    /// Another print setting was invalid.
    #[error("print settings are invalid: {0}")]
    Settings(String),
}

/// Translate a renderer `PrintError` into the app-level typed error.
fn map_print_error(error: PrintError) -> PrintModelError {
    match &error {
        PrintError::EmptySelection => PrintModelError::EmptySelection,
        PrintError::InvalidCopies(copies) => PrintModelError::InvalidCopies(*copies),
        PrintError::InvalidScalePercent(percent) | PrintError::InvalidScale(percent) => {
            PrintModelError::InvalidScale(*percent)
        }
        PrintError::InvalidPaper { .. } => PrintModelError::InvalidPaper,
        PrintError::InvalidRange(_)
        | PrintError::ReversedRange { .. }
        | PrintError::InvalidPage(_)
        | PrintError::PageOutOfRange(..) => PrintModelError::InvalidRange(error.to_string()),
        _ => PrintModelError::Settings(error.to_string()),
    }
}

/// Document context used to resolve a [`PrintModel`] into a plan.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PrintContext {
    /// Total number of pages in the document.
    pub page_count: usize,
    /// Zero-based current page of the active tab.
    pub current_page: usize,
    /// Zero-based selected pages, if any.
    pub selected_pages: Vec<usize>,
}

impl PrintContext {
    /// Build a context from page count, current page, and a selection slice.
    pub fn new(page_count: usize, current_page: usize, selected_pages: &[usize]) -> Self {
        Self {
            page_count,
            current_page,
            selected_pages: selected_pages.to_vec(),
        }
    }
}

/// A resolved, deterministic print job ready for a host to execute.
#[derive(Clone, Debug, PartialEq)]
pub struct PrintPlan {
    /// Zero-based source pages after sorting, deduplication, and copy expansion.
    pub pages: Vec<usize>,
    /// Settings to pass to `pdfxi_render::print::PrintSpooler`.
    pub settings: PrintSettings,
    /// Deterministic, human-readable summary of the plan.
    pub summary: String,
}

impl PrintPlan {
    /// Return the number of rasters this plan will produce (copies included).
    pub fn raster_count(&self) -> usize {
        self.pages.len()
    }

    /// Return `true` when the plan produces no rasters.
    ///
    /// A successfully built plan is never empty; this exists for API symmetry.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Return the one-based page numbers in emission order.
    pub fn page_numbers(&self) -> Vec<u32> {
        self.pages
            .iter()
            .map(|&page| (page as u32).saturating_add(1))
            .collect()
    }

    /// Return the duplex mode recorded for the host adapter.
    pub fn duplex(&self) -> Duplex {
        self.settings.duplex
    }
}

/// Window-free state for a print dialog.
///
/// The dialog edits a [`PrintModel`] and submits it through [`Self::submit`],
/// which only resolves a plan and records status. It never calls a printer; a
/// host adapter consumes the returned plan at the documented boundary.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct PrintDialogState {
    open: bool,
    model: PrintModel,
    last_plan: Option<PrintPlan>,
    last_error: Option<PrintModelError>,
    status: String,
}

impl PrintDialogState {
    /// Return `true` while the dialog is shown.
    pub fn is_open(&self) -> bool {
        self.open
    }

    /// Show the dialog.
    pub fn show(&mut self) {
        self.open = true;
    }

    /// Hide the dialog.
    pub fn hide(&mut self) {
        self.open = false;
    }

    /// Borrow the editable print model.
    pub fn model(&self) -> &PrintModel {
        &self.model
    }

    /// Mutably borrow the editable print model.
    pub fn model_mut(&mut self) -> &mut PrintModel {
        &mut self.model
    }

    /// Borrow the most recently submitted plan, if submission succeeded.
    pub fn last_plan(&self) -> Option<&PrintPlan> {
        self.last_plan.as_ref()
    }

    /// Borrow the most recent submission error, if submission failed.
    pub fn last_error(&self) -> Option<&PrintModelError> {
        self.last_error.as_ref()
    }

    /// The most recent status line (plan summary or error message).
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Resolve the current model against `context` and record the outcome.
    ///
    /// This is a pure data operation: no printer is contacted and no I/O occurs.
    ///
    /// # Errors
    ///
    /// Propagates every [`PrintModelError`] from [`PrintModel::build_plan`].
    pub fn submit(&mut self, context: &PrintContext) -> Result<PrintPlan, PrintModelError> {
        match self.model.build_plan(context) {
            Ok(plan) => {
                self.status = plan.summary.clone();
                self.last_plan = Some(plan.clone());
                self.last_error = None;
                Ok(plan)
            }
            Err(error) => {
                self.status = error.to_string();
                self.last_error = Some(error.clone());
                self.last_plan = None;
                Err(error)
            }
        }
    }
}
// ---------------------------------------------------------------------------
// Deterministic imposition and print-production models (T-0150).
//
// These are pure, window-free planners. Each builder validates its inputs and
// produces a fully resolved, deterministic plan (page order, destination cells,
// transforms) that a host print backend can execute. Nothing here opens a
// printer, paints pixels, or performs I/O.
// ---------------------------------------------------------------------------

/// Maximum number of logical pages any imposition planner accepts.
pub const MAX_IMPOSE_PAGES: usize = 100_000;
/// Maximum number of cells (sheets x sides x grid) any planner will allocate.
pub const MAX_IMPOSE_CELLS: usize = 65_536;
/// Maximum number of grid columns or rows accepted by an N-up layout.
pub const MAX_GRID_EXTENT: usize = 64;
/// Maximum number of separations accepted by a separations plan.
pub const MAX_SEPARATIONS: usize = 64;
/// Maximum length of a separation or output-intent name.
pub const MAX_PRINT_NAME_LEN: usize = 128;
/// Maximum bleed on any single edge, in points (1 inch).
pub const MAX_BLEED_PT: f32 = 72.0;

/// A finite, positive rectangle size expressed in PDF points.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct SizePt {
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

impl SizePt {
    /// Validate and build a size from a width and height in points.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] when either dimension is
    /// non-finite, zero, or negative.
    pub fn new(width: f32, height: f32) -> Result<Self, ImposeError> {
        if !Self::is_finite_positive(width) || !Self::is_finite_positive(height) {
            return Err(ImposeError::InvalidGeometry(
                "width and height must be finite and positive".to_owned(),
            ));
        }
        Ok(Self { width, height })
    }

    /// Return `true` when both dimensions are finite and positive.
    pub fn is_valid(&self) -> bool {
        Self::is_finite_positive(self.width) && Self::is_finite_positive(self.height)
    }

    fn is_finite_positive(value: f32) -> bool {
        value.is_finite() && value > 0.0
    }

    /// Build a sheet size from a renderer paper and an orientation.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] for an invalid paper size.
    pub fn from_paper(paper: &Paper, orientation: Orientation) -> Result<Self, ImposeError> {
        let (width, height) = match orientation {
            Orientation::Portrait => (paper.width_pt, paper.height_pt),
            Orientation::Landscape => (paper.height_pt, paper.width_pt),
        };
        Self::new(width, height)
    }
}

/// Uniform scale and translation used to place one page inside a cell.
///
/// The transform is expressed in PDF points: a source page is first scaled by
/// [`Self::scale`], rotated by [`Self::rotation_degrees`], then translated by
/// `(translate_x_pt, translate_y_pt)` from the destination cell's origin.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct PageTransform {
    /// Uniform, positive scale factor.
    pub scale: f32,
    /// Rotation in degrees; one of 0, 90, 180, or 270.
    pub rotation_degrees: i16,
    /// Horizontal translation in points.
    pub translate_x_pt: f32,
    /// Vertical translation in points.
    pub translate_y_pt: f32,
}

impl PageTransform {
    /// The identity transform: scale 1, no rotation, no translation.
    pub fn identity() -> Self {
        Self {
            scale: 1.0,
            rotation_degrees: 0,
            translate_x_pt: 0.0,
            translate_y_pt: 0.0,
        }
    }

    /// Validate that a transform is finite, positively scaled, and a quarter turn.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] or
    /// [`ImposeError::UnsupportedOption`] for a malformed transform.
    pub fn validate(&self) -> Result<(), ImposeError> {
        if !self.scale.is_finite() || self.scale <= 0.0 {
            return Err(ImposeError::InvalidScale(self.scale));
        }
        if !self.translate_x_pt.is_finite() || !self.translate_y_pt.is_finite() {
            return Err(ImposeError::InvalidGeometry(
                "transform translations must be finite".to_owned(),
            ));
        }
        if !matches!(self.rotation_degrees, 0 | 90 | 180 | 270) {
            return Err(ImposeError::UnsupportedOption(format!(
                "unsupported rotation {} degrees",
                self.rotation_degrees
            )));
        }
        Ok(())
    }
}

/// Shared geometry for the deterministic imposition planners.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct ImposeOptions {
    /// Size of one source document page in points.
    pub source_page: SizePt,
    /// Size of one output sheet in points.
    pub sheet: SizePt,
    /// Orientation of the output sheet.
    pub orientation: Orientation,
    /// Uniform margin around the usable sheet area, in points.
    pub margin_pt: f32,
}

impl Default for ImposeOptions {
    fn default() -> Self {
        let letter = SizePt {
            width: 612.0,
            height: 792.0,
        };
        Self {
            source_page: letter,
            sheet: letter,
            orientation: Orientation::Portrait,
            margin_pt: 18.0,
        }
    }
}

impl ImposeOptions {
    /// Validate the shared geometry.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] for non-finite or non-positive
    /// sizes, a negative margin, or a margin that consumes the whole sheet.
    pub fn validate(&self) -> Result<(), ImposeError> {
        if !self.source_page.is_valid() || !self.sheet.is_valid() {
            return Err(ImposeError::InvalidGeometry(
                "source page and sheet must be finite and positive".to_owned(),
            ));
        }
        if !self.margin_pt.is_finite() || self.margin_pt < 0.0 {
            return Err(ImposeError::InvalidGeometry(
                "margin must be finite and non-negative".to_owned(),
            ));
        }
        let sheet = self.effective_sheet();
        if 2.0 * self.margin_pt >= sheet.width.min(sheet.height) {
            return Err(ImposeError::InvalidGeometry(
                "margin consumes the usable sheet area".to_owned(),
            ));
        }
        Ok(())
    }

    /// Sheet size after applying the orientation swap.
    pub fn effective_sheet(&self) -> SizePt {
        match self.orientation {
            Orientation::Portrait => self.sheet,
            Orientation::Landscape => SizePt {
                width: self.sheet.height,
                height: self.sheet.width,
            },
        }
    }
}

/// Typed errors produced by imposition and print-production planners.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum ImposeError {
    /// The requested plan had no pages.
    #[error("no pages to impose")]
    EmptyPageSet,
    /// The page count exceeds the supported maximum.
    #[error("page count {0} exceeds the supported maximum of {1}")]
    TooManyPages(usize, usize),
    /// A size, margin, or placement was non-finite or out of bounds.
    #[error("invalid geometry: {0}")]
    InvalidGeometry(String),
    /// A scale factor was non-finite or non-positive.
    #[error("invalid scale {0}: must be finite and positive")]
    InvalidScale(f32),
    /// A caller requested an option this planner does not support.
    #[error("unsupported option: {0}")]
    UnsupportedOption(String),
    /// An N-up grid had a zero or oversized extent.
    #[error("invalid grid extent {0}")]
    InvalidGrid(usize),
    /// A poster overlap was invalid for the chosen sheet.
    #[error("invalid overlap {0}: must be finite, non-negative, and smaller than the sheet")]
    InvalidOverlap(f32),
    /// A cell or tile budget was exceeded.
    #[error("plan exceeds the {0} cell limit")]
    TooManyCells(usize),
    /// A separation input had an invalid or duplicate name.
    #[error("invalid separation: {0}")]
    InvalidSeparation(String),
    /// An output intent was malformed or ambiguous.
    #[error("invalid output intent: {0}")]
    InvalidOutputIntent(String),
}

/// Which physical side of a sheet a cell belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SheetSide {
    /// The first printed side of the sheet.
    Front,
    /// The second printed side of the sheet.
    Back,
}

impl SheetSide {
    /// Stable human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Front => "front",
            Self::Back => "back",
        }
    }
}

/// A deterministic destination address for one placed page.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DestinationCell {
    /// Zero-based physical sheet index.
    pub sheet: usize,
    /// Front or back side of that sheet.
    pub side: SheetSide,
    /// Zero-based row within the side's grid.
    pub row: usize,
    /// Zero-based column within the side's grid.
    pub column: usize,
}

impl DestinationCell {
    fn key(&self) -> (usize, u8, usize, usize) {
        let side = match self.side {
            SheetSide::Front => 0,
            SheetSide::Back => 1,
        };
        (self.sheet, side, self.row, self.column)
    }
}

/// One source page (or blank padding cell) placed in a destination cell.
#[derive(Clone, Debug, PartialEq)]
pub struct ImposedPage {
    /// Zero-based source page, or `None` for a blank padding cell.
    pub source_page: Option<usize>,
    /// Destination cell on the physical sheet.
    pub cell: DestinationCell,
    /// Transform that places the source page into the cell.
    pub transform: PageTransform,
}

/// A fully resolved, deterministic imposition plan for a host backend.
#[derive(Clone, Debug, PartialEq)]
pub struct ImpositionPlan {
    /// Placed pages sorted by `(sheet, side, row, column)`.
    pub pages: Vec<ImposedPage>,
    /// Number of physical sheets required.
    pub sheet_count: usize,
    /// Number of blank padding cells in the plan.
    pub blank_cells: usize,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl ImpositionPlan {
    /// Source pages in emission order; `None` marks a blank padding cell.
    pub fn page_order(&self) -> Vec<Option<usize>> {
        self.pages.iter().map(|page| page.source_page).collect()
    }

    /// Number of physical sheets required.
    pub fn sheet_count(&self) -> usize {
        self.sheet_count
    }

    /// Number of blank padding cells.
    pub fn blank_cells(&self) -> usize {
        self.blank_cells
    }

    /// Return `true` when the plan contains no cells.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }
}

fn page_if_present(index: usize, count: usize) -> Option<usize> {
    (index < count).then_some(index)
}

fn padded_count(count: usize, multiple: usize) -> Result<usize, ImposeError> {
    let remainder = count % multiple;
    if remainder == 0 {
        return Ok(count);
    }
    count
        .checked_add(multiple - remainder)
        .ok_or(ImposeError::TooManyPages(count, MAX_IMPOSE_PAGES))
}

fn cell_transform(
    options: &ImposeOptions,
    columns: usize,
    rows: usize,
    column: usize,
    row: usize,
) -> Result<PageTransform, ImposeError> {
    let sheet = options.effective_sheet();
    let usable_width = sheet.width - 2.0 * options.margin_pt;
    let usable_height = sheet.height - 2.0 * options.margin_pt;
    let cell_width = usable_width / columns as f32;
    let cell_height = usable_height / rows as f32;
    let scale =
        (cell_width / options.source_page.width).min(cell_height / options.source_page.height);
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ImposeError::InvalidScale(scale));
    }
    let transform = PageTransform {
        scale,
        rotation_degrees: 0,
        translate_x_pt: options.margin_pt + column as f32 * cell_width,
        translate_y_pt: options.margin_pt + row as f32 * cell_height,
    };
    transform.validate()?;
    Ok(transform)
}

fn placed(
    source_page: Option<usize>,
    sheet: usize,
    side: SheetSide,
    row: usize,
    column: usize,
    transform: PageTransform,
) -> ImposedPage {
    ImposedPage {
        source_page,
        cell: DestinationCell {
            sheet,
            side,
            row,
            column,
        },
        transform,
    }
}

fn finish_plan(mut pages: Vec<ImposedPage>, sheet_count: usize, label: &str) -> ImpositionPlan {
    pages.sort_by_key(|page| page.cell.key());
    let blank_cells = pages
        .iter()
        .filter(|page| page.source_page.is_none())
        .count();
    let summary = format!(
        "{label}: {sheet_count} sheet(s), {} cell(s), {blank_cells} blank(s)",
        pages.len()
    );
    ImpositionPlan {
        pages,
        sheet_count,
        blank_cells,
        summary,
    }
}

/// Build a saddle-stitch booklet imposition (two pages per sheet side).
///
/// For `n` source pages the count is padded to the next multiple of four with
/// blank cells. Sheet one's front holds `[last, first]`; its back holds
/// `[second, penultimate]`, and each following sheet steps inward. This is the
/// canonical order a bindery expects after folding and stapling.
///
/// # Errors
///
/// Returns a typed [`ImposeError`] for an empty or oversized page count or for
/// invalid shared geometry.
pub fn booklet_imposition(
    page_count: usize,
    options: &ImposeOptions,
) -> Result<ImpositionPlan, ImposeError> {
    if page_count == 0 {
        return Err(ImposeError::EmptyPageSet);
    }
    if page_count > MAX_IMPOSE_PAGES {
        return Err(ImposeError::TooManyPages(page_count, MAX_IMPOSE_PAGES));
    }
    options.validate()?;

    let padded = padded_count(page_count, 4)?;
    let sheet_count = padded / 4;
    if padded > MAX_IMPOSE_CELLS {
        return Err(ImposeError::TooManyCells(MAX_IMPOSE_CELLS));
    }
    let left_transform = cell_transform(options, 2, 1, 0, 0)?;
    let right_transform = cell_transform(options, 2, 1, 1, 0)?;

    let mut pages = Vec::with_capacity(padded);
    for sheet in 0..sheet_count {
        let front_left = padded - 1 - 2 * sheet;
        let front_right = 2 * sheet;
        let back_left = 2 * sheet + 1;
        let back_right = padded - 2 - 2 * sheet;
        pages.push(placed(
            page_if_present(front_left, page_count),
            sheet,
            SheetSide::Front,
            0,
            0,
            left_transform,
        ));
        pages.push(placed(
            page_if_present(front_right, page_count),
            sheet,
            SheetSide::Front,
            0,
            1,
            right_transform,
        ));
        pages.push(placed(
            page_if_present(back_left, page_count),
            sheet,
            SheetSide::Back,
            0,
            0,
            left_transform,
        ));
        pages.push(placed(
            page_if_present(back_right, page_count),
            sheet,
            SheetSide::Back,
            0,
            1,
            right_transform,
        ));
    }
    Ok(finish_plan(pages, sheet_count, "booklet"))
}

/// The fill order used when assigning logical pages to an N-up grid.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum NUpOrder {
    /// Fill each row left-to-right, then move to the next row.
    RowMajor,
    /// Fill each column top-to-bottom, then move to the next column.
    ColumnMajor,
}

impl NUpOrder {
    /// Stable human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::RowMajor => "row-major",
            Self::ColumnMajor => "column-major",
        }
    }
}

/// A bounded N-up grid declared as columns by rows.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct NUpLayout {
    /// Number of columns per sheet side.
    pub columns: usize,
    /// Number of rows per sheet side.
    pub rows: usize,
}

impl NUpLayout {
    /// Validate and build a grid.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGrid`] for a zero extent or an extent
    /// larger than [`MAX_GRID_EXTENT`].
    pub fn new(columns: usize, rows: usize) -> Result<Self, ImposeError> {
        if columns == 0 || columns > MAX_GRID_EXTENT || rows == 0 || rows > MAX_GRID_EXTENT {
            return Err(ImposeError::InvalidGrid(columns.max(rows)));
        }
        Ok(Self { columns, rows })
    }

    /// Number of pages placed on one sheet side.
    pub fn pages_per_sheet(&self) -> usize {
        self.columns * self.rows
    }

    /// Stable human-readable label such as `"2x2"`.
    pub fn label(&self) -> String {
        format!("{}x{}", self.columns, self.rows)
    }
}

/// Build an N-up imposition with deterministic page-to-cell assignment.
///
/// Trailing cells on the final sheet are blank. Within a sheet, `RowMajor`
/// increments the column fastest while `ColumnMajor` increments the row
/// fastest, so both produce stable but different page orders.
///
/// # Errors
///
/// Returns a typed [`ImposeError`] for an empty or oversized page count, an
/// invalid grid, invalid shared geometry, or an excessive cell count.
pub fn n_up_imposition(
    page_count: usize,
    layout: NUpLayout,
    order: NUpOrder,
    options: &ImposeOptions,
) -> Result<ImpositionPlan, ImposeError> {
    if page_count == 0 {
        return Err(ImposeError::EmptyPageSet);
    }
    if page_count > MAX_IMPOSE_PAGES {
        return Err(ImposeError::TooManyPages(page_count, MAX_IMPOSE_PAGES));
    }
    options.validate()?;

    let per_sheet = layout.pages_per_sheet();
    if per_sheet == 0 || per_sheet > MAX_IMPOSE_CELLS {
        return Err(ImposeError::InvalidGrid(per_sheet));
    }
    let padded = padded_count(page_count, per_sheet)?;
    if padded > MAX_IMPOSE_CELLS {
        return Err(ImposeError::TooManyCells(MAX_IMPOSE_CELLS));
    }
    let sheet_count = padded / per_sheet;

    let mut pages = Vec::with_capacity(padded);
    for sheet in 0..sheet_count {
        for slot in 0..per_sheet {
            let (column, row) = match order {
                NUpOrder::RowMajor => (slot % layout.columns, slot / layout.columns),
                NUpOrder::ColumnMajor => (slot / layout.rows, slot % layout.rows),
            };
            let index = sheet * per_sheet + slot;
            pages.push(placed(
                page_if_present(index, page_count),
                sheet,
                SheetSide::Front,
                row,
                column,
                cell_transform(options, layout.columns, layout.rows, column, row)?,
            ));
        }
    }
    let label = format!("{}-up({},{})", layout.label(), per_sheet, order.label());
    Ok(finish_plan(pages, sheet_count, &label))
}

/// One poster tile: a window over a scaled source page.
#[derive(Clone, Debug, PartialEq)]
pub struct PosterTile {
    /// Zero-based column in the tile grid.
    pub column: usize,
    /// Zero-based row in the tile grid.
    pub row: usize,
    /// Window origin in scaled page coordinates.
    pub offset_x_pt: f32,
    /// Window origin in scaled page coordinates.
    pub offset_y_pt: f32,
    /// Left edge of the source-page rectangle captured by this tile.
    pub source_x0: f32,
    /// Top edge of the source-page rectangle captured by this tile.
    pub source_y0: f32,
    /// Right edge of the source-page rectangle captured by this tile.
    pub source_x1: f32,
    /// Bottom edge of the source-page rectangle captured by this tile.
    pub source_y1: f32,
    /// Transform that places the scaled page so this tile's window is visible.
    pub transform: PageTransform,
}

/// A deterministic poster/tile printing plan.
#[derive(Clone, Debug, PartialEq)]
pub struct PosterPlan {
    /// Uniform scale derived from the requested percentage.
    pub scale: f32,
    /// Size of the source page after scaling.
    pub scaled_size: SizePt,
    /// Number of tile columns.
    pub columns: usize,
    /// Number of tile rows.
    pub rows: usize,
    /// Tiles in row-major order.
    pub tiles: Vec<PosterTile>,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl PosterPlan {
    /// Total number of sheets (tiles) this poster needs.
    pub fn tile_count(&self) -> usize {
        self.tiles.len()
    }

    /// Return `true` when the plan contains no tiles.
    pub fn is_empty(&self) -> bool {
        self.tiles.is_empty()
    }
}

fn tile_axis(total: f32, tile: f32, overlap: f32, stride: f32) -> usize {
    if total <= tile {
        return 1;
    }
    let required = ((total - overlap) / stride).ceil();
    if !required.is_finite() {
        return MAX_IMPOSE_CELLS + 1;
    }
    (required as usize).max(1)
}

/// Build a deterministic poster/tile plan for one oversized page.
///
/// Adjacent tiles overlap by `overlap_pt`, so the distance between tile origins
/// is `sheet - overlap`. Each tile records both its window offset and the exact
/// slice of the source page it captures, along with the translation that a host
/// backend applies before clipping to the sheet.
///
/// # Errors
///
/// Returns a typed [`ImposeError`] for invalid source/sheet geometry, a
/// non-finite or non-positive scale, an invalid overlap, or an excessive tile
/// count.
pub fn poster_tiles(
    source: SizePt,
    sheet: SizePt,
    scale_percent: f32,
    overlap_pt: f32,
) -> Result<PosterPlan, ImposeError> {
    if !source.is_valid() || !sheet.is_valid() {
        return Err(ImposeError::InvalidGeometry(
            "source and sheet must be finite and positive".to_owned(),
        ));
    }
    if !scale_percent.is_finite() || scale_percent <= 0.0 {
        return Err(ImposeError::InvalidScale(scale_percent));
    }
    if !overlap_pt.is_finite() || overlap_pt < 0.0 {
        return Err(ImposeError::InvalidOverlap(overlap_pt));
    }
    let scale = scale_percent / 100.0;
    let scaled = SizePt::new(source.width * scale, source.height * scale)?;
    let stride_x = sheet.width - overlap_pt;
    let stride_y = sheet.height - overlap_pt;
    if !stride_x.is_finite() || !stride_y.is_finite() || stride_x <= 0.0 || stride_y <= 0.0 {
        return Err(ImposeError::InvalidOverlap(overlap_pt));
    }

    let columns = tile_axis(scaled.width, sheet.width, overlap_pt, stride_x);
    let rows = tile_axis(scaled.height, sheet.height, overlap_pt, stride_y);
    let Some(tile_count) = columns.checked_mul(rows) else {
        return Err(ImposeError::TooManyCells(MAX_IMPOSE_CELLS));
    };
    if tile_count > MAX_IMPOSE_CELLS {
        return Err(ImposeError::TooManyCells(MAX_IMPOSE_CELLS));
    }

    let mut tiles = Vec::with_capacity(tile_count);
    for row in 0..rows {
        for column in 0..columns {
            let offset_x = column as f32 * stride_x;
            let offset_y = row as f32 * stride_y;
            let source_x0 = offset_x / scale;
            let source_y0 = offset_y / scale;
            let source_x1 = ((offset_x + sheet.width) / scale).min(source.width);
            let source_y1 = ((offset_y + sheet.height) / scale).min(source.height);
            tiles.push(PosterTile {
                column,
                row,
                offset_x_pt: offset_x,
                offset_y_pt: offset_y,
                source_x0,
                source_y0,
                source_x1,
                source_y1,
                transform: PageTransform {
                    scale,
                    rotation_degrees: 0,
                    translate_x_pt: -offset_x,
                    translate_y_pt: -offset_y,
                },
            });
        }
    }

    Ok(PosterPlan {
        scale,
        scaled_size: scaled,
        columns,
        rows,
        tiles,
        summary: format!(
            "poster: {columns}x{rows} tiles, {tile_count} sheet(s), {scale_percent}% scale, {overlap_pt} pt overlap"
        ),
    })
}

/// A page-fit mode with wider coverage than the renderer's scale enum.
#[derive(Clone, Copy, Debug, PartialEq)]
pub enum FitMode {
    /// Render at 100% (scale 1.0).
    Actual,
    /// Fit the whole page inside the target, preserving aspect ratio.
    Fit,
    /// Match the target width, preserving aspect ratio.
    FitWidth,
    /// Match the target height, preserving aspect ratio.
    FitHeight,
    /// Multiply actual size by a caller-supplied percentage.
    Custom(f32),
}

impl FitMode {
    /// Convert a renderer [`PageScaleMode`] into a [`FitMode`].
    pub fn from_render(mode: PageScaleMode) -> Self {
        match mode {
            PageScaleMode::Actual => Self::Actual,
            PageScaleMode::Fit => Self::Fit,
            PageScaleMode::CustomPercent(percent) => Self::Custom(percent),
        }
    }

    /// Stable human-readable label.
    pub fn label(&self) -> String {
        match self {
            Self::Actual => "actual".to_owned(),
            Self::Fit => "fit".to_owned(),
            Self::FitWidth => "fit-width".to_owned(),
            Self::FitHeight => "fit-height".to_owned(),
            Self::Custom(percent) => format!("{percent}%"),
        }
    }
}

/// The resolved geometry of one page placed on a target sheet.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FitTransform {
    /// Uniform scale factor.
    pub scale: f32,
    /// Width of the scaled page.
    pub rendered_width: f32,
    /// Height of the scaled page.
    pub rendered_height: f32,
    /// Horizontal offset that centres the scaled page on the target.
    pub offset_x_pt: f32,
    /// Vertical offset that centres the scaled page on the target.
    pub offset_y_pt: f32,
}

impl FitTransform {
    /// Return `true` when the scaled page fits entirely inside the target.
    pub fn fits(&self, target: SizePt) -> bool {
        self.rendered_width <= target.width && self.rendered_height <= target.height
    }
}

/// Resolve a fit mode into a deterministic, centred [`FitTransform`].
///
/// # Errors
///
/// Returns a typed [`ImposeError`] for invalid source/target geometry, a
/// non-finite or non-positive custom percentage, or a non-finite result.
pub fn resolve_fit(
    source: SizePt,
    target: SizePt,
    mode: FitMode,
) -> Result<FitTransform, ImposeError> {
    if !source.is_valid() || !target.is_valid() {
        return Err(ImposeError::InvalidGeometry(
            "source and target must be finite and positive".to_owned(),
        ));
    }
    let scale = match mode {
        FitMode::Actual => 1.0,
        FitMode::Fit => (target.width / source.width).min(target.height / source.height),
        FitMode::FitWidth => target.width / source.width,
        FitMode::FitHeight => target.height / source.height,
        FitMode::Custom(percent) => {
            if !percent.is_finite() || percent <= 0.0 {
                return Err(ImposeError::InvalidScale(percent));
            }
            percent / 100.0
        }
    };
    if !scale.is_finite() || scale <= 0.0 {
        return Err(ImposeError::InvalidScale(scale));
    }
    let rendered_width = source.width * scale;
    let rendered_height = source.height * scale;
    if !rendered_width.is_finite() || !rendered_height.is_finite() {
        return Err(ImposeError::InvalidScale(scale));
    }
    Ok(FitTransform {
        scale,
        rendered_width,
        rendered_height,
        offset_x_pt: (target.width - rendered_width) * 0.5,
        offset_y_pt: (target.height - rendered_height) * 0.5,
    })
}

/// Physical paper source (tray) selected for a print job.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PaperSource {
    /// Let the host driver choose.
    Auto,
    /// Numbered paper tray (1..=255).
    Tray(u8),
    /// Manual feed slot.
    ManualFeed,
    /// Envelope feeder.
    Envelope,
}

impl PaperSource {
    /// Validate a paper source.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::UnsupportedOption`] for tray number zero.
    pub fn validate(&self) -> Result<(), ImposeError> {
        match self {
            Self::Tray(0) => Err(ImposeError::UnsupportedOption(
                "tray number must be 1..=255".to_owned(),
            )),
            _ => Ok(()),
        }
    }

    /// Stable human-readable label.
    pub fn label(&self) -> String {
        match self {
            Self::Auto => "auto".to_owned(),
            Self::Tray(number) => format!("tray-{number}"),
            Self::ManualFeed => "manual-feed".to_owned(),
            Self::Envelope => "envelope".to_owned(),
        }
    }
}

/// One physical sheet in a duplex plan.
#[derive(Clone, Debug, PartialEq)]
pub struct SheetEntry {
    /// Zero-based sheet index.
    pub sheet: usize,
    /// Logical page printed on the front side.
    pub front_page: usize,
    /// Logical page printed on the back side, when duplex is enabled.
    pub back_page: Option<usize>,
    /// Binding edge described by the duplex mode.
    pub binding_edge: &'static str,
}

/// A deterministic duplex/paper-source plan for a host collator.
#[derive(Clone, Debug, PartialEq)]
pub struct DuplexPlan {
    /// Duplex mode recorded in the plan.
    pub mode: Duplex,
    /// Paper source recorded in the plan.
    pub paper_source: PaperSource,
    /// One entry per physical sheet, in ascending sheet order.
    pub sheets: Vec<SheetEntry>,
    /// Number of physical sheets required.
    pub sheet_count: usize,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl DuplexPlan {
    /// Number of printed sides (front plus optional back) in this plan.
    pub fn sides_used(&self) -> usize {
        self.sheets.len()
            + self
                .sheets
                .iter()
                .filter(|entry| entry.back_page.is_some())
                .count()
    }

    /// Build a duplex plan for `logical_pages` already-expanded pages.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ImposeError`] for an empty or oversized page count or
    /// an invalid paper source.
    pub fn build(
        logical_pages: usize,
        mode: Duplex,
        paper_source: PaperSource,
    ) -> Result<Self, ImposeError> {
        if logical_pages == 0 {
            return Err(ImposeError::EmptyPageSet);
        }
        if logical_pages > MAX_IMPOSE_PAGES {
            return Err(ImposeError::TooManyPages(logical_pages, MAX_IMPOSE_PAGES));
        }
        paper_source.validate()?;

        let binding_edge = match mode {
            Duplex::None => "none",
            Duplex::LongEdge => "long-edge",
            Duplex::ShortEdge => "short-edge",
        };
        let per_sheet = if matches!(mode, Duplex::None) { 1 } else { 2 };
        let sheet_count = logical_pages.div_ceil(per_sheet);
        let mut sheets = Vec::with_capacity(sheet_count);
        for sheet in 0..sheet_count {
            let front = sheet * per_sheet;
            let back = if per_sheet == 2 {
                page_if_present(front + 1, logical_pages)
            } else {
                None
            };
            sheets.push(SheetEntry {
                sheet,
                front_page: front,
                back_page: back,
                binding_edge,
            });
        }

        Ok(Self {
            mode,
            paper_source,
            sheet_count,
            summary: format!(
                "duplex: {sheet_count} sheet(s), {binding_edge} binding, {} paper",
                paper_source.label()
            ),
            sheets,
        })
    }
}

/// The printer marks a host backend should paint around the trimmed page.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PrinterMarks {
    /// Eight crop marks, two at each trim corner.
    pub crop_marks: bool,
    /// Four registration marks at the trim edge midpoints.
    pub registration_marks: bool,
    /// A colour control strip below the trim box.
    pub color_bars: bool,
    /// A page-information label strip above the trim box.
    pub page_info: bool,
}

/// Category of a printer mark.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MarkKind {
    /// A crop mark line.
    Crop,
    /// A registration cross.
    Registration,
    /// A colour control strip.
    ColorBar,
    /// A page-information label strip.
    PageInfo,
}

impl MarkKind {
    /// Stable human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::Crop => "crop",
            Self::Registration => "registration",
            Self::ColorBar => "color-bar",
            Self::PageInfo => "page-info",
        }
    }
}

/// An axis-aligned mark rectangle in sheet coordinates (points).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct MarkRect {
    /// Left edge in points.
    pub x: f32,
    /// Bottom edge in points.
    pub y: f32,
    /// Width in points.
    pub width: f32,
    /// Height in points.
    pub height: f32,
}

/// A single deterministic printer mark.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mark {
    /// The kind of mark.
    pub kind: MarkKind,
    /// Placement rectangle in sheet coordinates.
    pub rect: MarkRect,
}

/// A deterministic printer-mark placement plan.
#[derive(Clone, Debug, PartialEq)]
pub struct MarksPlan {
    /// Marks in fixed `(crop, registration, color-bar, page-info)` order.
    pub marks: Vec<Mark>,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

const MARK_THICKNESS: f32 = 0.5;
const MARK_LENGTH: f32 = 12.0;
const MARK_GAP: f32 = 3.0;

impl MarksPlan {
    /// Build a marks plan centred around a trim box on a larger sheet.
    ///
    /// Every generated rectangle is checked to lie within the sheet; a sheet
    /// equal to the trim box therefore fails because marks need bleed room.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] for invalid trim/sheet geometry,
    /// a sheet smaller than the trim box, or marks that fall outside the sheet.
    pub fn build(trim: SizePt, sheet: SizePt, options: PrinterMarks) -> Result<Self, ImposeError> {
        if !trim.is_valid() || !sheet.is_valid() {
            return Err(ImposeError::InvalidGeometry(
                "trim and sheet must be finite and positive".to_owned(),
            ));
        }
        if trim.width > sheet.width || trim.height > sheet.height {
            return Err(ImposeError::InvalidGeometry(
                "sheet must be at least as large as the trim box".to_owned(),
            ));
        }

        let origin_x = (sheet.width - trim.width) * 0.5;
        let origin_y = (sheet.height - trim.height) * 0.5;
        let right = origin_x + trim.width;
        let top = origin_y + trim.height;
        let mut marks = Vec::new();

        if options.crop_marks {
            let corners = [
                (origin_x, origin_y, true, true),
                (right, origin_y, false, true),
                (origin_x, top, true, false),
                (right, top, false, false),
            ];
            for (x, y, is_left, is_bottom) in corners {
                let horizontal_x = if is_left {
                    x - MARK_GAP - MARK_LENGTH
                } else {
                    x + MARK_GAP
                };
                let vertical_y = if is_bottom {
                    y - MARK_GAP - MARK_LENGTH
                } else {
                    y + MARK_GAP
                };
                marks.push(Mark {
                    kind: MarkKind::Crop,
                    rect: MarkRect {
                        x: horizontal_x,
                        y: y - MARK_THICKNESS * 0.5,
                        width: MARK_LENGTH,
                        height: MARK_THICKNESS,
                    },
                });
                marks.push(Mark {
                    kind: MarkKind::Crop,
                    rect: MarkRect {
                        x: x - MARK_THICKNESS * 0.5,
                        y: vertical_y,
                        width: MARK_THICKNESS,
                        height: MARK_LENGTH,
                    },
                });
            }
        }

        if options.registration_marks {
            let size = 4.0;
            let center_x = origin_x + trim.width * 0.5;
            let center_y = origin_y + trim.height * 0.5;
            let positions = [
                (center_x, origin_y - MARK_GAP - MARK_LENGTH - size),
                (center_x, top + MARK_GAP + MARK_LENGTH),
                (origin_x - MARK_GAP - MARK_LENGTH - size, center_y),
                (right + MARK_GAP + MARK_LENGTH, center_y),
            ];
            for (x, y) in positions {
                marks.push(Mark {
                    kind: MarkKind::Registration,
                    rect: MarkRect {
                        x,
                        y,
                        width: size,
                        height: size,
                    },
                });
            }
        }

        if options.color_bars {
            marks.push(Mark {
                kind: MarkKind::ColorBar,
                rect: MarkRect {
                    x: origin_x,
                    y: origin_y - 24.0,
                    width: trim.width,
                    height: 6.0,
                },
            });
        }

        if options.page_info {
            marks.push(Mark {
                kind: MarkKind::PageInfo,
                rect: MarkRect {
                    x: origin_x,
                    y: top + 6.0,
                    width: trim.width * 0.5,
                    height: 8.0,
                },
            });
        }

        for mark in &marks {
            let rect = mark.rect;
            let finite = rect.x.is_finite()
                && rect.y.is_finite()
                && rect.width.is_finite()
                && rect.height.is_finite();
            if !finite
                || rect.x < 0.0
                || rect.y < 0.0
                || rect.x + rect.width > sheet.width
                || rect.y + rect.height > sheet.height
            {
                return Err(ImposeError::InvalidGeometry(format!(
                    "{} mark does not fit on the sheet",
                    mark.kind.label()
                )));
            }
        }

        let summary = format!(
            "marks: {} rect(s), crop={}, registration={}, color-bars={}, page-info={}",
            marks.len(),
            options.crop_marks,
            options.registration_marks,
            options.color_bars,
            options.page_info
        );
        Ok(Self { marks, summary })
    }
}

/// Per-edge bleed in points, applied outwards from the trim box.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Bleed {
    /// Bleed beyond the top edge.
    pub top: f32,
    /// Bleed beyond the right edge.
    pub right: f32,
    /// Bleed beyond the bottom edge.
    pub bottom: f32,
    /// Bleed beyond the left edge.
    pub left: f32,
}

/// Resolved bleed rectangle enclosing the trim box.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct BleedBounds {
    /// Left edge in points (negative relative to the trim box).
    pub x: f32,
    /// Bottom edge in points (negative relative to the trim box).
    pub y: f32,
    /// Width including left and right bleed.
    pub width: f32,
    /// Height including top and bottom bleed.
    pub height: f32,
}

impl BleedBounds {
    /// Validate bleed and expand the trim box.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidGeometry`] for invalid trim geometry, a
    /// non-finite/negative/oversized bleed, or a non-finite expanded box.
    pub fn build(trim: SizePt, bleed: Bleed, max_bleed_pt: f32) -> Result<Self, ImposeError> {
        if !trim.is_valid() {
            return Err(ImposeError::InvalidGeometry(
                "trim must be finite and positive".to_owned(),
            ));
        }
        if !max_bleed_pt.is_finite() || max_bleed_pt <= 0.0 {
            return Err(ImposeError::InvalidGeometry(
                "maximum bleed must be finite and positive".to_owned(),
            ));
        }
        for value in [bleed.top, bleed.right, bleed.bottom, bleed.left] {
            if !value.is_finite() || value < 0.0 || value > max_bleed_pt {
                return Err(ImposeError::InvalidGeometry(format!(
                    "bleed must be within 0..={max_bleed_pt} pt"
                )));
            }
        }
        let bounds = Self {
            x: -bleed.left,
            y: -bleed.bottom,
            width: trim.width + bleed.left + bleed.right,
            height: trim.height + bleed.top + bleed.bottom,
        };
        if !bounds.x.is_finite()
            || !bounds.y.is_finite()
            || !bounds.width.is_finite()
            || !bounds.height.is_finite()
        {
            return Err(ImposeError::InvalidGeometry(
                "expanded bleed box is not finite".to_owned(),
            ));
        }
        Ok(bounds)
    }

    /// Return `true` when this box fully contains the given trim box.
    pub fn contains_trim(&self, trim: SizePt) -> bool {
        self.x <= 0.0
            && self.y <= 0.0
            && self.x + self.width >= trim.width
            && self.y + self.height >= trim.height
    }
}

/// One caller-supplied separation (ink) input.
#[derive(Clone, Debug, PartialEq)]
pub struct SeparationInput {
    /// Ink name, for example `"Cyan"` or `"PANTONE 185 C"`.
    pub name: String,
    /// `true` for a process ink, `false` for a spot ink.
    pub process: bool,
}

/// One resolved separation in a [`SeparationsPlan`].
#[derive(Clone, Debug, PartialEq)]
pub struct Separation {
    /// Stable one-based identifier assigned in canonical order.
    pub id: u32,
    /// Ink name.
    pub name: String,
    /// `true` for a process ink, `false` for a spot ink.
    pub process: bool,
    /// Whether the separation is enabled by default.
    pub enabled: bool,
}

/// A deterministic, canonical separations plan.
#[derive(Clone, Debug, PartialEq)]
pub struct SeparationsPlan {
    /// Separations ordered process-first, then by name.
    pub separations: Vec<Separation>,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl SeparationsPlan {
    /// Build a separations plan whose result is independent of input order.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidSeparation`] for an empty, oversized, or
    /// duplicate name, an empty list, or more than [`MAX_SEPARATIONS`] inks.
    pub fn build(inputs: &[SeparationInput]) -> Result<Self, ImposeError> {
        if inputs.is_empty() || inputs.len() > MAX_SEPARATIONS {
            return Err(ImposeError::InvalidSeparation(format!(
                "separation count must be 1..={MAX_SEPARATIONS}"
            )));
        }
        let mut entries = Vec::with_capacity(inputs.len());
        for input in inputs {
            let name = input.name.trim();
            if name.is_empty() || name.len() > MAX_PRINT_NAME_LEN {
                return Err(ImposeError::InvalidSeparation(
                    "separation name must be non-empty and bounded".to_owned(),
                ));
            }
            entries.push((name.to_owned(), input.process));
        }
        entries.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        for pair in entries.windows(2) {
            if pair[0].0 == pair[1].0 {
                return Err(ImposeError::InvalidSeparation(format!(
                    "duplicate separation {}",
                    pair[0].0
                )));
            }
        }
        let separations = entries
            .into_iter()
            .enumerate()
            .map(|(index, (name, process))| Separation {
                id: (index + 1) as u32,
                name,
                process,
                enabled: true,
            })
            .collect::<Vec<_>>();
        let process_count = separations.iter().filter(|sep| sep.process).count();
        let summary = format!(
            "separations: {} total, {process_count} process, {} spot",
            separations.len(),
            separations.len() - process_count
        );
        Ok(Self {
            separations,
            summary,
        })
    }
}

/// ICC output-intent subtype carried in a print-production plan.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputIntentSubtype {
    /// A PDF/X output condition.
    PdfX,
    /// A PDF/A output condition.
    PdfA,
    /// A generic ICC-based output intent.
    IccBased,
}

impl OutputIntentSubtype {
    /// Stable human-readable label.
    pub fn label(&self) -> &'static str {
        match self {
            Self::PdfX => "PDF/X",
            Self::PdfA => "PDF/A",
            Self::IccBased => "ICCBased",
        }
    }
}

/// One caller-supplied output intent.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputIntent {
    /// Output intent subtype.
    pub subtype: OutputIntentSubtype,
    /// Human-readable output condition name.
    pub name: String,
    /// ICC registry name from the output intent.
    pub registry: String,
    /// ICC rendering intent name.
    pub rendering_intent: String,
}

/// A deterministic output-intent report.
#[derive(Clone, Debug, PartialEq)]
pub struct OutputIntentReport {
    /// The single resolved intent, or `None` when none was declared.
    pub intent: Option<OutputIntent>,
    /// Deterministic, ordered warnings.
    pub warnings: Vec<String>,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl OutputIntentReport {
    /// Validate zero or one output intent and report the resolved condition.
    ///
    /// # Errors
    ///
    /// Returns [`ImposeError::InvalidOutputIntent`] for more than one intent, a
    /// blank or oversized name/registry, or an unknown rendering intent.
    pub fn build(intents: &[OutputIntent]) -> Result<Self, ImposeError> {
        if intents.len() > 1 {
            return Err(ImposeError::InvalidOutputIntent(
                "multiple output intents are not supported".to_owned(),
            ));
        }
        let mut warnings = Vec::new();
        let Some(source) = intents.first() else {
            warnings.push("no output intent declared; host must choose a default".to_owned());
            return Ok(Self {
                intent: None,
                warnings,
                summary: "output-intent: none declared".to_owned(),
            });
        };

        let name = source.name.trim();
        let registry = source.registry.trim();
        if name.is_empty() || name.len() > MAX_PRINT_NAME_LEN {
            return Err(ImposeError::InvalidOutputIntent(
                "output intent name must be non-empty and bounded".to_owned(),
            ));
        }
        if registry.is_empty() || registry.len() > MAX_PRINT_NAME_LEN {
            return Err(ImposeError::InvalidOutputIntent(
                "output intent registry must be non-empty and bounded".to_owned(),
            ));
        }
        let rendering_intent = match source.rendering_intent.to_ascii_lowercase().as_str() {
            "perceptual" => "Perceptual".to_owned(),
            "relativecolorimetric" => "RelativeColorimetric".to_owned(),
            "saturation" => "Saturation".to_owned(),
            "absolutecolorimetric" => "AbsoluteColorimetric".to_owned(),
            _ => {
                return Err(ImposeError::InvalidOutputIntent(format!(
                    "unsupported rendering intent {}",
                    source.rendering_intent
                )));
            }
        };
        let intent = OutputIntent {
            subtype: source.subtype,
            name: name.to_owned(),
            registry: registry.to_owned(),
            rendering_intent,
        };
        if matches!(intent.subtype, OutputIntentSubtype::PdfX) {
            warnings.push("PDF/X output condition requires host ICC validation".to_owned());
        }
        let summary = format!(
            "output-intent: {} {} [{}], {} warning(s)",
            intent.subtype.label(),
            intent.name,
            intent.registry,
            warnings.len()
        );
        Ok(Self {
            intent: Some(intent),
            warnings,
            summary,
        })
    }
}

/// All print-production options gathered for one production plan.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ProductionOptions {
    /// Printer marks to paint around the trim box.
    pub marks: PrinterMarks,
    /// Bleed applied outwards from the trim box.
    pub bleed: Bleed,
    /// Separations (inks) expected by the backend.
    pub separations: Vec<SeparationInput>,
    /// Zero or one output intent.
    pub intents: Vec<OutputIntent>,
}

/// A fully resolved, deterministic print-production plan.
#[derive(Clone, Debug, PartialEq)]
pub struct ProductionPlan {
    /// Resolved printer-mark placements.
    pub marks: MarksPlan,
    /// Resolved bleed bounds.
    pub bleed: BleedBounds,
    /// Resolved separations plan.
    pub separations: SeparationsPlan,
    /// Resolved output-intent report.
    pub output_intent: OutputIntentReport,
    /// Deterministic, human-readable summary.
    pub summary: String,
}

impl ProductionPlan {
    /// Validate and resolve every print-production option together.
    ///
    /// # Errors
    ///
    /// Propagates every [`ImposeError`] from the individual builders.
    pub fn build(
        trim: SizePt,
        sheet: SizePt,
        options: &ProductionOptions,
    ) -> Result<Self, ImposeError> {
        let marks = MarksPlan::build(trim, sheet, options.marks)?;
        let bleed = BleedBounds::build(trim, options.bleed, MAX_BLEED_PT)?;
        let separations = SeparationsPlan::build(&options.separations)?;
        let output_intent = OutputIntentReport::build(&options.intents)?;
        let summary = format!(
            "production: {} | {} | {} | {}",
            marks.summary, "bleed bounds", separations.summary, output_intent.summary
        );
        Ok(Self {
            marks,
            bleed,
            separations,
            output_intent,
            summary,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn context(pages: usize) -> PrintContext {
        PrintContext::new(pages, 0, &[])
    }

    #[test]
    fn range_all_resolves_every_page() {
        let plan = PrintModel::default().build_plan(&context(4)).expect("plan");
        assert_eq!(plan.pages, vec![0, 1, 2, 3]);
        assert_eq!(plan.page_numbers(), vec![1, 2, 3, 4]);
        assert_eq!(plan.raster_count(), 4);
        assert!(!plan.is_empty());
    }

    #[test]
    fn range_current_uses_current_page() {
        let model = PrintModel {
            range: PrintPageRange::Current,
            ..PrintModel::default()
        };
        let ctx = PrintContext::new(5, 3, &[]);
        let plan = model.build_plan(&ctx).expect("plan");
        assert_eq!(plan.pages, vec![3]);
    }

    #[test]
    fn range_selection_uses_selected_pages() {
        let model = PrintModel {
            range: PrintPageRange::Selection,
            ..PrintModel::default()
        };
        let ctx = PrintContext::new(6, 0, &[4, 1, 4]);
        let plan = model.build_plan(&ctx).expect("plan");
        assert_eq!(plan.pages, vec![1, 4]);
    }

    #[test]
    fn range_custom_parses_and_rejects_invalid() {
        let model = PrintModel {
            range: PrintPageRange::Custom("1-3,5".to_owned()),
            ..PrintModel::default()
        };
        let plan = model.build_plan(&context(6)).expect("valid custom plan");
        assert_eq!(plan.pages, vec![0, 1, 2, 4]);

        let bad = PrintModel {
            range: PrintPageRange::Custom("3-1".to_owned()),
            ..PrintModel::default()
        };
        assert!(matches!(
            bad.build_plan(&context(6)),
            Err(PrintModelError::InvalidRange(_))
        ));
        let out_of_range = PrintModel {
            range: PrintPageRange::Custom("9".to_owned()),
            ..PrintModel::default()
        };
        assert!(matches!(
            out_of_range.build_plan(&context(6)),
            Err(PrintModelError::InvalidRange(_))
        ));
    }

    #[test]
    fn copies_duplex_and_scale_are_recorded_in_plan() {
        let model = PrintModel {
            copies: 2,
            duplex: Duplex::LongEdge,
            scale: PageScaleMode::CustomPercent(50.0),
            orientation: Orientation::Landscape,
            paper: PaperSize::A4,
            ..PrintModel::default()
        };
        let plan = model.build_plan(&context(3)).expect("plan");
        assert_eq!(plan.pages, vec![0, 0, 1, 1, 2, 2]);
        assert_eq!(plan.settings.copies, 2);
        assert_eq!(plan.settings.duplex, Duplex::LongEdge);
        assert_eq!(plan.duplex(), Duplex::LongEdge);
        assert_eq!(plan.settings.orientation, Orientation::Landscape);
        assert_eq!(plan.settings.scale, PageScaleMode::CustomPercent(50.0));
        assert_eq!(plan.settings.paper.name, "A4");
        assert!(plan.summary.contains("50%"));
        assert!(plan.summary.contains("LongEdge"));
    }

    #[test]
    fn plan_is_deterministic() {
        let model = PrintModel {
            range: PrintPageRange::Custom("3,1,2".to_owned()),
            copies: 2,
            ..PrintModel::default()
        };
        let a = model.build_plan(&context(4)).expect("plan a");
        let b = model.build_plan(&context(4)).expect("plan b");
        assert_eq!(a, b);
        assert_eq!(a.summary, b.summary);
    }

    #[test]
    fn invalid_copies_and_scale_are_rejected() {
        let bad_copies = PrintModel {
            copies: 0,
            ..PrintModel::default()
        };
        assert_eq!(
            bad_copies.build_plan(&context(2)),
            Err(PrintModelError::InvalidCopies(0))
        );
        let bad_scale = PrintModel {
            scale: PageScaleMode::CustomPercent(-1.0),
            ..PrintModel::default()
        };
        assert!(matches!(
            bad_scale.build_plan(&context(2)),
            Err(PrintModelError::InvalidScale(_))
        ));
    }

    #[test]
    fn current_and_selection_out_of_range_are_typed_errors() {
        let current = PrintModel {
            range: PrintPageRange::Current,
            ..PrintModel::default()
        };
        assert_eq!(
            current.build_plan(&PrintContext::new(3, 9, &[])),
            Err(PrintModelError::CurrentPageOutOfRange(9, 3))
        );
        let selection = PrintModel {
            range: PrintPageRange::Selection,
            ..PrintModel::default()
        };
        assert_eq!(
            selection.build_plan(&PrintContext::new(3, 0, &[7])),
            Err(PrintModelError::SelectionOutOfRange(7, 3))
        );
    }

    #[test]
    fn empty_selection_is_a_typed_error() {
        let selection = PrintModel {
            range: PrintPageRange::Selection,
            ..PrintModel::default()
        };
        assert_eq!(
            selection.build_plan(&PrintContext::new(3, 0, &[])),
            Err(PrintModelError::EmptySelection)
        );
        assert_eq!(
            PrintModel::default().build_plan(&context(0)),
            Err(PrintModelError::EmptySelection)
        );
    }

    #[test]
    fn paper_size_maps_to_render_paper() {
        assert_eq!(PaperSize::Letter.to_render_paper().name, "Letter");
        assert_eq!(PaperSize::A4.to_render_paper().name, "A4");
        assert_eq!(PaperSize::Legal.to_render_paper().name, "Legal");
        let custom = PaperSize::Custom {
            name: "Wide".to_owned(),
            width_pt: 400.0,
            height_pt: 200.0,
        };
        assert!(custom.is_valid());
        assert_eq!(custom.name(), "Wide");
        assert_eq!(custom.to_render_paper().width_pt, 400.0);
        let invalid = PaperSize::Custom {
            name: "bad".to_owned(),
            width_pt: 0.0,
            height_pt: 10.0,
        };
        assert!(!invalid.is_valid());
    }

    #[test]
    fn print_dialog_state_submits_without_a_printer() {
        let mut dialog = PrintDialogState::default();
        assert!(!dialog.is_open());
        dialog.show();
        assert!(dialog.is_open());
        dialog.model_mut().copies = 3;
        let plan = dialog.submit(&context(2)).expect("plan submitted");
        assert_eq!(plan.raster_count(), 6);
        assert_eq!(dialog.last_plan().map(|p| p.raster_count()), Some(6));
        assert!(dialog.last_error().is_none());
        assert_eq!(dialog.status(), plan.summary);

        dialog.model_mut().range = PrintPageRange::Custom("bad".to_owned());
        assert!(dialog.submit(&context(2)).is_err());
        assert!(dialog.last_error().is_some());
        assert!(dialog.last_plan().is_none());
        dialog.hide();
        assert!(!dialog.is_open());
    }
}

#[cfg(test)]
mod impose_tests {
    use super::*;

    fn options() -> ImposeOptions {
        ImposeOptions::default()
    }

    fn approx(left: f32, right: f32) -> bool {
        (left - right).abs() < 1e-4
    }

    #[test]
    fn booklet_spread_order_includes_blank_padding() {
        let even = booklet_imposition(4, &options()).expect("even booklet");
        assert_eq!(even.sheet_count(), 1);
        assert_eq!(even.blank_cells(), 0);
        assert_eq!(even.page_order(), vec![Some(3), Some(0), Some(1), Some(2)]);

        let padded = booklet_imposition(5, &options()).expect("padded booklet");
        assert_eq!(padded.sheet_count(), 2);
        assert_eq!(padded.blank_cells(), 3);
        assert_eq!(
            padded.page_order(),
            vec![
                None,
                Some(0),
                Some(1),
                None,
                None,
                Some(2),
                Some(3),
                Some(4)
            ]
        );
        assert_eq!(padded, booklet_imposition(5, &options()).expect("repeat"));
        for page in &padded.pages {
            page.transform.validate().expect("finite transform");
            assert!(matches!(page.cell.side, SheetSide::Front | SheetSide::Back));
        }
        assert_eq!(
            padded.pages[0].cell,
            DestinationCell {
                sheet: 0,
                side: SheetSide::Front,
                row: 0,
                column: 0
            }
        );
        assert_eq!(padded.pages[2].cell.side, SheetSide::Back);
        assert!(padded.summary.contains("2 sheet(s)"));
    }

    #[test]
    fn booklet_rejects_empty_and_oversized_page_counts() {
        assert_eq!(
            booklet_imposition(0, &options()),
            Err(ImposeError::EmptyPageSet)
        );
        assert_eq!(
            booklet_imposition(MAX_IMPOSE_PAGES + 1, &options()),
            Err(ImposeError::TooManyPages(
                MAX_IMPOSE_PAGES + 1,
                MAX_IMPOSE_PAGES
            ))
        );
        let mut bad = options();
        bad.margin_pt = 400.0;
        assert!(matches!(
            booklet_imposition(4, &bad),
            Err(ImposeError::InvalidGeometry(_))
        ));
    }

    #[test]
    fn poster_tile_grid_and_overlap_are_deterministic() {
        let source = SizePt::new(612.0, 792.0).expect("source");
        let sheet = SizePt::new(612.0, 792.0).expect("sheet");
        let plan = poster_tiles(source, sheet, 200.0, 72.0).expect("poster");
        assert_eq!((plan.columns, plan.rows), (3, 3));
        assert_eq!(plan.tile_count(), 9);
        assert!(approx(plan.scale, 2.0));
        assert!(approx(plan.scaled_size.width, 1224.0));
        assert_eq!(plan.tiles[0].column, 0);
        assert_eq!(plan.tiles[0].row, 0);
        assert!(approx(plan.tiles[0].offset_x_pt, 0.0));
        assert!(approx(plan.tiles[0].source_x1, 306.0));
        let last = &plan.tiles[8];
        assert!(approx(last.offset_x_pt, 1080.0));
        assert!(approx(last.offset_y_pt, 1440.0));
        assert!(approx(last.source_x0, 540.0));
        assert!(approx(last.source_x1, 612.0));
        assert!(approx(last.transform.translate_x_pt, -1080.0));
        assert_eq!(
            plan,
            poster_tiles(source, sheet, 200.0, 72.0).expect("repeat")
        );
    }

    #[test]
    fn poster_rejects_bad_overlap_scale_geometry_and_huge_grids() {
        let source = SizePt::new(612.0, 792.0).expect("source");
        let sheet = SizePt::new(612.0, 792.0).expect("sheet");
        assert!(matches!(
            poster_tiles(source, sheet, 200.0, f32::NAN),
            Err(ImposeError::InvalidOverlap(value)) if value.is_nan()
        ));
        assert!(matches!(
            poster_tiles(source, sheet, 200.0, 612.0),
            Err(ImposeError::InvalidOverlap(_))
        ));
        assert!(matches!(
            poster_tiles(source, sheet, f32::NAN, 0.0),
            Err(ImposeError::InvalidScale(_))
        ));
        assert!(matches!(
            poster_tiles(
                SizePt {
                    width: 0.0,
                    height: 10.0
                },
                sheet,
                100.0,
                0.0
            ),
            Err(ImposeError::InvalidGeometry(_))
        ));
        let tiny = SizePt::new(1.0, 1.0).expect("tiny sheet");
        assert!(matches!(
            poster_tiles(source, tiny, 100.0, 0.0),
            Err(ImposeError::TooManyCells(_))
        ));
    }

    #[test]
    fn n_up_row_and_column_orders_are_both_deterministic() {
        let layout = NUpLayout::new(3, 2).expect("3x2");
        assert_eq!(layout.pages_per_sheet(), 6);
        let row = n_up_imposition(6, layout, NUpOrder::RowMajor, &options()).expect("row");
        let column = n_up_imposition(6, layout, NUpOrder::ColumnMajor, &options()).expect("column");
        assert_eq!(row.sheet_count(), 1);
        assert_eq!(
            row.page_order(),
            vec![Some(0), Some(1), Some(2), Some(3), Some(4), Some(5)]
        );
        assert_eq!(
            column.page_order(),
            vec![Some(0), Some(2), Some(4), Some(1), Some(3), Some(5)]
        );
        assert_eq!(
            row,
            n_up_imposition(6, layout, NUpOrder::RowMajor, &options()).expect("repeat")
        );
        assert!(row.summary.contains("row-major"));
        assert!(column.summary.contains("column-major"));

        let partial = n_up_imposition(
            7,
            NUpLayout::new(2, 2).expect("2x2"),
            NUpOrder::RowMajor,
            &options(),
        )
        .expect("partial");
        assert_eq!(partial.sheet_count(), 2);
        assert_eq!(partial.blank_cells(), 1);
        assert_eq!(
            partial.page_order(),
            vec![
                Some(0),
                Some(1),
                Some(2),
                Some(3),
                Some(4),
                Some(5),
                Some(6),
                None
            ]
        );
    }

    #[test]
    fn n_up_rejects_invalid_grids_and_counts() {
        assert_eq!(NUpLayout::new(0, 2), Err(ImposeError::InvalidGrid(2)));
        assert!(matches!(
            NUpLayout::new(MAX_GRID_EXTENT + 1, 1),
            Err(ImposeError::InvalidGrid(_))
        ));
        let layout = NUpLayout::new(2, 2).expect("2x2");
        assert_eq!(
            n_up_imposition(0, layout, NUpOrder::RowMajor, &options()),
            Err(ImposeError::EmptyPageSet)
        );
        assert_eq!(
            n_up_imposition(MAX_IMPOSE_PAGES + 1, layout, NUpOrder::RowMajor, &options()),
            Err(ImposeError::TooManyPages(
                MAX_IMPOSE_PAGES + 1,
                MAX_IMPOSE_PAGES
            ))
        );
    }

    #[test]
    fn fit_math_is_deterministic_for_actual_fit_and_custom() {
        let source = SizePt::new(612.0, 792.0).expect("source");
        let target = SizePt::new(306.0, 396.0).expect("target");
        let actual = resolve_fit(source, target, FitMode::Actual).expect("actual");
        assert!(approx(actual.scale, 1.0));
        assert!(approx(actual.rendered_width, 612.0));
        assert!(approx(actual.offset_x_pt, -153.0));
        assert!(!actual.fits(target));

        let fit = resolve_fit(source, target, FitMode::Fit).expect("fit");
        assert!(approx(fit.scale, 0.5));
        assert!(approx(fit.rendered_height, 396.0));
        assert!(approx(fit.offset_y_pt, 0.0));
        assert!(fit.fits(target));

        let square = SizePt::new(400.0, 400.0).expect("square");
        let fit_width = resolve_fit(source, square, FitMode::FitWidth).expect("width");
        let fit_height = resolve_fit(source, square, FitMode::FitHeight).expect("height");
        assert!(fit_width.scale > fit_height.scale);
        assert!(approx(fit_width.rendered_width, 400.0));
        assert!(approx(fit_height.rendered_height, 400.0));

        let custom = resolve_fit(source, target, FitMode::Custom(50.0)).expect("custom");
        assert_eq!(custom, fit);
        assert_eq!(FitMode::from_render(PageScaleMode::Fit), FitMode::Fit);
        assert_eq!(
            FitMode::from_render(PageScaleMode::CustomPercent(50.0)),
            FitMode::Custom(50.0)
        );
        assert_eq!(FitMode::FitWidth.label(), "fit-width");
    }

    #[test]
    fn fit_rejects_invalid_geometry_and_scale() {
        let source = SizePt::new(612.0, 792.0).expect("source");
        let target = SizePt::new(306.0, 396.0).expect("target");
        assert!(matches!(
            resolve_fit(
                SizePt {
                    width: 0.0,
                    height: 1.0,
                },
                target,
                FitMode::Fit
            ),
            Err(ImposeError::InvalidGeometry(_))
        ));
        assert!(matches!(
            resolve_fit(source, target, FitMode::Custom(f32::NAN)),
            Err(ImposeError::InvalidScale(value)) if value.is_nan()
        ));
        assert!(matches!(
            resolve_fit(source, target, FitMode::Custom(-5.0)),
            Err(ImposeError::InvalidScale(_))
        ));
        assert!(matches!(
            resolve_fit(
                source,
                SizePt {
                    width: f32::INFINITY,
                    height: 1.0
                },
                FitMode::Fit
            ),
            Err(ImposeError::InvalidGeometry(_))
        ));
    }

    #[test]
    fn marks_placement_is_deterministic_and_bounded() {
        let trim = SizePt::new(612.0, 792.0).expect("trim");
        let sheet = SizePt::new(680.0, 860.0).expect("sheet");
        let all = PrinterMarks {
            crop_marks: true,
            registration_marks: true,
            color_bars: true,
            page_info: true,
        };
        let plan = MarksPlan::build(trim, sheet, all).expect("marks");
        assert_eq!(plan.marks.len(), 14);
        assert_eq!(
            plan.marks
                .iter()
                .filter(|m| m.kind == MarkKind::Crop)
                .count(),
            8
        );
        assert_eq!(
            plan.marks
                .iter()
                .filter(|m| m.kind == MarkKind::Registration)
                .count(),
            4
        );
        let crop_only = MarksPlan::build(
            trim,
            sheet,
            PrinterMarks {
                crop_marks: true,
                ..PrinterMarks::default()
            },
        )
        .expect("crop only");
        assert_eq!(crop_only.marks.len(), 8);
        assert_eq!(plan, MarksPlan::build(trim, sheet, all).expect("repeat"));
        assert!(plan.summary.contains("14 rect(s)"));
        assert_eq!(
            MarksPlan::build(trim, trim, all),
            Err(ImposeError::InvalidGeometry(
                "crop mark does not fit on the sheet".to_owned()
            ))
        );
    }

    #[test]
    fn bleed_bounds_expand_trim_and_validate_inputs() {
        let trim = SizePt::new(612.0, 792.0).expect("trim");
        let bounds = BleedBounds::build(
            trim,
            Bleed {
                top: 3.0,
                right: 4.0,
                bottom: 5.0,
                left: 6.0,
            },
            MAX_BLEED_PT,
        )
        .expect("bleed");
        assert!(approx(bounds.x, -6.0));
        assert!(approx(bounds.y, -5.0));
        assert!(approx(bounds.width, 622.0));
        assert!(approx(bounds.height, 800.0));
        assert!(bounds.contains_trim(trim));
        assert!(matches!(
            BleedBounds::build(
                trim,
                Bleed {
                    left: -1.0,
                    ..Bleed::default()
                },
                MAX_BLEED_PT
            ),
            Err(ImposeError::InvalidGeometry(_))
        ));
        assert!(matches!(
            BleedBounds::build(
                trim,
                Bleed {
                    top: 100.0,
                    ..Bleed::default()
                },
                MAX_BLEED_PT
            ),
            Err(ImposeError::InvalidGeometry(_))
        ));
        assert!(matches!(
            BleedBounds::build(trim, Bleed::default(), 0.0),
            Err(ImposeError::InvalidGeometry(_))
        ));
    }

    #[test]
    fn separations_plan_is_order_independent_and_validated() {
        let first = vec![
            SeparationInput {
                name: "PANTONE 185 C".to_owned(),
                process: false,
            },
            SeparationInput {
                name: "Cyan".to_owned(),
                process: true,
            },
            SeparationInput {
                name: "Black".to_owned(),
                process: true,
            },
        ];
        let second = vec![
            SeparationInput {
                name: "Black".to_owned(),
                process: true,
            },
            SeparationInput {
                name: "PANTONE 185 C".to_owned(),
                process: false,
            },
            SeparationInput {
                name: "Cyan".to_owned(),
                process: true,
            },
        ];
        let plan = SeparationsPlan::build(&first).expect("separations");
        assert_eq!(plan, SeparationsPlan::build(&second).expect("repeat"));
        assert_eq!(plan.separations[0].name, "Black");
        assert_eq!(plan.separations[0].id, 1);
        assert_eq!(plan.separations[2].name, "PANTONE 185 C");
        assert!(!plan.separations[2].process);
        assert!(plan.summary.contains("2 process, 1 spot"));
        assert!(matches!(
            SeparationsPlan::build(&[
                SeparationInput {
                    name: "Cyan".to_owned(),
                    process: true
                },
                SeparationInput {
                    name: "Cyan".to_owned(),
                    process: false
                }
            ]),
            Err(ImposeError::InvalidSeparation(_))
        ));
        assert!(matches!(
            SeparationsPlan::build(&[SeparationInput {
                name: " ".to_owned(),
                process: true
            }]),
            Err(ImposeError::InvalidSeparation(_))
        ));
        assert!(matches!(
            SeparationsPlan::build(&[]),
            Err(ImposeError::InvalidSeparation(_))
        ));
    }

    #[test]
    fn output_intent_reports_missing_single_and_multiple_intents() {
        let missing = OutputIntentReport::build(&[]).expect("missing");
        assert!(missing.intent.is_none());
        assert_eq!(missing.warnings.len(), 1);

        let one = OutputIntent {
            subtype: OutputIntentSubtype::PdfX,
            name: "FOGRA39".to_owned(),
            registry: "CGATS21-2".to_owned(),
            rendering_intent: "relativeColorimetric".to_owned(),
        };
        let report = OutputIntentReport::build(std::slice::from_ref(&one)).expect("single");
        let resolved = report.intent.as_ref().expect("resolved");
        assert_eq!(resolved.rendering_intent, "RelativeColorimetric");
        assert_eq!(report.warnings.len(), 1);
        assert_eq!(
            report,
            OutputIntentReport::build(std::slice::from_ref(&one)).expect("repeat")
        );

        assert!(matches!(
            OutputIntentReport::build(&[one.clone(), one.clone()]),
            Err(ImposeError::InvalidOutputIntent(_))
        ));
        assert!(matches!(
            OutputIntentReport::build(&[OutputIntent {
                rendering_intent: "magic".to_owned(),
                ..one.clone()
            }]),
            Err(ImposeError::InvalidOutputIntent(_))
        ));
        assert!(matches!(
            OutputIntentReport::build(&[OutputIntent {
                name: String::new(),
                ..one
            }]),
            Err(ImposeError::InvalidOutputIntent(_))
        ));
    }

    #[test]
    fn production_plan_combines_every_print_production_option() {
        let trim = SizePt::new(612.0, 792.0).expect("trim");
        let sheet = SizePt::new(680.0, 860.0).expect("sheet");
        let options = ProductionOptions {
            marks: PrinterMarks {
                crop_marks: true,
                ..PrinterMarks::default()
            },
            bleed: Bleed {
                top: 3.0,
                right: 3.0,
                bottom: 3.0,
                left: 3.0,
            },
            separations: vec![
                SeparationInput {
                    name: "Cyan".to_owned(),
                    process: true,
                },
                SeparationInput {
                    name: "Spot".to_owned(),
                    process: false,
                },
            ],
            intents: vec![OutputIntent {
                subtype: OutputIntentSubtype::IccBased,
                name: "sRGB".to_owned(),
                registry: "IEC".to_owned(),
                rendering_intent: "Perceptual".to_owned(),
            }],
        };
        let plan = ProductionPlan::build(trim, sheet, &options).expect("production");
        assert_eq!(plan.marks.marks.len(), 8);
        assert!(approx(plan.bleed.width, 618.0));
        assert_eq!(plan.separations.separations.len(), 2);
        assert!(plan.output_intent.intent.is_some());
        assert!(
            plan.summary
                .contains("separations: 2 total, 1 process, 1 spot")
        );
        assert_eq!(
            plan,
            ProductionPlan::build(trim, sheet, &options).expect("repeat")
        );
    }

    #[test]
    fn duplex_plan_maps_pages_to_sheets_and_sides() {
        let single = DuplexPlan::build(5, Duplex::None, PaperSource::Auto).expect("single");
        assert_eq!(single.sheet_count, 5);
        assert_eq!(single.sides_used(), 5);
        assert!(single.sheets.iter().all(|s| s.back_page.is_none()));
        assert_eq!(single.sheets[0].binding_edge, "none");

        let long = DuplexPlan::build(5, Duplex::LongEdge, PaperSource::Tray(2)).expect("long");
        assert_eq!(long.sheet_count, 3);
        assert_eq!(long.sides_used(), 5);
        assert_eq!(long.sheets[0].front_page, 0);
        assert_eq!(long.sheets[0].back_page, Some(1));
        assert_eq!(long.sheets[2].back_page, None);
        assert_eq!(long.sheets[0].binding_edge, "long-edge");

        let short =
            DuplexPlan::build(5, Duplex::ShortEdge, PaperSource::ManualFeed).expect("short");
        assert_eq!(short.sheets[0].binding_edge, "short-edge");
        assert_ne!(short.summary, long.summary);
        assert_eq!(
            long,
            DuplexPlan::build(5, Duplex::LongEdge, PaperSource::Tray(2)).expect("repeat")
        );
    }

    #[test]
    fn duplex_and_paper_source_inputs_are_validated() {
        assert_eq!(
            DuplexPlan::build(0, Duplex::None, PaperSource::Auto),
            Err(ImposeError::EmptyPageSet)
        );
        assert_eq!(
            DuplexPlan::build(MAX_IMPOSE_PAGES + 1, Duplex::None, PaperSource::Auto),
            Err(ImposeError::TooManyPages(
                MAX_IMPOSE_PAGES + 1,
                MAX_IMPOSE_PAGES
            ))
        );
        assert_eq!(
            DuplexPlan::build(2, Duplex::None, PaperSource::Tray(0)),
            Err(ImposeError::UnsupportedOption(
                "tray number must be 1..=255".to_owned()
            ))
        );
        assert_eq!(PaperSource::Tray(3).label(), "tray-3");
        assert_eq!(PaperSource::Envelope.label(), "envelope");
    }

    #[test]
    fn geometry_helpers_sanitize_negative_and_non_finite_inputs() {
        assert!(SizePt::new(0.0, 10.0).is_err());
        assert!(SizePt::new(f32::NAN, 10.0).is_err());
        assert!(SizePt::new(10.0, f32::INFINITY).is_err());
        let paper = Paper::letter();
        let landscape = SizePt::from_paper(&paper, Orientation::Landscape).expect("landscape");
        assert!(approx(landscape.width, 792.0));
        assert!(approx(landscape.height, 612.0));

        let mut bad = options();
        bad.sheet = SizePt {
            width: f32::NAN,
            height: 792.0,
        };
        assert!(matches!(
            bad.validate(),
            Err(ImposeError::InvalidGeometry(_))
        ));
        bad = options();
        bad.margin_pt = -1.0;
        assert!(matches!(
            bad.validate(),
            Err(ImposeError::InvalidGeometry(_))
        ));

        assert!(PageTransform::identity().validate().is_ok());
        assert_eq!(
            PageTransform {
                rotation_degrees: 45,
                ..PageTransform::identity()
            }
            .validate(),
            Err(ImposeError::UnsupportedOption(
                "unsupported rotation 45 degrees".to_owned()
            ))
        );
        assert!(matches!(
            PageTransform {
                scale: 0.0,
                ..PageTransform::identity()
            }
            .validate(),
            Err(ImposeError::InvalidScale(_))
        ));
        assert!(matches!(
            PageTransform {
                translate_x_pt: f32::NAN,
                ..PageTransform::identity()
            }
            .validate(),
            Err(ImposeError::InvalidGeometry(_))
        ));
    }
}
