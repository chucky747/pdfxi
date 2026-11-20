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
