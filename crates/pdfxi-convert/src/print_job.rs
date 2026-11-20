//! Print-job envelope model for a local "virtual PDF printer" (CREATE-002).
//!
//! ADR 0008 requires a local equivalent with no spooler, OS printer driver, or
//! host integration. This module models the *data* a virtual printer would
//! receive as a [`PrintJob`]: job id, source application, paper size,
//! orientation, copies, page range, and source page count. It validates the
//! envelope with typed errors and resolves a deterministic [`PrintJobPlan`]
//! that a host can feed to the existing PDF writers. No spooler is contacted
//! and no rasterization happens here.
//!
//! [`parse_page_range`] implements a small, documented one-based range grammar
//! (`all`, `N`, `N-M`, `N-`) and reuses the crate's shared [`PageSize`]
//! validator for paper geometry.

use crate::create::PageSize;
use std::collections::BTreeSet;

/// Largest accepted copy count.
pub const PRINT_MAX_COPIES: u16 = 999;

/// Largest accepted source page count.
pub const PRINT_MAX_PAGES: usize = 10_000;

/// Maximum length of a job id, in bytes.
pub const PRINT_MAX_JOB_ID_BYTES: usize = 256;

/// Maximum length of a source-application label, in bytes.
pub const PRINT_MAX_SOURCE_APP_BYTES: usize = 256;

/// Maximum number of comma-separated page-range tokens.
pub const PRINT_MAX_RANGE_TOKENS: usize = 512;

/// Paper size requested by a print job.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub enum PaperSize {
    /// US Letter, 612 by 792 points.
    #[default]
    Letter,
    /// ISO A4, approximately 595.28 by 841.89 points.
    A4,
    /// US Legal, 612 by 1008 points.
    Legal,
    /// Caller-supplied size in PDF points.
    Custom {
        /// Page width in points.
        width_pt: f64,
        /// Page height in points.
        height_pt: f64,
    },
}

impl PaperSize {
    /// Resolve the portrait media geometry, reusing the shared page validator.
    ///
    /// # Errors
    ///
    /// Returns [`PrintJobError::InvalidPaperSize`] when a custom size is
    /// non-finite, non-positive, or oversized.
    pub fn portrait_size(self) -> Result<PageSize, PrintJobError> {
        let size = match self {
            Self::Letter => PageSize::LETTER,
            Self::A4 => PageSize::A4,
            Self::Legal => PageSize {
                width: 612.0,
                height: 1008.0,
            },
            Self::Custom {
                width_pt,
                height_pt,
            } => PageSize::new(width_pt, height_pt).map_err(|error| {
                PrintJobError::InvalidPaperSize {
                    reason: error.to_string(),
                }
            })?,
        };
        Ok(size)
    }
}

/// Page orientation applied to the resolved media box.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Orientation {
    /// Portrait: width < height as supplied by the paper size.
    #[default]
    Portrait,
    /// Landscape: width and height are swapped.
    Landscape,
}

/// The pages a job requests, expressed in the documented range grammar.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum PageRange {
    /// Every page of the source document.
    #[default]
    All,
    /// A parsed one-based range expression such as `1-3,5,7-`.
    Spec(String),
}

impl PageRange {
    /// Request every source page.
    #[must_use]
    pub const fn all() -> Self {
        Self::All
    }

    /// Request a one-based range expression.
    #[must_use]
    pub fn spec(text: impl Into<String>) -> Self {
        Self::Spec(text.into())
    }

    /// Return the raw expression, or `all` for [`PageRange::All`].
    #[must_use]
    pub fn as_str(&self) -> &str {
        match self {
            Self::All => "all",
            Self::Spec(text) => text,
        }
    }
}

/// A typed failure while validating or resolving a print job.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PrintJobError {
    /// The job id was empty.
    #[error("print job id must not be empty")]
    EmptyJobId,
    /// The job id exceeded the documented byte bound.
    #[error("print job id exceeds {limit} bytes")]
    JobIdTooLong {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The source-application label was empty.
    #[error("source application must not be empty")]
    EmptySourceApp,
    /// The source-application label exceeded the documented byte bound.
    #[error("source application exceeds {limit} bytes")]
    SourceAppTooLong {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The source document had no pages.
    #[error("print job has no source pages")]
    EmptyDocument,
    /// The source document exceeded the page budget.
    #[error("print job has {count} pages (limit {limit})")]
    TooManyPages {
        /// Number of source pages.
        count: usize,
        /// Enforced page budget.
        limit: usize,
    },
    /// The copy count was outside `1..=PRINT_MAX_COPIES`.
    #[error("copies must be in 1..={limit}, found {value}")]
    InvalidCopies {
        /// Rejected copy count.
        value: u16,
        /// Largest accepted copy count.
        limit: u16,
    },
    /// The paper size was invalid.
    #[error("invalid paper size: {reason}")]
    InvalidPaperSize {
        /// Stable explanation from the geometry validator.
        reason: String,
    },
    /// The page-range expression was malformed or out of bounds.
    #[error("invalid page range '{detail}'")]
    InvalidPageRange {
        /// The offending expression or token.
        detail: String,
    },
}

/// A virtual-printer job envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintJob {
    job_id: String,
    source_app: String,
    paper_size: PaperSize,
    orientation: Orientation,
    copies: u16,
    total_pages: usize,
    page_range: PageRange,
}

impl PrintJob {
    /// Create a print job from its envelope fields.
    #[must_use]
    pub fn new(
        job_id: impl Into<String>,
        source_app: impl Into<String>,
        paper_size: PaperSize,
        orientation: Orientation,
        copies: u16,
        total_pages: usize,
        page_range: PageRange,
    ) -> Self {
        Self {
            job_id: job_id.into(),
            source_app: source_app.into(),
            paper_size,
            orientation,
            copies,
            total_pages,
            page_range,
        }
    }

    /// Create a job for a single source page with default media and one copy.
    #[must_use]
    pub fn simple(
        job_id: impl Into<String>,
        source_app: impl Into<String>,
        total_pages: usize,
    ) -> Self {
        Self::new(
            job_id,
            source_app,
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            total_pages,
            PageRange::All,
        )
    }

    /// Return the job id.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Return the source-application label.
    #[must_use]
    pub fn source_app(&self) -> &str {
        &self.source_app
    }

    /// Return the requested paper size.
    #[must_use]
    pub const fn paper_size(&self) -> PaperSize {
        self.paper_size
    }

    /// Return the requested orientation.
    #[must_use]
    pub const fn orientation(&self) -> Orientation {
        self.orientation
    }

    /// Return the requested copy count.
    #[must_use]
    pub const fn copies(&self) -> u16 {
        self.copies
    }

    /// Return the source page count.
    #[must_use]
    pub const fn total_pages(&self) -> usize {
        self.total_pages
    }

    /// Return the requested page range.
    #[must_use]
    pub fn page_range(&self) -> &PageRange {
        &self.page_range
    }

    /// Validate every envelope field without producing a plan.
    ///
    /// # Errors
    ///
    /// Returns the typed [`PrintJobError`] variants for malformed ids, empty or
    /// oversized documents, out-of-range copies, invalid paper geometry, or an
    /// invalid page range.
    pub fn validate(&self) -> Result<(), PrintJobError> {
        if self.job_id.is_empty() {
            return Err(PrintJobError::EmptyJobId);
        }
        if self.job_id.len() > PRINT_MAX_JOB_ID_BYTES {
            return Err(PrintJobError::JobIdTooLong {
                limit: PRINT_MAX_JOB_ID_BYTES,
            });
        }
        if self.source_app.is_empty() {
            return Err(PrintJobError::EmptySourceApp);
        }
        if self.source_app.len() > PRINT_MAX_SOURCE_APP_BYTES {
            return Err(PrintJobError::SourceAppTooLong {
                limit: PRINT_MAX_SOURCE_APP_BYTES,
            });
        }
        if self.total_pages == 0 {
            return Err(PrintJobError::EmptyDocument);
        }
        if self.total_pages > PRINT_MAX_PAGES {
            return Err(PrintJobError::TooManyPages {
                count: self.total_pages,
                limit: PRINT_MAX_PAGES,
            });
        }
        if self.copies == 0 || self.copies > PRINT_MAX_COPIES {
            return Err(PrintJobError::InvalidCopies {
                value: self.copies,
                limit: PRINT_MAX_COPIES,
            });
        }
        self.paper_size.portrait_size()?;
        parse_page_range(self.page_range.as_str(), self.total_pages)?;
        Ok(())
    }

    /// Resolve a deterministic print plan, validating the envelope first.
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`PrintJob::validate`].
    pub fn resolve_plan(&self) -> Result<PrintJobPlan, PrintJobError> {
        self.validate()?;
        let one_based = parse_page_range(self.page_range.as_str(), self.total_pages)?;
        let pages: Vec<usize> = one_based.into_iter().map(|page| page - 1).collect();
        let portrait = self.paper_size.portrait_size()?;
        let media = match self.orientation {
            Orientation::Portrait => portrait,
            Orientation::Landscape => PageSize {
                width: portrait.height,
                height: portrait.width,
            },
        };
        let summary = format!(
            "job={} app={} paper={:?} orientation={:?} copies={} pages=[{}]",
            self.job_id,
            self.source_app,
            self.paper_size,
            self.orientation,
            self.copies,
            pages
                .iter()
                .map(|page| page.to_string())
                .collect::<Vec<_>>()
                .join(",")
        );
        Ok(PrintJobPlan {
            pages,
            copies: self.copies,
            media,
            orientation: self.orientation,
            summary,
        })
    }
}

/// A deterministic, host-consumable conversion plan for a print job.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintJobPlan {
    pages: Vec<usize>,
    copies: u16,
    media: PageSize,
    orientation: Orientation,
    summary: String,
}

impl PrintJobPlan {
    /// Return the zero-based source page indices to render, in ascending order.
    #[must_use]
    pub fn pages(&self) -> &[usize] {
        &self.pages
    }

    /// Return the number of distinct pages in the plan.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Return the requested copy count.
    #[must_use]
    pub const fn copies(&self) -> u16 {
        self.copies
    }

    /// Return the resolved (orientation-applied) media box.
    #[must_use]
    pub const fn media(&self) -> PageSize {
        self.media
    }

    /// Return the requested orientation.
    #[must_use]
    pub const fn orientation(&self) -> Orientation {
        self.orientation
    }

    /// Return a stable, human-readable summary of the plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Parse one comma-separated page-range token into one-based page numbers.
fn parse_range_token(
    token: &str,
    total_pages: usize,
    out: &mut BTreeSet<usize>,
) -> Result<(), PrintJobError> {
    let invalid = || PrintJobError::InvalidPageRange {
        detail: token.to_owned(),
    };
    let token = token.trim();
    if token.is_empty() {
        return Err(invalid());
    }
    if token.eq_ignore_ascii_case("all") {
        for page in 1..=total_pages {
            out.insert(page);
        }
        return Ok(());
    }
    let (first, last) = match token.split_once('-') {
        None => {
            let page: usize = token.parse().map_err(|_| invalid())?;
            (page, page)
        }
        Some((first, last)) => {
            let first_page: usize = if first.is_empty() {
                1
            } else {
                first.parse().map_err(|_| invalid())?
            };
            let last_page: usize = if last.is_empty() {
                total_pages
            } else {
                last.parse().map_err(|_| invalid())?
            };
            if last_page < first_page {
                return Err(invalid());
            }
            (first_page, last_page)
        }
    };
    if first == 0 || last == 0 || first > total_pages || last > total_pages {
        return Err(invalid());
    }
    for page in first..=last {
        out.insert(page);
    }
    Ok(())
}

/// Parse a one-based page-range expression against a source page count.
///
/// The grammar accepts `all` (case-insensitive) or comma-separated tokens of
/// `N` (one page), `N-M` (inclusive), and `N-` (N through the last page).
/// Results are deduplicated and returned in ascending order. Zero, negative,
/// reversed, non-numeric, out-of-bounds, and over-tokenized expressions are
/// rejected with [`PrintJobError::InvalidPageRange`].
///
/// # Errors
///
/// Returns [`PrintJobError::InvalidPageRange`] for any malformed expression and
/// [`PrintJobError::EmptyDocument`] when `total_pages` is zero.
pub fn parse_page_range(expression: &str, total_pages: usize) -> Result<Vec<usize>, PrintJobError> {
    if total_pages == 0 {
        return Err(PrintJobError::EmptyDocument);
    }
    if expression.trim().is_empty() {
        return Err(PrintJobError::InvalidPageRange {
            detail: expression.to_owned(),
        });
    }
    let tokens: Vec<&str> = expression.split(',').collect();
    if tokens.len() > PRINT_MAX_RANGE_TOKENS {
        return Err(PrintJobError::InvalidPageRange {
            detail: format!("more than {PRINT_MAX_RANGE_TOKENS} tokens"),
        });
    }
    let mut pages = BTreeSet::new();
    for token in tokens {
        parse_range_token(token, total_pages, &mut pages)?;
    }
    if pages.is_empty() {
        return Err(PrintJobError::InvalidPageRange {
            detail: expression.to_owned(),
        });
    }
    Ok(pages.into_iter().collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn job(copies: u16, total_pages: usize, range: PageRange) -> PrintJob {
        PrintJob::new(
            "job-1",
            "Local App",
            PaperSize::Letter,
            Orientation::Portrait,
            copies,
            total_pages,
            range,
        )
    }

    #[test]
    fn page_range_parses_lists_and_open_ended_ranges() {
        assert_eq!(
            parse_page_range("1-3,5,7-9", 10).expect("range"),
            vec![1, 2, 3, 5, 7, 8, 9]
        );
        assert_eq!(
            parse_page_range("2-", 4).expect("open range"),
            vec![2, 3, 4]
        );
        assert_eq!(parse_page_range("ALL", 3).expect("all"), vec![1, 2, 3]);
        assert_eq!(parse_page_range("4,2,2,4", 5).expect("dedupe"), vec![2, 4]);
    }

    #[test]
    fn page_range_rejects_malformed_and_out_of_bounds_expressions() {
        for expression in [
            "", "0", "1-0", "5-3", "1,,2", "abc", "1-2-3", "1-11", "-1-2",
        ] {
            assert!(
                matches!(
                    parse_page_range(expression, 10),
                    Err(PrintJobError::InvalidPageRange { .. })
                ),
                "expected rejection for '{expression}'"
            );
        }
        assert_eq!(parse_page_range("1", 0), Err(PrintJobError::EmptyDocument));
    }

    #[test]
    fn print_job_validation_accepts_a_well_formed_envelope() {
        let valid = job(2, 5, PageRange::spec("1-3"));
        assert!(valid.validate().is_ok());
    }

    #[test]
    fn print_job_validation_rejects_empty_ids_and_source_apps() {
        let empty_id = PrintJob::new(
            "",
            "App",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            1,
            PageRange::all(),
        );
        assert_eq!(empty_id.validate(), Err(PrintJobError::EmptyJobId));
        let empty_app = PrintJob::new(
            "id",
            "",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            1,
            PageRange::all(),
        );
        assert_eq!(empty_app.validate(), Err(PrintJobError::EmptySourceApp));
    }

    #[test]
    fn copies_bounds_are_enforced() {
        assert_eq!(
            job(0, 1, PageRange::all()).validate(),
            Err(PrintJobError::InvalidCopies {
                value: 0,
                limit: PRINT_MAX_COPIES
            })
        );
        assert_eq!(
            job(PRINT_MAX_COPIES + 1, 1, PageRange::all()).validate(),
            Err(PrintJobError::InvalidCopies {
                value: PRINT_MAX_COPIES + 1,
                limit: PRINT_MAX_COPIES
            })
        );
        assert!(
            job(PRINT_MAX_COPIES, 1, PageRange::all())
                .validate()
                .is_ok()
        );
    }

    #[test]
    fn empty_and_oversized_documents_are_rejected() {
        assert_eq!(
            job(1, 0, PageRange::all()).validate(),
            Err(PrintJobError::EmptyDocument)
        );
        assert_eq!(
            job(1, PRINT_MAX_PAGES + 1, PageRange::all()).validate(),
            Err(PrintJobError::TooManyPages {
                count: PRINT_MAX_PAGES + 1,
                limit: PRINT_MAX_PAGES
            })
        );
    }

    #[test]
    fn invalid_paper_size_is_a_typed_error() {
        let custom = PaperSize::Custom {
            width_pt: 0.0,
            height_pt: 100.0,
        };
        assert!(matches!(
            custom.portrait_size(),
            Err(PrintJobError::InvalidPaperSize { .. })
        ));
        let job = PrintJob::new(
            "id",
            "App",
            custom,
            Orientation::Portrait,
            1,
            1,
            PageRange::all(),
        );
        assert!(matches!(
            job.validate(),
            Err(PrintJobError::InvalidPaperSize { .. })
        ));
    }

    #[test]
    fn resolve_plan_applies_orientation_and_expands_pages() {
        let landscape = PrintJob::new(
            "job-2",
            "App",
            PaperSize::Letter,
            Orientation::Landscape,
            3,
            6,
            PageRange::spec("2,4-5"),
        );
        let plan = landscape.resolve_plan().expect("plan");
        assert_eq!(plan.pages(), &[1, 3, 4]);
        assert_eq!(plan.copies(), 3);
        assert_eq!(plan.orientation(), Orientation::Landscape);
        assert_eq!(plan.media().width, 792.0);
        assert_eq!(plan.media().height, 612.0);
    }

    #[test]
    fn plan_is_byte_for_byte_deterministic() {
        let first = job(2, 5, PageRange::spec("1-3,5"))
            .resolve_plan()
            .expect("plan");
        let second = job(2, 5, PageRange::spec("1-3,5"))
            .resolve_plan()
            .expect("plan");
        assert_eq!(first, second);
        assert_eq!(first.summary(), second.summary());
        assert!(first.summary().contains("copies=2"));
    }

    #[test]
    fn reversed_range_expression_is_rejected_during_validation() {
        let bad = job(1, 5, PageRange::spec("4-2"));
        assert!(matches!(
            bad.validate(),
            Err(PrintJobError::InvalidPageRange { .. })
        ));
        assert!(bad.resolve_plan().is_err());
    }
}
