//! End-to-end library-level PDF assembly contract for CREATE-001 and
//! CREATE-002 (ADR 0008).
//!
//! [`crate::print_capture`] validates caller-supplied rendered images,
//! PostScript, and HTML into an ordered [`crate::PrintCapturePlan`], and
//! [`crate::print_job`] validates a virtual-printer envelope into a
//! [`crate::PrintJobPlan`]. This module is the missing *join*: it reconciles the
//! two into a single deterministic [`PdfAssemblyPlan`] with an explicit page
//! layout (media box, centring, scale, and collated copy order) that a host can
//! hand to the existing PDF writers in [`crate::create`].
//!
//! The whole operation is pure planning over caller-supplied descriptions. No
//! spooler, printer driver, GUI capture, image decoding, or network access is
//! performed here; those remain host responsibilities. Every budget, ordering
//! rule, and printed number is deterministic, so equivalent inputs always yield
//! a byte-identical [`PdfAssemblyPlan::summary`].

use crate::create::PageSize;
use crate::print_capture::{
    CapturePage, PlannedCapturePage, PrintCaptureError, PrintCaptureOptions,
    build_print_capture_plan,
};
use crate::print_job::{Orientation, PrintJob, PrintJobError, PrintJobPlan};
use std::collections::{HashMap, HashSet};

/// Maximum number of output pages in one assembly plan, including copies.
pub const ASSEMBLY_MAX_OUTPUT_PAGES: usize = 10_000;

/// Stable source-kind label for a caller-supplied rendered image.
const RENDERED_IMAGE_KIND: &str = "rendered-image";

/// Options controlling the reconciliation and layout step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct AssemblyOptions {
    capture: PrintCaptureOptions,
    fit_to_media: bool,
}

impl AssemblyOptions {
    /// Defaults: share the print-capture defaults and scale oversized images
    /// down to fit the resolved media box.
    #[must_use]
    pub const fn new() -> Self {
        Self {
            capture: PrintCaptureOptions::new(),
            fit_to_media: true,
        }
    }

    /// Replace the nested print-capture options.
    #[must_use]
    pub const fn with_capture_options(mut self, capture: PrintCaptureOptions) -> Self {
        self.capture = capture;
        self
    }

    /// Choose whether rendered images larger than the media box are scaled
    /// down (`true`) or rejected with [`AssemblyError::ImageExceedsMedia`].
    #[must_use]
    pub const fn with_fit_to_media(mut self, fit_to_media: bool) -> Self {
        self.fit_to_media = fit_to_media;
        self
    }

    /// Return the nested print-capture options.
    #[must_use]
    pub const fn capture_options(&self) -> PrintCaptureOptions {
        self.capture
    }

    /// Return whether oversized images are scaled down to fit.
    #[must_use]
    pub const fn fit_to_media(&self) -> bool {
        self.fit_to_media
    }
}

impl Default for AssemblyOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Deterministic placement of a rendered image inside a media box.
///
/// Coordinates are PDF points measured from the top-left of the page. The
/// image is centred and only ever scaled down (`scale <= 1.0`), so no caller
/// pixel buffer is enlarged as a side effect of the fit step.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct ImagePlacement {
    x: f64,
    y: f64,
    width: f64,
    height: f64,
    scale: f64,
}

impl ImagePlacement {
    /// Left edge of the image in PDF points.
    #[must_use]
    pub const fn x(&self) -> f64 {
        self.x
    }

    /// Top edge of the image in PDF points.
    #[must_use]
    pub const fn y(&self) -> f64 {
        self.y
    }

    /// Scaled image width in PDF points.
    #[must_use]
    pub const fn width(&self) -> f64 {
        self.width
    }

    /// Scaled image height in PDF points.
    #[must_use]
    pub const fn height(&self) -> f64 {
        self.height
    }

    /// Applied uniform scale factor, in `0.0..=1.0`.
    #[must_use]
    pub const fn scale(&self) -> f64 {
        self.scale
    }
}

/// One output page of a deterministic assembly plan.
#[derive(Debug, Clone, PartialEq)]
pub struct AssemblyPage {
    output_index: usize,
    source_index: usize,
    label: String,
    source_kind: &'static str,
    media: PageSize,
    placement: Option<ImagePlacement>,
}

impl AssemblyPage {
    /// Zero-based position of this page in the final, collated output.
    #[must_use]
    pub const fn output_index(&self) -> usize {
        self.output_index
    }

    /// Zero-based index of the requested source page.
    #[must_use]
    pub const fn source_index(&self) -> usize {
        self.source_index
    }

    /// Opaque diagnostic label inherited from the captured page.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Stable source-kind label (`rendered-image`, `postscript`, or `html`).
    #[must_use]
    pub const fn source_kind(&self) -> &'static str {
        self.source_kind
    }

    /// Validated media box applied to this output page.
    #[must_use]
    pub const fn media(&self) -> PageSize {
        self.media
    }

    /// Image layout for rendered-image pages; `None` for flowable payloads.
    #[must_use]
    pub const fn placement(&self) -> Option<ImagePlacement> {
        self.placement
    }
}

/// A deterministic, host-consumable PDF assembly plan.
#[derive(Debug, Clone, PartialEq)]
pub struct PdfAssemblyPlan {
    job_id: String,
    source_app: String,
    media: PageSize,
    orientation: Orientation,
    copies: u16,
    pages: Vec<AssemblyPage>,
    summary: String,
}

impl PdfAssemblyPlan {
    /// Job identifier inherited from the validated envelope.
    #[must_use]
    pub fn job_id(&self) -> &str {
        &self.job_id
    }

    /// Source application inherited from the validated envelope.
    #[must_use]
    pub fn source_app(&self) -> &str {
        &self.source_app
    }

    /// Orientation-applied media box shared by every output page.
    #[must_use]
    pub const fn media(&self) -> PageSize {
        self.media
    }

    /// Requested orientation.
    #[must_use]
    pub const fn orientation(&self) -> Orientation {
        self.orientation
    }

    /// Requested copy count.
    #[must_use]
    pub const fn copies(&self) -> u16 {
        self.copies
    }

    /// Final, collated output pages.
    #[must_use]
    pub fn pages(&self) -> &[AssemblyPage] {
        &self.pages
    }

    /// Number of output pages, including copies.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }

    /// Stable, byte-for-byte deterministic summary of the plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A typed failure while assembling a print job and its captured pages.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum AssemblyError {
    /// The virtual-printer envelope failed validation.
    #[error("invalid print job envelope: {0}")]
    Job(#[from] PrintJobError),
    /// One or more captured pages failed validation.
    #[error("invalid print capture input: {0}")]
    Capture(#[from] PrintCaptureError),
    /// The job requested a source page with no matching captured page.
    #[error("no captured page supplies source index {source_index}")]
    MissingCapturePage {
        /// The missing zero-based source page index.
        source_index: usize,
    },
    /// A captured page sequence was not requested by the job.
    #[error("captured page sequence {sequence} is not requested by the job")]
    UnexpectedCapturePage {
        /// The unexpected captured sequence.
        sequence: usize,
    },
    /// An image could not be placed inside the resolved media box.
    #[error("image on source page {source_index} does not fit the media box")]
    ImageExceedsMedia {
        /// The offending zero-based source page index.
        source_index: usize,
    },
    /// Collated output exceeded the documented page budget.
    #[error("assembly has {count} output pages (limit {limit})")]
    TooManyOutputPages {
        /// Number of output pages produced.
        count: usize,
        /// Enforced output page budget.
        limit: usize,
    },
}

/// Place one validated rendered-image geometry inside the media box.
fn placement_for(
    source_index: usize,
    natural: PageSize,
    media: PageSize,
    fit_to_media: bool,
) -> Result<ImagePlacement, AssemblyError> {
    let fit_scale = (media.width / natural.width).min(media.height / natural.height);
    let scale = if fit_to_media {
        fit_scale.min(1.0)
    } else {
        1.0
    };
    let width = natural.width * scale;
    let height = natural.height * scale;
    // A tiny tolerance absorbs IEEE-754 rounding at the exact fit boundary.
    if width > media.width + 1e-9 || height > media.height + 1e-9 {
        return Err(AssemblyError::ImageExceedsMedia { source_index });
    }
    Ok(ImagePlacement {
        x: (media.width - width) / 2.0,
        y: (media.height - height) / 2.0,
        width,
        height,
        scale,
    })
}

/// Build a deterministic PDF assembly plan from a validated print-job envelope
/// and the caller-supplied captured pages it references.
///
/// The job plan is resolved first (which validates the envelope), then every
/// captured page is validated through [`build_print_capture_plan`]. The set of
/// captured sequences must exactly match the set of source page indices
/// requested by the job; extras and omissions are typed errors. Output pages
/// are emitted in collated copy order (all requested pages, repeated once per
/// copy), each with an explicit media box and, for rendered images, a centred
/// fit-to-media placement.
///
/// # Errors
///
/// Returns [`AssemblyError`] for an invalid envelope, an invalid captured page,
/// a missing or unexpected sequence, an image that does not fit when fitting is
/// disabled, or an over-budget output page count.
pub fn build_pdf_assembly_plan(
    job: &PrintJob,
    pages: &[CapturePage],
    options: &AssemblyOptions,
) -> Result<PdfAssemblyPlan, AssemblyError> {
    let job_plan: PrintJobPlan = job.resolve_plan()?;
    let capture_plan = build_print_capture_plan(pages, &options.capture)?;

    let requested: HashSet<usize> = job_plan.pages().iter().copied().collect();
    let mut by_sequence: HashMap<usize, &PlannedCapturePage> =
        HashMap::with_capacity(capture_plan.pages().len());
    for page in capture_plan.pages() {
        if !requested.contains(&page.sequence()) {
            return Err(AssemblyError::UnexpectedCapturePage {
                sequence: page.sequence(),
            });
        }
        by_sequence.insert(page.sequence(), page);
    }

    let mut base: Vec<AssemblyPage> = Vec::with_capacity(job_plan.page_count());
    for (position, source_index) in job_plan.pages().iter().enumerate() {
        let captured = by_sequence
            .get(source_index)
            .ok_or(AssemblyError::MissingCapturePage {
                source_index: *source_index,
            })?;
        let placement = if captured.source_kind() == RENDERED_IMAGE_KIND {
            Some(placement_for(
                *source_index,
                captured.page_size(),
                job_plan.media(),
                options.fit_to_media(),
            )?)
        } else {
            None
        };
        base.push(AssemblyPage {
            output_index: position,
            source_index: *source_index,
            label: captured.label().to_owned(),
            source_kind: captured.source_kind(),
            media: job_plan.media(),
            placement,
        });
    }

    let copies = usize::from(job_plan.copies());
    let total = base.len().saturating_mul(copies);
    if total > ASSEMBLY_MAX_OUTPUT_PAGES {
        return Err(AssemblyError::TooManyOutputPages {
            count: total,
            limit: ASSEMBLY_MAX_OUTPUT_PAGES,
        });
    }

    let mut output: Vec<AssemblyPage> = Vec::with_capacity(total);
    for _ in 0..copies {
        for page in &base {
            let mut planned = page.clone();
            planned.output_index = output.len();
            output.push(planned);
        }
    }

    let fragments: Vec<String> = output
        .iter()
        .map(|page| match page.placement() {
            Some(place) => format!(
                "{}:{}:{}:{:.3},{:.3},{:.3},{:.3}",
                page.output_index(),
                page.source_index(),
                page.source_kind(),
                place.x(),
                place.y(),
                place.width(),
                place.height()
            ),
            None => format!(
                "{}:{}:{}:flow",
                page.output_index(),
                page.source_index(),
                page.source_kind()
            ),
        })
        .collect();
    let summary = format!(
        "job={} app={} media={:.3}x{:.3} orientation={:?} copies={} pages=[{}]",
        job.job_id(),
        job.source_app(),
        job_plan.media().width,
        job_plan.media().height,
        job_plan.orientation(),
        job_plan.copies(),
        fragments.join(", ")
    );

    Ok(PdfAssemblyPlan {
        job_id: job.job_id().to_owned(),
        source_app: job.source_app().to_owned(),
        media: job_plan.media(),
        orientation: job_plan.orientation(),
        copies: job_plan.copies(),
        pages: output,
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::print_capture::CaptureSource;
    use crate::print_job::{Orientation, PageRange, PaperSize, PrintJob};

    fn job() -> PrintJob {
        PrintJob::new(
            "job-1",
            "test-app",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            2,
            PageRange::all(),
        )
    }

    fn capture(sequence: usize, source: CaptureSource) -> CapturePage {
        CapturePage::new(sequence, format!("page-{sequence}"), source)
    }

    fn html(sequence: usize) -> CapturePage {
        capture(
            sequence,
            CaptureSource::Html {
                markup: "<p>hello</p>".to_owned(),
            },
        )
    }

    fn image(sequence: usize, width_px: u32, height_px: u32) -> CapturePage {
        capture(
            sequence,
            CaptureSource::RenderedImage {
                width_px,
                height_px,
                dpi: 72.0,
            },
        )
    }

    #[test]
    fn assembly_plan_joins_job_and_capture_pages_in_resolved_order() {
        let pages = vec![html(1), html(0)];
        let selected = PrintJob::new(
            "job-1",
            "test-app",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            2,
            PageRange::spec("2,1"),
        );
        let plan = build_pdf_assembly_plan(&selected, &pages, &AssemblyOptions::new());
        assert!(plan.is_ok());
        let plan = plan.expect("assembly plan");
        assert_eq!(plan.page_count(), 2);
        // `parse_page_range` deduplicates into ascending one-based order and
        // `resolve_plan` converts to zero-based, so "2,1" resolves to [0, 1].
        assert_eq!(plan.pages()[0].source_index(), 0);
        assert_eq!(plan.pages()[1].source_index(), 1);
        assert_eq!(plan.job_id(), "job-1");
        assert_eq!(plan.source_app(), "test-app");
    }

    #[test]
    fn assembly_plan_applies_copies_in_collated_output_order() {
        let selected = PrintJob::new(
            "job-1",
            "test-app",
            PaperSize::Letter,
            Orientation::Portrait,
            3,
            2,
            PageRange::all(),
        );
        let plan = build_pdf_assembly_plan(&selected, &[html(0), html(1)], &AssemblyOptions::new());
        assert!(plan.is_ok());
        let plan = plan.expect("assembly plan");
        let sources: Vec<usize> = plan
            .pages()
            .iter()
            .map(AssemblyPage::source_index)
            .collect();
        assert_eq!(sources, vec![0, 1, 0, 1, 0, 1]);
        assert_eq!(plan.copies(), 3);
        assert_eq!(plan.page_count(), 6);
    }

    #[test]
    fn assembly_plan_fits_oversized_rendered_image_to_media() {
        // 1224 x 1584 px at 72 dpi = 1224 x 1584 pt, twice Letter.
        let plan = build_pdf_assembly_plan(
            &job(),
            &[image(0, 1224, 792), image(1, 612, 1584)],
            &AssemblyOptions::new(),
        );
        assert!(plan.is_ok());
        let plan = plan.expect("assembly plan");
        for page in plan.pages() {
            let place = page.placement().expect("rendered-image placement");
            assert!(place.width() <= plan.media().width + 1e-9);
            assert!(place.height() <= plan.media().height + 1e-9);
            assert!(place.scale() <= 1.0);
            assert!(place.x() >= 0.0 && place.y() >= 0.0);
        }
    }

    #[test]
    fn assembly_plan_rejects_image_that_exceeds_media_without_fit() {
        let options = AssemblyOptions::new().with_fit_to_media(false);
        let result = build_pdf_assembly_plan(&job(), &[image(0, 1224, 792), html(1)], &options);
        assert_eq!(
            result,
            Err(AssemblyError::ImageExceedsMedia { source_index: 0 })
        );
    }

    #[test]
    fn missing_and_unexpected_capture_pages_are_typed_errors() {
        let missing = build_pdf_assembly_plan(&job(), &[html(0)], &AssemblyOptions::new());
        assert_eq!(
            missing,
            Err(AssemblyError::MissingCapturePage { source_index: 1 })
        );
        let unexpected = build_pdf_assembly_plan(
            &job(),
            &[html(0), html(1), html(9)],
            &AssemblyOptions::new(),
        );
        assert_eq!(
            unexpected,
            Err(AssemblyError::UnexpectedCapturePage { sequence: 9 })
        );
    }

    #[test]
    fn invalid_job_envelope_and_capture_payload_propagate_typed_errors() {
        let empty_job = PrintJob::new(
            "",
            "test-app",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            1,
            PageRange::all(),
        );
        assert_eq!(
            build_pdf_assembly_plan(&empty_job, &[html(0)], &AssemblyOptions::new()),
            Err(AssemblyError::Job(PrintJobError::EmptyJobId))
        );
        let bad_payload = capture(
            0,
            CaptureSource::Html {
                markup: String::new(),
            },
        );
        let single_page = PrintJob::new(
            "job-1",
            "test-app",
            PaperSize::Letter,
            Orientation::Portrait,
            1,
            1,
            PageRange::all(),
        );
        let result = build_pdf_assembly_plan(&single_page, &[bad_payload], &AssemblyOptions::new());
        assert_eq!(
            result,
            Err(AssemblyError::Capture(PrintCaptureError::EmptyPayload {
                sequence: 0,
                kind: "html",
            }))
        );
    }

    #[test]
    fn assembly_summary_is_byte_for_byte_deterministic() {
        let pages = vec![image(1, 300, 200), html(0)];
        let first = build_pdf_assembly_plan(&job(), &pages, &AssemblyOptions::new());
        let second = build_pdf_assembly_plan(&job(), &pages, &AssemblyOptions::new());
        assert_eq!(first, second);
        let first = first.expect("first plan");
        let second = second.expect("second plan");
        assert_eq!(first.summary(), second.summary());
        assert!(first.summary().contains("job=job-1"));
        assert!(first.summary().contains("app=test-app"));
    }
}
