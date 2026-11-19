//! Page-image export on top of the [`pdfxi_render::Renderer`] contract.
//!
//! This module is the renderer-facing half of image export. It is compiled only
//! when the `renderer` feature is enabled (on by default), so callers that just
//! need the pure encoders or format classification can disable default
//! features and stay dependency-free. The API is deliberately agnostic of any
//! particular backend: [`export_pages`] accepts any `&dyn Renderer`, and only
//! the `pdfium`-gated convenience functions construct a concrete
//! [`PdfiumRenderer`](pdfxi_render::PdfiumRenderer).
//!
//! # The plan
//!
//! [`ImageExportPlan`] collects everything needed for a batch:
//!
//! - **Page selection** — [`PageSelection::All`] exports every page the
//!   renderer reports, [`PageSelection::Range`] takes a one-based inclusive
//!   range, and [`PageSelection::Pages`] takes an explicit one-based list in
//!   the caller's order.
//! - **Resolution** — [`RenderResolution::Scale`] is a direct renderer scale
//!   (`1.0` is 72 DPI); [`RenderResolution::Dpi`] is converted with
//!   `scale = dpi / 72.0`. Both are validated before any work starts.
//! - **Format** — [`ImageFormat::Png`] or [`ImageFormat::Ppm`].
//! - **Output naming** — a pattern where `{page}` expands to the one-based
//!   page number and `{index}` to the zero-based sequence position, e.g.
//!   `"page-{page}"` becomes `page-1.png`.
//!
//! [`export_pages`] performs no file-system I/O: it returns in-memory
//! [`ExportedImage`] values. Callers combine each image's bytes with
//! [`ImageExportPlan::output_name`] and the format's extension when writing
//! files. Cancellation is cooperative: the supplied token is checked before
//! the first page and between pages, and is passed through to the renderer.

use crate::ConversionError;
use crate::image::{ImageFormat, encode};
use pdfxi_render::{CancellationToken, PageIndex, RenderError, Renderer};
#[cfg(feature = "pdfium")]
use std::path::Path;

/// Selects which pages an [`ImageExportPlan`] exports.
///
/// All variants use **one-based** page numbers in user-facing input; the
/// resolved indices handed to the renderer are zero-based.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageSelection {
    /// Export every page, in order, as reported by `Renderer::page_count`.
    All,
    /// Export the inclusive one-based range `first..=last`.
    Range {
        /// First one-based page to export (must be at least 1).
        first: usize,
        /// Last one-based page to export (must be at least `first`).
        last: usize,
    },
    /// Export an explicit one-based page list in the caller's order.
    ///
    /// Duplicates are preserved; an empty list is rejected.
    Pages(Vec<usize>),
}

impl PageSelection {
    /// Resolve this selection into zero-based renderer indices.
    ///
    /// `page_count` is the number of pages the renderer reports. The result is
    /// never empty on success; an empty selection is an
    /// [`ConversionError::InvalidInput`].
    fn resolve(&self, page_count: usize) -> Result<Vec<usize>, ConversionError> {
        match self {
            Self::All => {
                if page_count == 0 {
                    return Err(ConversionError::InvalidInput {
                        message: "page selection is empty: the document has no pages".to_owned(),
                    });
                }
                Ok((0..page_count).collect())
            }
            Self::Range { first, last } => {
                if *first == 0 || *last < *first {
                    return Err(ConversionError::InvalidInput {
                        message: format!("invalid one-based page range {first}..={last}"),
                    });
                }
                if *last > page_count {
                    return Err(ConversionError::InvalidInput {
                        message: format!(
                            "page range ends at page {last}, but the document has {page_count}"
                        ),
                    });
                }
                Ok((first - 1..=last - 1).collect())
            }
            Self::Pages(pages) => {
                if pages.is_empty() {
                    return Err(ConversionError::InvalidInput {
                        message: "page selection is empty".to_owned(),
                    });
                }
                let mut indices = Vec::with_capacity(pages.len());
                for &one_based in pages {
                    if one_based == 0 || one_based > page_count {
                        return Err(ConversionError::InvalidInput {
                            message: format!(
                                "page {one_based} is outside a {page_count}-page document"
                            ),
                        });
                    }
                    indices.push(one_based - 1);
                }
                Ok(indices)
            }
        }
    }

    /// Check the structural validity of this selection without a page count.
    fn validate(&self) -> Result<(), ConversionError> {
        match self {
            Self::All => Ok(()),
            Self::Range { first, last } if *first == 0 || *last < *first => {
                Err(ConversionError::InvalidInput {
                    message: format!("invalid one-based page range {first}..={last}"),
                })
            }
            Self::Range { .. } => Ok(()),
            Self::Pages(pages) if pages.is_empty() => Err(ConversionError::InvalidInput {
                message: "page selection is empty".to_owned(),
            }),
            Self::Pages(_) => Ok(()),
        }
    }
}

/// Raster resolution requested by an [`ImageExportPlan`].
///
/// Renderer scales and physical DPI are two views of the same value: a scale
/// of `1.0` maps one PDF point to one pixel and therefore equals 72 DPI.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RenderResolution {
    /// A direct renderer scale; `1.0` is 72 DPI.
    Scale(f32),
    /// A physical resolution in dots per inch, converted to a scale.
    Dpi(u32),
}

impl RenderResolution {
    /// Convert this resolution into the scale passed to `Renderer::render_page`.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] if the scale is not finite and
    /// positive, or the DPI value is zero.
    pub fn effective_scale(self) -> Result<f32, ConversionError> {
        let scale = match self {
            Self::Scale(scale) => scale,
            Self::Dpi(dpi) => {
                if dpi == 0 {
                    return Err(ConversionError::InvalidInput {
                        message: "DPI must be greater than zero".to_owned(),
                    });
                }
                f64::from(dpi) as f32 / 72.0
            }
        };
        if !scale.is_finite() || scale <= 0.0 {
            return Err(ConversionError::InvalidInput {
                message: format!("render scale must be finite and positive, got {scale}"),
            });
        }
        Ok(scale)
    }
}

impl Default for RenderResolution {
    fn default() -> Self {
        Self::Scale(1.0)
    }
}

/// Everything needed to export a set of pages as raster images.
///
/// Build a plan with [`ImageExportPlan::new`] and the `with_*` methods, then
/// hand it to [`export_pages`]. Defaults are `PageSelection::All`, a scale of
/// `1.0` (72 DPI), [`ImageFormat::Png`], and the name pattern `page-{page}`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImageExportPlan {
    pages: PageSelection,
    resolution: RenderResolution,
    format: ImageFormat,
    name_pattern: String,
}

impl ImageExportPlan {
    /// Default output-name pattern; `{page}` is the one-based page number.
    pub const DEFAULT_NAME_PATTERN: &'static str = "page-{page}";

    /// Create a plan for `pages` with conservative defaults.
    #[must_use]
    pub fn new(pages: PageSelection) -> Self {
        Self {
            pages,
            resolution: RenderResolution::default(),
            format: ImageFormat::Png,
            name_pattern: Self::DEFAULT_NAME_PATTERN.to_owned(),
        }
    }

    /// Replace the page selection.
    #[must_use]
    pub fn with_pages(mut self, pages: PageSelection) -> Self {
        self.pages = pages;
        self
    }

    /// Request a direct renderer scale.
    #[must_use]
    pub fn with_scale(mut self, scale: f32) -> Self {
        self.resolution = RenderResolution::Scale(scale);
        self
    }

    /// Request a physical resolution in dots per inch.
    #[must_use]
    pub fn with_dpi(mut self, dpi: u32) -> Self {
        self.resolution = RenderResolution::Dpi(dpi);
        self
    }

    /// Replace the output image format.
    #[must_use]
    pub fn with_format(mut self, format: ImageFormat) -> Self {
        self.format = format;
        self
    }

    /// Replace the output-name pattern.
    ///
    /// `{page}` expands to the one-based page number and `{index}` to the
    /// zero-based position in the export batch.
    #[must_use]
    pub fn with_name_pattern(mut self, pattern: impl Into<String>) -> Self {
        self.name_pattern = pattern.into();
        self
    }

    /// Return the configured page selection.
    #[must_use]
    pub fn pages(&self) -> &PageSelection {
        &self.pages
    }

    /// Return the configured resolution.
    #[must_use]
    pub fn resolution(&self) -> RenderResolution {
        self.resolution
    }

    /// Return the configured output format.
    #[must_use]
    pub const fn format(&self) -> ImageFormat {
        self.format
    }

    /// Return the configured output-name pattern.
    #[must_use]
    pub fn name_pattern(&self) -> &str {
        &self.name_pattern
    }

    /// Return the renderer scale this plan resolves to.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] for an invalid scale or DPI.
    pub fn effective_scale(&self) -> Result<f32, ConversionError> {
        self.resolution.effective_scale()
    }

    /// Compute a base output name for a zero-based page index.
    ///
    /// `{page}` becomes `page + 1` and `{index}` becomes `page`. The caller
    /// appends the extension from [`ImageFormat::extension`] when writing a
    /// file.
    #[must_use]
    pub fn output_name(&self, page: usize) -> String {
        self.name_pattern
            .replace("{page}", &page.saturating_add(1).to_string())
            .replace("{index}", &page.to_string())
    }

    /// Validate plan values that do not depend on the renderer's page count.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] for an empty name pattern, an
    /// invalid resolution, or a structurally invalid page selection.
    pub fn validate(&self) -> Result<(), ConversionError> {
        if self.name_pattern.trim().is_empty() {
            return Err(ConversionError::InvalidInput {
                message: "output-name pattern must not be empty".to_owned(),
            });
        }
        self.resolution.effective_scale()?;
        self.pages.validate()
    }
}

/// One exported raster image.
///
/// `page` is the zero-based renderer page index; `bytes` is a complete file in
/// `format` (not raw pixels), ready to be written to disk with the name from
/// [`ImageExportPlan::output_name`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportedImage {
    /// Zero-based page index this image depicts.
    pub page: usize,
    /// Raster width in pixels.
    pub width: u32,
    /// Raster height in pixels.
    pub height: u32,
    /// Encoded file bytes (PNG or PPM), never raw pixels.
    pub bytes: Vec<u8>,
    /// Encoding format of `bytes`.
    pub format: ImageFormat,
}

impl ExportedImage {
    /// Return the one-based page number for user-facing messages.
    #[must_use]
    pub const fn page_number(&self) -> usize {
        self.page + 1
    }

    /// Return the number of encoded bytes.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return `true` when encoding produced no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// Map a renderer failure to a [`ConversionError`], preserving cancellation.
fn render_error(error: RenderError) -> ConversionError {
    match error {
        RenderError::Cancelled => ConversionError::Cancelled,
        other => ConversionError::InvalidInput {
            message: format!("render failed: {other}"),
        },
    }
}

/// Render and encode every page selected by `plan`.
///
/// Pages are exported in selection order. The token is checked before any
/// renderer call and again before each page, then handed to
/// [`Renderer::render_page`] so cancellation during a render is honored too.
/// No files are written; callers use [`ImageExportPlan::output_name`] to name
/// the returned bytes.
///
/// # Errors
///
/// Returns [`ConversionError::Cancelled`] when the token fires,
/// [`ConversionError::InvalidInput`] for an invalid plan, empty selection,
/// malformed renderer output, or a renderer failure, in the order those checks
/// occur.
pub fn export_pages(
    renderer: &dyn Renderer,
    plan: &ImageExportPlan,
    cancellation: &CancellationToken,
) -> Result<Vec<ExportedImage>, ConversionError> {
    plan.validate()?;
    cancellation
        .check()
        .map_err(|_| ConversionError::Cancelled)?;

    let page_count = renderer.page_count().map_err(render_error)?;
    let pages = plan.pages().resolve(page_count)?;
    let scale = plan.effective_scale()?;
    let format = plan.format();

    let mut exported = Vec::with_capacity(pages.len());
    for page in pages {
        cancellation
            .check()
            .map_err(|_| ConversionError::Cancelled)?;
        let rendered = renderer
            .render_page(PageIndex::new(page), scale, cancellation, &mut |_| {})
            .map_err(render_error)?;
        let (width, height) = (rendered.width(), rendered.height());
        if width == 0 || height == 0 {
            return Err(ConversionError::InvalidInput {
                message: format!("renderer returned a zero-sized raster for page {page}"),
            });
        }
        let expected = (width as usize)
            .checked_mul(height as usize)
            .and_then(|area| area.checked_mul(4))
            .ok_or_else(|| ConversionError::InvalidInput {
                message: format!("raster size overflow for page {page}"),
            })?;
        if rendered.pixels().len() != expected {
            return Err(ConversionError::InvalidInput {
                message: format!(
                    "renderer returned {} pixel bytes for page {page}, expected {expected}",
                    rendered.pixels().len()
                ),
            });
        }
        let bytes = encode(width, height, rendered.pixels(), format)?;
        exported.push(ExportedImage {
            page,
            width,
            height,
            bytes,
            format,
        });
    }
    Ok(exported)
}

/// Export pages from in-memory PDF bytes using a freshly constructed
/// [`PdfiumRenderer`](pdfxi_render::PdfiumRenderer).
///
/// This convenience path is available only with the `pdfium` feature; the core
/// [`export_pages`] API stays renderer-agnostic. The native library is loaded
/// by `PdfiumRenderer::from_bytes`, so a missing PDFium install surfaces as a
/// [`ConversionError::InvalidInput`] with the backend's message.
///
/// # Errors
///
/// Propagates PDFium construction and export failures as described by
/// [`export_pages`].
#[cfg(feature = "pdfium")]
pub fn export_pdf_bytes(
    pdf_bytes: Vec<u8>,
    plan: &ImageExportPlan,
    cancellation: &CancellationToken,
) -> Result<Vec<ExportedImage>, ConversionError> {
    let renderer = pdfxi_render::PdfiumRenderer::from_bytes(pdf_bytes).map_err(render_error)?;
    export_pages(&renderer, plan, cancellation)
}

/// Export pages from a PDF file using a freshly constructed
/// [`PdfiumRenderer`](pdfxi_render::PdfiumRenderer).
///
/// File reading and native-library loading are performed by
/// `PdfiumRenderer::from_file`; this function only selects pages and encodes
/// the rendered rasters. See [`export_pdf_bytes`] for the in-memory variant.
///
/// # Errors
///
/// Propagates file-reading, PDFium construction, and export failures as
/// [`ConversionError::InvalidInput`], or [`ConversionError::Cancelled`].
#[cfg(feature = "pdfium")]
pub fn export_pdf_file(
    path: impl AsRef<Path>,
    plan: &ImageExportPlan,
    cancellation: &CancellationToken,
) -> Result<Vec<ExportedImage>, ConversionError> {
    let renderer = pdfxi_render::PdfiumRenderer::from_file(path).map_err(render_error)?;
    export_pages(&renderer, plan, cancellation)
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_render::{Progress, RenderedPage};
    use std::sync::{
        Arc,
        atomic::{AtomicUsize, Ordering},
    };

    /// A deterministic renderer that records render calls and can cancel a
    /// shared token after a chosen number of renders.
    struct MockRenderer {
        page_count: usize,
        side: u32,
        renders: Arc<AtomicUsize>,
        cancel_after: Option<(CancellationToken, usize)>,
        broken_buffer: bool,
    }

    impl MockRenderer {
        fn new(page_count: usize) -> Self {
            Self {
                page_count,
                side: 2,
                renders: Arc::new(AtomicUsize::new(0)),
                cancel_after: None,
                broken_buffer: false,
            }
        }

        fn cancel_after(mut self, token: CancellationToken, count: usize) -> Self {
            self.cancel_after = Some((token, count));
            self
        }

        fn with_broken_buffer(mut self) -> Self {
            self.broken_buffer = true;
            self
        }

        fn render_count(&self) -> usize {
            self.renders.load(Ordering::SeqCst)
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
            self.renders.fetch_add(1, Ordering::SeqCst);
            progress(Progress::new(0, 1));
            cancellation.check()?;

            // A broken renderer reports `side` dimensions but allocates one
            // extra row so the pixel buffer does not match the declared size.
            let allocated_side = if self.broken_buffer {
                self.side + 1
            } else {
                self.side
            };
            let pixels = vec![0x40u8; allocated_side as usize * allocated_side as usize * 4];
            progress(Progress::new(1, 1));

            if let Some((token, count)) = &self.cancel_after
                && self.render_count() >= *count
            {
                token.cancel();
            }
            cancellation.check()?;
            Ok(RenderedPage::new(page, self.side, self.side, pixels))
        }

        fn page_count(&self) -> Result<usize, RenderError> {
            Ok(self.page_count)
        }
    }

    fn export(
        renderer: &dyn Renderer,
        plan: &ImageExportPlan,
        token: &CancellationToken,
    ) -> Result<Vec<ExportedImage>, ConversionError> {
        export_pages(renderer, plan, token)
    }

    #[test]
    fn multi_page_export_preserves_selection_order() {
        let renderer = MockRenderer::new(3);
        let plan = ImageExportPlan::new(PageSelection::All);
        let images = export(&renderer, &plan, &CancellationToken::new()).expect("export all pages");

        assert_eq!(images.len(), 3);
        assert_eq!(
            images.iter().map(|image| image.page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(renderer.render_count(), 3);
        for image in &images {
            assert_eq!((image.width, image.height), (2, 2));
            assert_eq!(image.format, ImageFormat::Png);
            assert_eq!(
                &image.bytes[..8],
                &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
            );
            assert_eq!(image.page_number(), image.page + 1);
            assert!(!image.is_empty());
        }
    }

    #[test]
    fn one_based_range_and_format_selection_are_honoured() {
        let renderer = MockRenderer::new(5);
        let plan = ImageExportPlan::new(PageSelection::Range { first: 2, last: 4 })
            .with_format(ImageFormat::Ppm)
            .with_name_pattern("scan-{page}");
        let images = export(&renderer, &plan, &CancellationToken::new()).expect("range export");

        assert_eq!(
            images.iter().map(|image| image.page).collect::<Vec<_>>(),
            vec![1, 2, 3]
        );
        assert!(images.iter().all(|image| image.format == ImageFormat::Ppm));
        assert_eq!(&images[0].bytes[..11], b"P6\n2 2\n255\n");
        assert_eq!(plan.output_name(1), "scan-2");
    }

    #[test]
    fn empty_page_selection_is_rejected() {
        let renderer = MockRenderer::new(0);
        let empty_list = ImageExportPlan::new(PageSelection::Pages(Vec::new()));
        assert!(matches!(
            export(&renderer, &empty_list, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));

        let all_on_empty = ImageExportPlan::new(PageSelection::All);
        assert!(matches!(
            export(&renderer, &all_on_empty, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert_eq!(renderer.render_count(), 0);

        let reversed = ImageExportPlan::new(PageSelection::Range { first: 4, last: 2 });
        assert!(matches!(
            export(&renderer, &reversed, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn invalid_scale_and_dpi_are_rejected_before_work() {
        let renderer = MockRenderer::new(3);
        for scale in [0.0, -1.0, f32::INFINITY, f32::NEG_INFINITY, f32::NAN] {
            let plan = ImageExportPlan::new(PageSelection::All).with_scale(scale);
            assert!(matches!(
                export(&renderer, &plan, &CancellationToken::new()),
                Err(ConversionError::InvalidInput { .. })
            ));
        }
        let zero_dpi = ImageExportPlan::new(PageSelection::All).with_dpi(0);
        assert!(matches!(
            export(&renderer, &zero_dpi, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert_eq!(renderer.render_count(), 0);
    }

    #[test]
    fn dpi_is_converted_to_a_renderer_scale() {
        let plan = ImageExportPlan::new(PageSelection::All).with_dpi(144);
        let scale = plan.effective_scale().expect("144 DPI is valid");
        assert!((scale - 2.0).abs() < f32::EPSILON);
        assert_eq!(plan.resolution(), RenderResolution::Dpi(144));
    }

    #[test]
    fn out_of_range_and_empty_pattern_are_rejected() {
        let renderer = MockRenderer::new(3);
        let out_of_range = ImageExportPlan::new(PageSelection::Pages(vec![1, 9]));
        assert!(matches!(
            export(&renderer, &out_of_range, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));

        let empty_pattern = ImageExportPlan::new(PageSelection::All).with_name_pattern("   ");
        assert!(matches!(
            export(&renderer, &empty_pattern, &CancellationToken::new()),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert_eq!(renderer.render_count(), 0);
    }

    #[test]
    fn cancellation_before_work_prevents_all_renders() {
        let renderer = MockRenderer::new(3);
        let token = CancellationToken::new();
        token.cancel();
        let plan = ImageExportPlan::new(PageSelection::All);
        assert!(matches!(
            export(&renderer, &plan, &token),
            Err(ConversionError::Cancelled)
        ));
        assert_eq!(renderer.render_count(), 0);
    }

    #[test]
    fn cancellation_between_pages_stops_after_the_first_page() {
        let token = CancellationToken::new();
        let renderer = MockRenderer::new(4).cancel_after(token.clone(), 1);
        let plan = ImageExportPlan::new(PageSelection::All);
        assert!(matches!(
            export(&renderer, &plan, &token),
            Err(ConversionError::Cancelled)
        ));
        assert_eq!(
            renderer.render_count(),
            1,
            "the second page must not be rendered once cancellation fires"
        );
    }

    #[test]
    fn mismatched_renderer_buffer_is_rejected() {
        let renderer = MockRenderer::new(1).with_broken_buffer();
        let plan = ImageExportPlan::new(PageSelection::All);
        let error = export(&renderer, &plan, &CancellationToken::new())
            .expect_err("a mismatched pixel buffer must fail");
        assert!(matches!(error, ConversionError::InvalidInput { .. }));
        assert!(error.to_string().contains("expected 16"));
    }

    #[test]
    fn output_name_substitutes_page_and_index_placeholders() {
        let plan =
            ImageExportPlan::new(PageSelection::All).with_name_pattern("scan-{index}-p{page}");
        assert_eq!(plan.output_name(0), "scan-0-p1");
        assert_eq!(plan.output_name(2), "scan-2-p3");
        assert_eq!(plan.name_pattern(), "scan-{index}-p{page}");
        assert_eq!(
            ImageExportPlan::new(PageSelection::All).output_name(0),
            "page-1"
        );
    }

    #[test]
    fn render_error_maps_cancellation_and_backend_failures() {
        assert!(matches!(
            render_error(RenderError::Cancelled),
            ConversionError::Cancelled
        ));
        let error = render_error(RenderError::Backend("boom".to_owned()));
        assert!(matches!(error, ConversionError::InvalidInput { .. }));
        assert!(error.to_string().contains("boom"));
    }

    #[test]
    fn plan_builder_getters_round_trip() {
        let plan = ImageExportPlan::new(PageSelection::Pages(vec![2, 1]))
            .with_scale(1.5)
            .with_format(ImageFormat::Ppm)
            .with_name_pattern("x-{page}");
        assert_eq!(
            plan.pages(),
            &PageSelection::Pages(vec![2, 1]),
            "explicit page order is preserved"
        );
        assert_eq!(plan.resolution(), RenderResolution::Scale(1.5));
        assert_eq!(plan.format(), ImageFormat::Ppm);
        assert_eq!(plan.name_pattern(), "x-{page}");
        assert!(plan.validate().is_ok());
    }
}
