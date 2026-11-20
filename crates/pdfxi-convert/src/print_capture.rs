//! Local print-capture ingestion contract for "create PDF from any printable
//! application" (CREATE-001).
//!
//! ADR 0008 requires a local, file-based equivalent rather than an OS spooler,
//! printer driver, or remote service. This module accepts the *outputs* a host
//! already holds — caller-supplied rendered page images or a PostScript/HTML
//! payload — and produces a deterministic [`PrintCapturePlan`]: an ordered list
//! of pages with validated geometry and a stable summary. No printer, network,
//! process, or GUI boundary is crossed here.
//!
//! The plan is a description a host can hand to the existing PDF writers in
//! [`crate::create`]; this crate deliberately does not rasterize a native
//! application window. Existing modules are reused where possible:
//!
//! - [`crate::create::PageSize`] validates every resolved page geometry;
//! - [`ingest_postscript_str`] validates a PostScript payload through the
//!   bounded DSC reader instead of inventing a second parser;
//! - [`HTML_MAX_INPUT_BYTES`] bounds HTML payloads already held by the caller.
//!
//! All budgets and ordering rules are deterministic, so identical input always
//! yields an identical plan.

use crate::create::PageSize;
use crate::{HTML_MAX_INPUT_BYTES, PS_INGEST_MAX_BYTES, ingest_postscript_str};
use std::collections::HashSet;

/// Maximum number of captured pages accepted in a single plan.
pub const PRINT_CAPTURE_MAX_PAGES: usize = 10_000;

/// Maximum length of a caller-supplied capture label, in bytes.
pub const PRINT_CAPTURE_MAX_LABEL_BYTES: usize = 256;

/// The family of a caller-supplied captured page.
#[derive(Debug, Clone, PartialEq)]
pub enum CaptureSource {
    /// A page the host has already rasterized, described by its pixel geometry.
    ///
    /// The PDF page size is derived as `pixels / dpi * 72` points on each axis.
    /// No pixel buffer is copied or decoded here; the caller keeps the bytes.
    RenderedImage {
        /// Raster width in pixels.
        width_px: u32,
        /// Raster height in pixels.
        height_px: u32,
        /// Horizontal/vertical resolution used to convert pixels to points.
        dpi: f64,
    },
    /// A caller-held PostScript payload, validated with the bounded DSC reader.
    PostScript {
        /// The DSC/PostScript source text.
        payload: String,
    },
    /// A caller-held HTML markup payload.
    Html {
        /// UTF-8 markup already extracted or copied by the caller.
        markup: String,
    },
}

impl CaptureSource {
    /// Returns a stable machine-readable kind label.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::RenderedImage { .. } => "rendered-image",
            Self::PostScript { .. } => "postscript",
            Self::Html { .. } => "html",
        }
    }
}

/// One caller-supplied captured page with an explicit assembly sequence.
#[derive(Debug, Clone, PartialEq)]
pub struct CapturePage {
    sequence: usize,
    label: String,
    source: CaptureSource,
}

impl CapturePage {
    /// Create a captured page.
    ///
    /// `sequence` is the caller's assembly ordinal; pages are ordered by it
    /// ascending in the resulting plan. `label` is an opaque diagnostic tag.
    #[must_use]
    pub fn new(sequence: usize, label: impl Into<String>, source: CaptureSource) -> Self {
        Self {
            sequence,
            label: label.into(),
            source,
        }
    }

    /// Return the caller's assembly ordinal.
    #[must_use]
    pub const fn sequence(&self) -> usize {
        self.sequence
    }

    /// Return the opaque diagnostic label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the captured source description.
    #[must_use]
    pub fn source(&self) -> &CaptureSource {
        &self.source
    }
}

/// Options controlling capture-plan assembly.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PrintCaptureOptions {
    default_page_size: PageSize,
    max_pages: usize,
}

impl PrintCaptureOptions {
    /// Defaults: US Letter pages for vector payloads and at most
    /// [`PRINT_CAPTURE_MAX_PAGES`] pages.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            default_page_size: PageSize::LETTER,
            max_pages: PRINT_CAPTURE_MAX_PAGES,
        }
    }

    /// Replace the page size assigned to PostScript and HTML payloads.
    #[must_use]
    pub const fn with_default_page_size(mut self, page_size: PageSize) -> Self {
        self.default_page_size = page_size;
        self
    }

    /// Replace the page-count budget.
    #[must_use]
    pub const fn with_max_pages(mut self, max_pages: usize) -> Self {
        self.max_pages = max_pages;
        self
    }

    /// Return the default vector-payload page size.
    #[must_use]
    pub const fn default_page_size(&self) -> PageSize {
        self.default_page_size
    }

    /// Return the page-count budget.
    #[must_use]
    pub const fn max_pages(&self) -> usize {
        self.max_pages
    }
}

impl Default for PrintCaptureOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A typed failure while assembling a print-capture plan.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum PrintCaptureError {
    /// No captured pages were supplied.
    #[error("print capture has no pages")]
    EmptyInput,
    /// More pages were supplied than the configured budget.
    #[error("print capture has {count} pages (limit {limit})")]
    TooManyPages {
        /// Number of pages supplied.
        count: usize,
        /// Enforced page budget.
        limit: usize,
    },
    /// Two pages declared the same assembly sequence.
    #[error("duplicate capture sequence {sequence}")]
    DuplicateSequence {
        /// The repeated sequence number.
        sequence: usize,
    },
    /// A label was empty or exceeded the documented length bound.
    #[error("invalid capture label for sequence {sequence}: {reason}")]
    InvalidLabel {
        /// Sequence of the offending page.
        sequence: usize,
        /// Stable explanation.
        reason: &'static str,
    },
    /// A rendered image had zero-sized pixel geometry.
    #[error("invalid image geometry for sequence {sequence}: {width_px}x{height_px}")]
    InvalidImageGeometry {
        /// Sequence of the offending page.
        sequence: usize,
        /// Declared raster width.
        width_px: u32,
        /// Declared raster height.
        height_px: u32,
    },
    /// A rendered image carried an unusable DPI.
    #[error("invalid DPI for sequence {sequence}: {dpi}")]
    InvalidDpi {
        /// Sequence of the offending page.
        sequence: usize,
        /// Declared resolution.
        dpi: f64,
    },
    /// A resolved page size was rejected by the shared geometry validator.
    #[error("invalid page size for sequence {sequence}: {reason}")]
    InvalidPageSize {
        /// Sequence of the offending page.
        sequence: usize,
        /// Stable explanation from the validator.
        reason: String,
    },
    /// A text payload was empty.
    #[error("empty {kind} payload for sequence {sequence}")]
    EmptyPayload {
        /// Sequence of the offending page.
        sequence: usize,
        /// Stable source-kind label.
        kind: &'static str,
    },
    /// A text payload exceeded its documented byte budget.
    #[error("{kind} payload for sequence {sequence} has {bytes} bytes (limit {limit})")]
    PayloadTooLarge {
        /// Sequence of the offending page.
        sequence: usize,
        /// Stable source-kind label.
        kind: &'static str,
        /// Number of bytes observed.
        bytes: usize,
        /// Enforced byte budget.
        limit: usize,
    },
    /// A PostScript payload was rejected by the shared DSC reader.
    #[error("PostScript payload for sequence {sequence} is not supported: {reason}")]
    PostScript {
        /// Sequence of the offending page.
        sequence: usize,
        /// Reason from the DSC reader.
        reason: String,
    },
}

/// One resolved page in a deterministic capture plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PlannedCapturePage {
    sequence: usize,
    label: String,
    source_kind: &'static str,
    page_size: PageSize,
}

impl PlannedCapturePage {
    /// Return the assembly ordinal of this page.
    #[must_use]
    pub const fn sequence(&self) -> usize {
        self.sequence
    }

    /// Return the opaque source label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the stable source-kind label.
    #[must_use]
    pub const fn source_kind(&self) -> &'static str {
        self.source_kind
    }

    /// Return the validated page geometry in PDF points.
    #[must_use]
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }
}

/// A deterministic assembly plan for a print-capture job.
#[derive(Debug, Clone, PartialEq)]
pub struct PrintCapturePlan {
    pages: Vec<PlannedCapturePage>,
    summary: String,
}

impl PrintCapturePlan {
    /// Return the ordered, resolved pages.
    #[must_use]
    pub fn pages(&self) -> &[PlannedCapturePage] {
        &self.pages
    }

    /// Return the number of planned pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Return a stable, human-readable summary of the plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// Resolve a text payload's page size, reusing the shared geometry validator.
fn validate_page_size(sequence: usize, page_size: PageSize) -> Result<PageSize, PrintCaptureError> {
    PageSize::new(page_size.width, page_size.height).map_err(|error| {
        let reason = match error {
            crate::CreateError::InvalidPageSize(reason) => reason,
            other => other.to_string(),
        };
        PrintCaptureError::InvalidPageSize { sequence, reason }
    })
}

/// Resolve one caller-supplied page to a validated geometry.
fn resolve_page(
    page: &CapturePage,
    options: &PrintCaptureOptions,
) -> Result<PlannedCapturePage, PrintCaptureError> {
    let sequence = page.sequence;
    if page.label.is_empty() || page.label.len() > PRINT_CAPTURE_MAX_LABEL_BYTES {
        let reason = if page.label.is_empty() {
            "label must not be empty"
        } else {
            "label exceeds the byte bound"
        };
        return Err(PrintCaptureError::InvalidLabel { sequence, reason });
    }

    let page_size = match &page.source {
        CaptureSource::RenderedImage {
            width_px,
            height_px,
            dpi,
        } => {
            if *width_px == 0 || *height_px == 0 {
                return Err(PrintCaptureError::InvalidImageGeometry {
                    sequence,
                    width_px: *width_px,
                    height_px: *height_px,
                });
            }
            if !dpi.is_finite() || *dpi <= 0.0 {
                return Err(PrintCaptureError::InvalidDpi {
                    sequence,
                    dpi: *dpi,
                });
            }
            let width_pt = f64::from(*width_px) / *dpi * 72.0;
            let height_pt = f64::from(*height_px) / *dpi * 72.0;
            validate_page_size(
                sequence,
                PageSize {
                    width: width_pt,
                    height: height_pt,
                },
            )?
        }
        CaptureSource::PostScript { payload } => {
            if payload.is_empty() {
                return Err(PrintCaptureError::EmptyPayload {
                    sequence,
                    kind: page.source.kind(),
                });
            }
            if payload.len() > PS_INGEST_MAX_BYTES {
                return Err(PrintCaptureError::PayloadTooLarge {
                    sequence,
                    kind: page.source.kind(),
                    bytes: payload.len(),
                    limit: PS_INGEST_MAX_BYTES,
                });
            }
            ingest_postscript_str(payload).map_err(|error| PrintCaptureError::PostScript {
                sequence,
                reason: error.to_string(),
            })?;
            validate_page_size(sequence, options.default_page_size)?
        }
        CaptureSource::Html { markup } => {
            if markup.is_empty() {
                return Err(PrintCaptureError::EmptyPayload {
                    sequence,
                    kind: page.source.kind(),
                });
            }
            if markup.len() > HTML_MAX_INPUT_BYTES {
                return Err(PrintCaptureError::PayloadTooLarge {
                    sequence,
                    kind: page.source.kind(),
                    bytes: markup.len(),
                    limit: HTML_MAX_INPUT_BYTES,
                });
            }
            validate_page_size(sequence, options.default_page_size)?
        }
    };

    Ok(PlannedCapturePage {
        sequence,
        label: page.label.clone(),
        source_kind: page.source.kind(),
        page_size,
    })
}

/// Build a deterministic assembly plan from caller-supplied captured pages.
///
/// Pages are validated first, then ordered by ascending `sequence` (ties broken
/// by label, though equal sequences are rejected as [`PrintCaptureError::DuplicateSequence`]).
/// The returned plan is independent of the input ordering.
///
/// # Errors
///
/// Returns [`PrintCaptureError::EmptyInput`] when `pages` is empty,
/// [`PrintCaptureError::TooManyPages`] over the budget, and the typed validation
/// errors above for malformed pages.
pub fn build_print_capture_plan(
    pages: &[CapturePage],
    options: &PrintCaptureOptions,
) -> Result<PrintCapturePlan, PrintCaptureError> {
    if pages.is_empty() {
        return Err(PrintCaptureError::EmptyInput);
    }
    if pages.len() > options.max_pages {
        return Err(PrintCaptureError::TooManyPages {
            count: pages.len(),
            limit: options.max_pages,
        });
    }

    let mut seen = HashSet::with_capacity(pages.len());
    for page in pages {
        if !seen.insert(page.sequence) {
            return Err(PrintCaptureError::DuplicateSequence {
                sequence: page.sequence,
            });
        }
    }

    let mut resolved: Vec<PlannedCapturePage> = pages
        .iter()
        .map(|page| resolve_page(page, options))
        .collect::<Result<Vec<_>, _>>()?;
    resolved.sort_by(|left, right| {
        left.sequence
            .cmp(&right.sequence)
            .then_with(|| left.label.cmp(&right.label))
    });

    let fragments: Vec<String> = resolved
        .iter()
        .map(|page| {
            format!(
                "{}:{}:{}x{}",
                page.sequence, page.source_kind, page.page_size.width, page.page_size.height
            )
        })
        .collect();
    let summary = format!("{} pages [{}]", resolved.len(), fragments.join(", "));

    Ok(PrintCapturePlan {
        pages: resolved,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn image(sequence: usize, width_px: u32, height_px: u32, dpi: f64) -> CapturePage {
        CapturePage::new(
            sequence,
            format!("page-{sequence}"),
            CaptureSource::RenderedImage {
                width_px,
                height_px,
                dpi,
            },
        )
    }

    #[test]
    fn pages_are_ordered_by_sequence_not_input_order() {
        let pages = vec![
            image(3, 300, 300, 72.0),
            image(1, 200, 200, 72.0),
            image(2, 100, 100, 72.0),
        ];
        let plan = build_print_capture_plan(&pages, &PrintCaptureOptions::new()).expect("plan");
        let order: Vec<usize> = plan
            .pages()
            .iter()
            .map(PlannedCapturePage::sequence)
            .collect();
        assert_eq!(order, vec![1, 2, 3]);
        assert_eq!(plan.page_count(), 3);
    }

    #[test]
    fn rendered_image_page_size_is_derived_from_pixels_and_dpi() {
        let plan =
            build_print_capture_plan(&[image(1, 612, 792, 72.0)], &PrintCaptureOptions::new())
                .expect("plan");
        let size = plan.pages()[0].page_size();
        assert!((size.width - 612.0).abs() < 1e-9);
        assert!((size.height - 792.0).abs() < 1e-9);
    }

    #[test]
    fn invalid_page_sizes_are_rejected_with_a_typed_error() {
        let bad_default = PrintCaptureOptions::new().with_default_page_size(PageSize {
            width: -1.0,
            height: 100.0,
        });
        let page = CapturePage::new(
            1,
            "html",
            CaptureSource::Html {
                markup: "<p>hi</p>".to_owned(),
            },
        );
        let error = build_print_capture_plan(&[page], &bad_default)
            .expect_err("invalid default page size must be rejected");
        assert!(matches!(
            error,
            PrintCaptureError::InvalidPageSize { sequence: 1, .. }
        ));

        let zero_dpi =
            build_print_capture_plan(&[image(1, 100, 100, 0.0)], &PrintCaptureOptions::new())
                .expect_err("zero dpi must be rejected");
        assert!(matches!(zero_dpi, PrintCaptureError::InvalidDpi { .. }));
    }

    #[test]
    fn oversized_rendered_geometry_is_rejected() {
        let huge = image(1, u32::MAX, u32::MAX, 1.0);
        let error = build_print_capture_plan(&[huge], &PrintCaptureOptions::new())
            .expect_err("pixels at 1 dpi exceed the page-size bound");
        assert!(matches!(error, PrintCaptureError::InvalidPageSize { .. }));
    }

    #[test]
    fn duplicate_sequences_and_empty_labels_are_rejected() {
        let error = build_print_capture_plan(
            &[image(1, 10, 10, 72.0), image(1, 10, 10, 72.0)],
            &PrintCaptureOptions::new(),
        )
        .expect_err("duplicate sequence must be rejected");
        assert!(matches!(
            error,
            PrintCaptureError::DuplicateSequence { sequence: 1 }
        ));

        let blank = CapturePage::new(
            1,
            "",
            CaptureSource::Html {
                markup: "<p>x</p>".to_owned(),
            },
        );
        let error = build_print_capture_plan(&[blank], &PrintCaptureOptions::new())
            .expect_err("blank label must be rejected");
        assert!(matches!(error, PrintCaptureError::InvalidLabel { .. }));
    }

    #[test]
    fn postscript_payloads_are_validated_through_the_shared_reader() {
        let valid = "%!PS-Adobe-3.0\n%%BoundingBox: 0 0 100 100\n%%Pages: 1\n%%Page: 1 1\nshowpage\n%%Trailer\n";
        let page = CapturePage::new(
            1,
            "ps",
            CaptureSource::PostScript {
                payload: valid.to_owned(),
            },
        );
        let plan =
            build_print_capture_plan(&[page], &PrintCaptureOptions::new()).expect("valid ps");
        assert_eq!(plan.pages()[0].source_kind(), "postscript");

        let bad = CapturePage::new(
            1,
            "ps",
            CaptureSource::PostScript {
                payload: "not postscript".to_owned(),
            },
        );
        let error = build_print_capture_plan(&[bad], &PrintCaptureOptions::new())
            .expect_err("invalid ps must be rejected");
        assert!(matches!(error, PrintCaptureError::PostScript { .. }));
    }

    #[test]
    fn empty_and_over_budget_inputs_are_rejected() {
        let error = build_print_capture_plan(&[], &PrintCaptureOptions::new())
            .expect_err("empty input must be rejected");
        assert_eq!(error, PrintCaptureError::EmptyInput);

        let options = PrintCaptureOptions::new().with_max_pages(1);
        let two = vec![image(1, 10, 10, 72.0), image(2, 10, 10, 72.0)];
        let error = build_print_capture_plan(&two, &options)
            .expect_err("over-budget input must be rejected");
        assert!(matches!(
            error,
            PrintCaptureError::TooManyPages { count: 2, limit: 1 }
        ));
    }

    #[test]
    fn empty_and_oversized_text_payloads_are_rejected() {
        let empty = CapturePage::new(
            1,
            "html",
            CaptureSource::Html {
                markup: String::new(),
            },
        );
        let error = build_print_capture_plan(&[empty], &PrintCaptureOptions::new())
            .expect_err("empty markup must be rejected");
        assert!(matches!(error, PrintCaptureError::EmptyPayload { .. }));

        let oversized = CapturePage::new(
            1,
            "html",
            CaptureSource::Html {
                markup: "x".repeat(HTML_MAX_INPUT_BYTES + 1),
            },
        );
        let error = build_print_capture_plan(&[oversized], &PrintCaptureOptions::new())
            .expect_err("oversized markup must be rejected");
        assert!(matches!(error, PrintCaptureError::PayloadTooLarge { .. }));
    }

    #[test]
    fn plan_summary_is_byte_for_byte_deterministic() {
        let pages = vec![image(2, 300, 300, 72.0), image(1, 150, 150, 72.0)];
        let first = build_print_capture_plan(&pages, &PrintCaptureOptions::new()).expect("plan");
        let second = build_print_capture_plan(&pages, &PrintCaptureOptions::new()).expect("plan");
        assert_eq!(first, second);
        assert_eq!(first.summary(), second.summary());
        assert!(first.summary().starts_with("2 pages [1:rendered-image"));
    }
}
