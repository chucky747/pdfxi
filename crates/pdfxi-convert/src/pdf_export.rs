//! End-to-end export of a parsed [`pdfxi_core::Document`].
//!
//! The older [`crate::export_page_text`], [`crate::export_rtf`], and
//! [`crate::office_export`] writers are pure functions over caller-supplied
//! models. This module is the
//! bridge that makes an export reachable from real PDF bytes: it extracts page
//! text through [`pdfxi_core::Document::page_text`] (including `/ToUnicode`
//! decoding), applies the shared text-plan rules, and maps pages into the
//! existing OOXML Word writer.
//!
//! # Fidelity limits
//!
//! - Text comes from `Document::page_text`, so paragraph boundaries are the
//!   line breaks that extractor emits (the `'` and `"` operators). The core
//!   extractor does not expose glyph coordinates; a page whose content uses
//!   only `Tj`/`TJ` becomes one paragraph. Position-based line reconstruction is
//!   a separate extraction feature and is not claimed here.
//! - DOCX, XLSX, PPTX, HTML, and RTF export write text only. Images, vector art,
//!   annotations, charts, and page backgrounds are not embedded and are not
//!   silently approximated.
//! - XLSX uses one worksheet per PDF page named `Page N`, with one extracted
//!   line per row in column A. Numeric cells, charts, and formulas are not
//!   produced from PDF text.
//! - PPTX uses one slide per PDF page with a generated `Page N` title shape and
//!   the extracted lines flowed into a body text box. The slide size follows the
//!   first source page (its aspect ratio), not a forced 16:9 deck.
//! - Image export renders through the optional `pdfium` feature. Without that
//!   feature [`ExportFormat::Png`], [`ExportFormat::Jpeg`] and
//!   [`ExportFormat::Tiff`] are not listed by [`ExportFormat::implemented`] and
//!   fail with [`PdfExportError::UnsupportedFormat`].

use crate::export_options::ExportOptionError;
use crate::office_export::xml;
use crate::office_export::{
    DocxExportError, DocxExporter, OfficeBlock, OfficeDocument, OfficeExport, OfficePage,
    ParagraphStyle, PptxExportError, PptxExporter, ReportedUnsupported, XlsxExportError,
    XlsxExporter,
};
use crate::rtf::{RtfOptions, RtfPage, export_rtf_string};
use crate::text::{TextExportPlan, TextPageSelection};
use pdfxi_core::Document;
#[cfg(feature = "renderer")]
use pdfxi_render::CancellationToken;
use std::collections::HashSet;

/// Cancellation handle accepted by the dispatch.
///
/// With the `renderer` feature this is the renderer's token. A
/// `--no-default-features` build has no renderer dependency, so the dispatch is
/// synchronous and the alias is a unit reference; that build also does not
/// implement image formats.
#[cfg(feature = "renderer")]
type ExportToken<'a> = &'a CancellationToken;
/// Cancellation placeholder for dependency-free builds.
#[cfg(not(feature = "renderer"))]
type ExportToken<'a> = &'a ();

/// Default title written into the DOCX core properties and every other format
/// that carries a document title.
pub const DEFAULT_DOCX_TITLE: &str = "PDFXI export";

/// Default resolution for page-image export.
pub const DEFAULT_EXPORT_DPI: u32 = 300;

/// Largest accepted resolution for page-image export.
///
/// The value matches `pdfxi_render::export::MAX_EXPORT_DPI` so a plan accepted
/// here cannot be rejected by the renderer for its DPI.
pub const MAX_EXPORT_DPI: u32 = 12_000;

/// Default JPEG quality for [`ExportFormat::Jpeg`].
pub const DEFAULT_JPEG_QUALITY: u8 = 85;

/// The separator inserted between exported pages.
///
/// The choice is explicit rather than a free-form string so the common styles
/// used by text editors and office tools are typed and testable. A custom
/// separator remains available through [`crate::TextExportPlan`] when the
/// low-level writer is used directly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum PdfPageSeparator {
    /// A single line feed (`\n`).
    #[default]
    LineFeed,
    /// Two line feeds, producing an empty line between pages.
    BlankLine,
    /// A single form feed (`\u{0C}`), the classic printer page break.
    FormFeed,
}

impl PdfPageSeparator {
    /// Every supported separator, in a stable order.
    pub const ALL: &'static [Self] = &[Self::LineFeed, Self::BlankLine, Self::FormFeed];

    /// Return every supported separator in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Return the literal string inserted between pages.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::LineFeed => "\n",
            Self::BlankLine => "\n\n",
            Self::FormFeed => "\u{0c}",
        }
    }

    /// Return a short stable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::LineFeed => "line-feed",
            Self::BlankLine => "blank-line",
            Self::FormFeed => "form-feed",
        }
    }
}

impl std::fmt::Display for PdfPageSeparator {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Options for [`export_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfTextOptions {
    /// String inserted between page bodies.
    pub separator: PdfPageSeparator,
    /// Prefix each page with `--- Page N ---` (one-based).
    pub page_headings: bool,
    /// Pages to export, using the shared zero-based selection rules.
    pub selection: TextPageSelection,
}

impl Default for PdfTextOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfTextOptions {
    /// Create options with the documented defaults: line feed, no headings,
    /// every page.
    #[must_use]
    pub fn new() -> Self {
        Self {
            separator: PdfPageSeparator::default(),
            page_headings: false,
            selection: TextPageSelection::default(),
        }
    }

    /// Replace the page separator.
    #[must_use]
    pub fn with_separator(mut self, separator: PdfPageSeparator) -> Self {
        self.separator = separator;
        self
    }

    /// Enable or disable one-based page headings.
    #[must_use]
    pub fn with_page_headings(mut self, include: bool) -> Self {
        self.page_headings = include;
        self
    }

    /// Replace the page selection.
    #[must_use]
    pub fn with_selection(mut self, selection: TextPageSelection) -> Self {
        self.selection = selection;
        self
    }
}

/// Options for [`export_docx`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfDocxOptions {
    /// Value written to the DOCX core-properties title.
    pub title: String,
}

impl Default for PdfDocxOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl PdfDocxOptions {
    /// Create options with [`DEFAULT_DOCX_TITLE`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            title: DEFAULT_DOCX_TITLE.to_owned(),
        }
    }

    /// Replace the document title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }
}

/// A target format accepted by [`export_pdf`].
///
/// The variants mirror acceptance item A-27: Word, Excel, PowerPoint, image,
/// text, HTML, and RTF. A variant existing does not by itself mean the current
/// build can write it; check [`ExportFormat::implemented`] or
/// [`ExportFormat::is_implemented`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExportFormat {
    /// Plain UTF-8 text (`.txt`).
    Text,
    /// WordprocessingML (`.docx`).
    Docx,
    /// SpreadsheetML (`.xlsx`).
    Xlsx,
    /// PresentationML (`.pptx`).
    Pptx,
    /// 8-bit RGBA PNG through the renderer.
    Png,
    /// Baseline JPEG through the renderer.
    Jpeg,
    /// Uncompressed, optionally multi-page TIFF through the renderer.
    Tiff,
    /// Self-contained semantic HTML (`.html`).
    Html,
    /// Rich Text Format (`.rtf`).
    Rtf,
}

impl ExportFormat {
    /// Every format A-27 names, in acceptance order.
    pub const ALL: &'static [Self] = &[
        Self::Text,
        Self::Docx,
        Self::Xlsx,
        Self::Pptx,
        Self::Png,
        Self::Jpeg,
        Self::Tiff,
        Self::Html,
        Self::Rtf,
    ];

    /// Return only the formats this build can actually write.
    ///
    /// Image formats require the `pdfium` feature; every text-shaped format is
    /// always available. A UI should call this before populating an export menu
    /// so it never offers a dead entry.
    #[must_use]
    pub const fn implemented() -> &'static [Self] {
        #[cfg(feature = "pdfium")]
        {
            &[
                Self::Text,
                Self::Docx,
                Self::Xlsx,
                Self::Pptx,
                Self::Png,
                Self::Jpeg,
                Self::Tiff,
                Self::Html,
                Self::Rtf,
            ]
        }
        #[cfg(not(feature = "pdfium"))]
        {
            &[
                Self::Text,
                Self::Docx,
                Self::Xlsx,
                Self::Pptx,
                Self::Html,
                Self::Rtf,
            ]
        }
    }

    /// Return true when this build can write `self`.
    #[must_use]
    pub const fn is_implemented(self) -> bool {
        match self {
            Self::Png | Self::Jpeg | Self::Tiff => cfg!(feature = "pdfium"),
            Self::Text | Self::Docx | Self::Xlsx | Self::Pptx | Self::Html | Self::Rtf => true,
        }
    }

    /// Return the canonical lower-case file extension.
    #[must_use]
    pub const fn extension(self) -> &'static str {
        match self {
            Self::Text => "txt",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Png => "png",
            Self::Jpeg => "jpg",
            Self::Tiff => "tiff",
            Self::Html => "html",
            Self::Rtf => "rtf",
        }
    }

    /// Return the IANA media type written for this format.
    #[must_use]
    pub const fn media_type(self) -> &'static str {
        match self {
            Self::Text => "text/plain; charset=utf-8",
            Self::Docx => "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
            Self::Xlsx => "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
            Self::Pptx => {
                "application/vnd.openxmlformats-officedocument.presentationml.presentation"
            }
            Self::Png => "image/png",
            Self::Jpeg => "image/jpeg",
            Self::Tiff => "image/tiff",
            Self::Html => "text/html; charset=utf-8",
            Self::Rtf => "application/rtf",
        }
    }

    /// Return a short stable label for menus and reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "Text",
            Self::Docx => "Word",
            Self::Xlsx => "Excel",
            Self::Pptx => "PowerPoint",
            Self::Png => "PNG image",
            Self::Jpeg => "JPEG image",
            Self::Tiff => "TIFF image",
            Self::Html => "HTML",
            Self::Rtf => "RTF",
        }
    }

    /// Return true for the raster formats.
    #[must_use]
    pub const fn is_image(self) -> bool {
        matches!(self, Self::Png | Self::Jpeg | Self::Tiff)
    }
}

impl std::fmt::Display for ExportFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Options shared by every [`export_pdf`] target.
///
/// Page selections use the same **zero-based** indices as
/// [`TextPageSelection`]. Out-of-range pages are rejected by the dispatch
/// instead of being silently skipped, because an Office package cannot contain
/// a missing page. Duplicate explicit pages are collapsed to their first
/// occurrence so the mapping stays one source page to one output page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExportOptions {
    /// Title used by DOCX core properties, XLSX/PPTX metadata, and the HTML
    /// heading. Text and RTF ignore it.
    pub title: String,
    /// Pages to export, zero-based.
    pub selection: TextPageSelection,
    /// Raster resolution for PNG, JPEG and TIFF output.
    pub dpi: u32,
    /// Baseline JPEG quality in `1..=100`; ignored by the other formats.
    pub jpeg_quality: u8,
}

impl Default for ExportOptions {
    fn default() -> Self {
        Self::new()
    }
}

impl ExportOptions {
    /// Create options with [`DEFAULT_DOCX_TITLE`], every page,
    /// [`DEFAULT_EXPORT_DPI`], and [`DEFAULT_JPEG_QUALITY`].
    #[must_use]
    pub fn new() -> Self {
        Self {
            title: DEFAULT_DOCX_TITLE.to_owned(),
            selection: TextPageSelection::default(),
            dpi: DEFAULT_EXPORT_DPI,
            jpeg_quality: DEFAULT_JPEG_QUALITY,
        }
    }

    /// Replace the document title.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = title.into();
        self
    }

    /// Replace the page selection.
    #[must_use]
    pub fn with_selection(mut self, selection: TextPageSelection) -> Self {
        self.selection = selection;
        self
    }

    /// Replace the raster resolution used by image formats.
    #[must_use]
    pub fn with_dpi(mut self, dpi: u32) -> Self {
        self.dpi = dpi;
        self
    }

    /// Replace the JPEG quality used by [`ExportFormat::Jpeg`].
    #[must_use]
    pub fn with_jpeg_quality(mut self, quality: u8) -> Self {
        self.jpeg_quality = quality;
        self
    }
}

/// One file inside an [`ExportedFile`].
///
/// Single-file formats produce exactly one part named `document`; PNG and JPEG
/// produce one part per source page named `page-N` (one-based). TIFF also
/// produces one `document` part containing every selected page in one IFD
/// chain. Callers append [`ExportFormat::extension`] when writing a part.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedPart {
    /// Filename stem without the extension.
    pub name: String,
    /// Complete encoded file bytes.
    pub bytes: Vec<u8>,
    /// Pixel dimensions for image parts.
    pub pixel_size: Option<(u32, u32)>,
}

impl ExportedPart {
    /// Return the encoded byte length.
    #[must_use]
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return true when the part has no bytes.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }
}

/// The validated result of [`export_pdf`].
///
/// Every part has already been produced by the format's writer and, for Office
/// formats, read back and validated by the adapter before this value is
/// returned.
#[derive(Debug, Clone, PartialEq)]
pub struct ExportedFile {
    /// Format that produced the parts.
    pub format: ExportFormat,
    /// Number of source pages represented by this export.
    pub page_count: usize,
    /// One or more encoded outputs.
    pub parts: Vec<ExportedPart>,
    /// Office adapters' typed unsupported-content findings. Always empty for
    /// text, HTML, RTF, and image exports.
    pub unsupported: Vec<ReportedUnsupported>,
}

impl ExportedFile {
    /// Return the canonical file extension for every part.
    #[must_use]
    pub const fn extension(&self) -> &'static str {
        self.format.extension()
    }

    /// Return the IANA media type of the parts.
    #[must_use]
    pub const fn media_type(&self) -> &'static str {
        self.format.media_type()
    }

    /// Return true when this export produces exactly one file.
    #[must_use]
    pub fn is_single_file(&self) -> bool {
        self.parts.len() == 1
    }

    /// Return the only part, or `None` for a multi-file export.
    #[must_use]
    pub fn single(&self) -> Option<&ExportedPart> {
        if self.parts.len() == 1 {
            self.parts.first()
        } else {
            None
        }
    }

    /// Return the byte length of every part summed.
    #[must_use]
    pub fn total_bytes(&self) -> usize {
        self.parts.iter().map(ExportedPart::len).sum()
    }
}

/// A typed failure while exporting a parsed document.
#[derive(Debug, thiserror::Error)]
pub enum PdfExportError {
    /// Reading pages or text from the source document failed.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// The DOCX adapter rejected the mapped model or archive.
    #[error(transparent)]
    Docx(#[from] DocxExportError),
    /// The XLSX adapter rejected the mapped model or archive.
    #[error(transparent)]
    Xlsx(#[from] XlsxExportError),
    /// The PPTX adapter rejected the mapped model or archive.
    #[error(transparent)]
    Pptx(#[from] PptxExportError),
    /// The RTF adapter rejected its options.
    #[error(transparent)]
    Rtf(#[from] ExportOptionError),
    /// The renderer could not open or rasterize the source.
    #[cfg(feature = "pdfium")]
    #[error(transparent)]
    Render(#[from] pdfxi_render::RenderError),
    /// The renderer-backed image encoder rejected the plan or a raster.
    #[cfg(feature = "pdfium")]
    #[error(transparent)]
    Image(#[from] pdfxi_render::ExportError),
    /// The document has no pages, so there is nothing to export.
    #[error("cannot export a PDF with no pages")]
    EmptyDocument,
    /// A page media box cannot be represented as positive `f32` geometry.
    #[error("page {page} has geometry that cannot be exported")]
    InvalidPageGeometry {
        /// Zero-based page index.
        page: usize,
    },
    /// The requested title is empty or whitespace only.
    #[error("the export title must not be empty")]
    EmptyTitle,
    /// The format exists in the API but this build cannot write it.
    #[error("the {format} export format is not implemented in this build")]
    UnsupportedFormat {
        /// Requested format.
        format: ExportFormat,
    },
    /// The page selection is empty.
    #[error("the page selection is empty")]
    EmptyPageSelection,
    /// A zero-based page range starts after it ends.
    #[error("invalid page range {first}..={last}")]
    InvalidPageRange {
        /// First selected zero-based page.
        first: usize,
        /// Last selected zero-based page.
        last: usize,
    },
    /// A selected page does not exist in the document.
    #[error("page {page} is outside a {page_count}-page document")]
    PageOutOfRange {
        /// Requested zero-based page.
        page: usize,
        /// Number of pages in the document.
        page_count: usize,
    },
    /// The requested image resolution is zero or above [`MAX_EXPORT_DPI`].
    #[error("DPI must be between 1 and {MAX_EXPORT_DPI}, got {0}")]
    InvalidDpi(u32),
    /// JPEG quality is outside `1..=100`.
    #[error("JPEG quality must be between 1 and 100, got {0}")]
    InvalidJpegQuality(u8),
}

/// Export every page's extracted text as one deterministic string.
///
/// Text is read through [`pdfxi_core::Document::page_text`]. Ordering,
/// duplicates, missing pages, separators, and page headings follow
/// [`crate::TextExportPlan`]'s documented rules. The result has no trailing separator
/// beyond what the selected pages themselves contain.
///
/// # Errors
///
/// Returns [`PdfExportError::Core`] when the page tree or a content stream
/// cannot be read or decoded.
pub fn export_text(
    document: &Document,
    options: &PdfTextOptions,
) -> Result<String, PdfExportError> {
    let page_count = document.page_count()?;
    let explicit: HashSet<usize> = match &options.selection {
        TextPageSelection::Pages(pages) => pages.iter().copied().collect(),
        TextPageSelection::All | TextPageSelection::Range { .. } => HashSet::new(),
    };
    let mut pages = Vec::with_capacity(page_count);
    for index in 0..page_count {
        if !selection_includes(&options.selection, &explicit, index, page_count) {
            continue;
        }
        pages.push((index, document.page_text(index)?));
    }

    let plan = TextExportPlan::new()
        .with_pages(options.selection.clone())
        .with_page_separator(options.separator.as_str())
        .with_page_headers(options.page_headings);
    Ok(crate::text::export_text(pages.into_iter(), &plan))
}

/// Map a parsed PDF into the shared [`OfficeDocument`] model.
///
/// Each page becomes an [`OfficePage`] carrying the source media-box size and
/// one normal paragraph per non-empty line of extracted text. Pages stay in
/// ascending document order and keep their zero-based index, which is what the
/// DOCX writer uses to place page breaks.
///
/// # Errors
///
/// Returns [`PdfExportError::Core`] for extraction failures,
/// [`PdfExportError::EmptyDocument`] for a PDF with no pages, and
/// [`PdfExportError::InvalidPageGeometry`] when a media box cannot be
/// represented as positive `f32` geometry. An empty title is rejected with
/// [`PdfExportError::EmptyTitle`].
pub fn build_office_document(
    document: &Document,
    title: &str,
) -> Result<OfficeDocument, PdfExportError> {
    if title.trim().is_empty() {
        return Err(PdfExportError::EmptyTitle);
    }
    let page_count = document.page_count()?;
    if page_count == 0 {
        return Err(PdfExportError::EmptyDocument);
    }
    let pages: Vec<usize> = (0..page_count).collect();
    build_office_document_for_pages(document, title, &pages)
}

/// Map only `pages` (zero-based, already validated) into the Office model.
fn build_office_document_for_pages(
    document: &Document,
    title: &str,
    pages: &[usize],
) -> Result<OfficeDocument, PdfExportError> {
    let mut office_pages = Vec::with_capacity(pages.len());
    for &index in pages {
        let page = document.page(index)?;
        let width = export_geometry(page.width(), index)?;
        let height = export_geometry(page.height(), index)?;
        let text = document.page_text(index)?;
        let blocks = text_lines(&text)
            .into_iter()
            .map(|line| OfficeBlock::paragraph(line.to_owned()))
            .collect();
        office_pages.push(OfficePage::new(index, width, height).with_blocks(blocks));
    }
    Ok(OfficeDocument::new(title, office_pages))
}

/// Export a parsed PDF as a WordprocessingML (`.docx`) archive.
///
/// The archive is written in-process by [`DocxExporter`]: it has the required
/// content-types, relationships, styles, numbering, and document parts, and it
/// is read back and validated before being returned. Each source page is
/// separated by a real Word page break, and one paragraph is emitted per
/// extracted line. Images are not embedded in this round.
///
/// # Errors
///
/// Returns a typed [`PdfExportError`] when extraction, model mapping, or ZIP
/// packaging fails.
pub fn export_docx(
    document: &Document,
    options: &PdfDocxOptions,
) -> Result<OfficeExport, PdfExportError> {
    let model = build_office_document(document, &options.title)?;
    Ok(DocxExporter.write(&model)?)
}

/// Export a parsed PDF to any A-27 format in one call.
///
/// This is the single dispatch point the application shell should call. It
/// validates the format, the page selection, the title, and the raster options
/// before doing any work, extracts only the selected pages, and returns an
/// [`ExportedFile`] whose parts have already been written and, for Office
/// formats, read back and validated by their adapter.
///
/// # Format behaviour
///
/// | Format | Parts | Content |
/// |---|---|---|
/// | `Text` | 1 | extracted text, one line feed between pages |
/// | `Docx` | 1 | one paragraph per extracted line, one Word page break per source page |
/// | `Xlsx` | 1 | one sheet per source page, one extracted line per row |
/// | `Pptx` | 1 | one slide per source page with a `Page N` title and body lines |
/// | `Html` | 1 | one `<section>` per page with escaped `<p>` lines and inline CSS |
/// | `Rtf` | 1 | RTF 1.9 with `\page` between pages and `\uN?` escapes |
/// | `Png`/`Jpeg` | one per page | renderer raster at [`ExportOptions::dpi`] |
/// | `Tiff` | 1 | one multi-page uncompressed TIFF at [`ExportOptions::dpi`] |
///
/// # Errors
///
/// Returns [`PdfExportError::UnsupportedFormat`] when the format needs a
/// feature this build lacks, [`PdfExportError::EmptyPageSelection`],
/// [`PdfExportError::InvalidPageRange`], or
/// [`PdfExportError::PageOutOfRange`] for a bad selection,
/// [`PdfExportError::InvalidDpi`] or
/// [`PdfExportError::InvalidJpegQuality`] for bad raster options, and the
/// writer-specific variants for a packaging failure.
pub fn export_pdf(
    document: &Document,
    format: ExportFormat,
    options: &ExportOptions,
) -> Result<ExportedFile, PdfExportError> {
    #[cfg(feature = "renderer")]
    let result = export_pdf_inner(document, format, options, &CancellationToken::new());
    #[cfg(not(feature = "renderer"))]
    let result = export_pdf_inner(document, format, options, &());
    result
}

/// Cancellable form of [`export_pdf`].
///
/// Image export passes `cancellation` to the renderer; every other format
/// checks it once before extraction. A cancelled token surfaces as the
/// renderer's typed cancellation error or as
/// [`PdfExportError::Core`] (`pdfxi_core::Error::Cancelled`), so callers see a
/// typed failure rather than partial output.
///
/// # Errors
///
/// Everything [`export_pdf`] documents.
#[cfg(feature = "renderer")]
pub fn export_pdf_with_cancellation(
    document: &Document,
    format: ExportFormat,
    options: &ExportOptions,
    cancellation: &CancellationToken,
) -> Result<ExportedFile, PdfExportError> {
    export_pdf_inner(document, format, options, cancellation)
}

/// Shared implementation behind [`export_pdf`].
fn export_pdf_inner(
    document: &Document,
    format: ExportFormat,
    options: &ExportOptions,
    cancellation: ExportToken<'_>,
) -> Result<ExportedFile, PdfExportError> {
    if !format.is_implemented() {
        return Err(PdfExportError::UnsupportedFormat { format });
    }
    let page_count = document.page_count()?;
    if page_count == 0 {
        return Err(PdfExportError::EmptyDocument);
    }
    let pages = selected_pages(&options.selection, page_count)?;
    if format.is_image() {
        if options.dpi == 0 || options.dpi > MAX_EXPORT_DPI {
            return Err(PdfExportError::InvalidDpi(options.dpi));
        }
        if format == ExportFormat::Jpeg && (options.jpeg_quality == 0 || options.jpeg_quality > 100)
        {
            return Err(PdfExportError::InvalidJpegQuality(options.jpeg_quality));
        }
    } else if format != ExportFormat::Text
        && format != ExportFormat::Rtf
        && options.title.trim().is_empty()
    {
        return Err(PdfExportError::EmptyTitle);
    }
    #[cfg(feature = "renderer")]
    if cancellation.is_cancelled() {
        return Err(cancelled_error(format));
    }

    match format {
        ExportFormat::Text => export_dispatch_text(document, &pages),
        ExportFormat::Docx => export_dispatch_docx(document, options, &pages),
        ExportFormat::Xlsx => export_dispatch_xlsx(document, options, &pages),
        ExportFormat::Pptx => export_dispatch_pptx(document, options, &pages),
        ExportFormat::Html => export_dispatch_html(document, options, &pages),
        ExportFormat::Rtf => export_dispatch_rtf(document, &pages),
        ExportFormat::Png | ExportFormat::Jpeg | ExportFormat::Tiff => {
            export_dispatch_image(document, format, options, &pages, cancellation)
        }
    }
}

/// Map a pre-cancelled dispatch to the format-appropriate typed error.
#[cfg(feature = "renderer")]
fn cancelled_error(format: ExportFormat) -> PdfExportError {
    if format.is_image() {
        #[cfg(feature = "pdfium")]
        {
            return PdfExportError::Image(pdfxi_render::ExportError::Cancelled);
        }
        #[cfg(not(feature = "pdfium"))]
        {
            return PdfExportError::UnsupportedFormat { format };
        }
    }
    PdfExportError::Core(pdfxi_core::Error::Cancelled)
}

/// Resolve a [`TextPageSelection`] into ordered, validated, deduplicated
/// zero-based page indices.
fn selected_pages(
    selection: &TextPageSelection,
    page_count: usize,
) -> Result<Vec<usize>, PdfExportError> {
    match selection {
        TextPageSelection::All => Ok((0..page_count).collect()),
        TextPageSelection::Range { first, last } => {
            if last < first {
                return Err(PdfExportError::InvalidPageRange {
                    first: *first,
                    last: *last,
                });
            }
            if *last >= page_count {
                return Err(PdfExportError::PageOutOfRange {
                    page: *last,
                    page_count,
                });
            }
            Ok((*first..=*last).collect())
        }
        TextPageSelection::Pages(pages) => {
            if pages.is_empty() {
                return Err(PdfExportError::EmptyPageSelection);
            }
            let mut seen = HashSet::with_capacity(pages.len());
            let mut out = Vec::with_capacity(pages.len());
            for &page in pages {
                if page >= page_count {
                    return Err(PdfExportError::PageOutOfRange { page, page_count });
                }
                if seen.insert(page) {
                    out.push(page);
                }
            }
            Ok(out)
        }
    }
}

/// Extract raw text for the selected pages, for text-shaped writers.
fn extracted_page_texts(
    document: &Document,
    pages: &[usize],
) -> Result<Vec<(usize, String)>, PdfExportError> {
    let mut out = Vec::with_capacity(pages.len());
    for &page in pages {
        out.push((page, document.page_text(page)?));
    }
    Ok(out)
}

/// Plain text: one line feed between pages, no headings.
fn export_dispatch_text(
    document: &Document,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let selection = TextPageSelection::Pages(pages.to_vec());
    let text = export_text(document, &PdfTextOptions::new().with_selection(selection))?;
    Ok(ExportedFile {
        format: ExportFormat::Text,
        page_count: pages.len(),
        parts: vec![ExportedPart {
            name: "document".to_owned(),
            bytes: text.into_bytes(),
            pixel_size: None,
        }],
        unsupported: Vec::new(),
    })
}

/// DOCX: one paragraph per line and a real Word page break between pages.
fn export_dispatch_docx(
    document: &Document,
    options: &ExportOptions,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let model = build_office_document_for_pages(document, &options.title, pages)?;
    let export = DocxExporter.write(&model)?;
    Ok(office_exported_file(
        ExportFormat::Docx,
        pages.len(),
        export,
    ))
}

/// XLSX: one worksheet per page named `Page N`, one line per row in column A.
fn export_dispatch_xlsx(
    document: &Document,
    options: &ExportOptions,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let model = build_office_document_for_pages(document, &options.title, pages)?;
    let export = XlsxExporter::new().write(&model)?;
    Ok(office_exported_file(
        ExportFormat::Xlsx,
        pages.len(),
        export,
    ))
}

/// PPTX: one slide per page with a generated `Page N` title and body lines.
fn export_dispatch_pptx(
    document: &Document,
    options: &ExportOptions,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let base = build_office_document_for_pages(document, &options.title, pages)?;
    let slide_pages = base
        .pages
        .into_iter()
        .map(|page| {
            let mut blocks = Vec::with_capacity(page.blocks.len() + 1);
            blocks.push(OfficeBlock::styled(
                format!("Page {}", page.index.saturating_add(1)),
                ParagraphStyle::Title,
            ));
            blocks.extend(page.blocks);
            OfficePage::new(page.index, page.width, page.height).with_blocks(blocks)
        })
        .collect();
    let model = OfficeDocument::new(base.title, slide_pages);
    let export = PptxExporter.write(&model)?;
    Ok(office_exported_file(
        ExportFormat::Pptx,
        pages.len(),
        export,
    ))
}

/// HTML: one semantic `<section>` per page, inline CSS, no external assets.
fn export_dispatch_html(
    document: &Document,
    options: &ExportOptions,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let texts = extracted_page_texts(document, pages)?;
    let html = build_pdf_html(&options.title, &texts);
    Ok(ExportedFile {
        format: ExportFormat::Html,
        page_count: pages.len(),
        parts: vec![ExportedPart {
            name: "document".to_owned(),
            bytes: html.into_bytes(),
            pixel_size: None,
        }],
        unsupported: Vec::new(),
    })
}

/// RTF: fixed header, `\par` per line, `\page` between pages.
fn export_dispatch_rtf(
    document: &Document,
    pages: &[usize],
) -> Result<ExportedFile, PdfExportError> {
    let texts = extracted_page_texts(document, pages)?;
    let rtf_pages = texts
        .into_iter()
        .map(|(page, text)| RtfPage::new(page, text));
    let text = export_rtf_string(rtf_pages, &RtfOptions::new().with_page_headers(false))?;
    Ok(ExportedFile {
        format: ExportFormat::Rtf,
        page_count: pages.len(),
        parts: vec![ExportedPart {
            name: "document".to_owned(),
            bytes: text.into_bytes(),
            pixel_size: None,
        }],
        unsupported: Vec::new(),
    })
}

/// Wrap an adapter's validated archive in an [`ExportedFile`].
fn office_exported_file(
    format: ExportFormat,
    page_count: usize,
    export: OfficeExport,
) -> ExportedFile {
    ExportedFile {
        format,
        page_count,
        parts: vec![ExportedPart {
            name: "document".to_owned(),
            bytes: export.bytes,
            pixel_size: None,
        }],
        unsupported: export.report.unsupported,
    }
}

/// Build the self-contained semantic HTML document.
///
/// The output is deliberately simple: one `<h2>` and one `<p>` per extracted
/// line inside a `<section>` per page. There is no script, no image, no
/// stylesheet link, and no external URL; the CSS is one inline `<style>` block.
/// Every text and attribute value goes through [`xml::escape`].
fn build_pdf_html(title: &str, texts: &[(usize, String)]) -> String {
    let mut out = String::with_capacity(512 + texts.len() * 256);
    out.push_str("<!DOCTYPE html>\n<html lang=\"en\">\n<head>\n");
    out.push_str("<meta charset=\"utf-8\"/>\n");
    out.push_str("<meta name=\"viewport\" content=\"width=device-width, initial-scale=1\"/>\n");
    out.push_str(&format!("<title>{}</title>\n", xml::escape(title)));
    out.push_str(
        "<style>body{font-family:system-ui,-apple-system,'Segoe UI',sans-serif;\
         max-width:48rem;margin:2rem auto;padding:0 1rem;line-height:1.5;color:#1a1a1a}\
         h1{font-size:1.6rem;margin:0 0 1rem}h2{font-size:1.15rem;margin:0 0 .75rem}\
         section.page{border-top:1px solid #d0d0d0;padding-top:1rem;margin-top:2rem}\
         section.page:first-of-type{border-top:0;margin-top:0}\
         p{margin:0 0 .5rem;white-space:pre-wrap}</style>\n",
    );
    out.push_str("</head>\n<body>\n");
    out.push_str(&format!("<h1>{}</h1>\n", xml::escape(title)));
    for (page, text) in texts {
        let number = page.saturating_add(1);
        out.push_str(&format!(
            "<section class=\"page\" id=\"page-{number}\" data-page=\"{number}\">\n\
             <h2>Page {number}</h2>\n"
        ));
        for line in text_lines(text) {
            out.push_str(&format!("<p>{}</p>\n", xml::escape(line)));
        }
        out.push_str("</section>\n");
    }
    out.push_str("</body>\n</html>\n");
    out
}

/// Renderer-backed image export (compiled only with the `pdfium` feature).
#[cfg(feature = "pdfium")]
fn export_dispatch_image(
    document: &Document,
    format: ExportFormat,
    options: &ExportOptions,
    pages: &[usize],
    cancellation: ExportToken<'_>,
) -> Result<ExportedFile, PdfExportError> {
    let bytes = document.save()?;
    let renderer = pdfxi_render::PdfiumRenderer::from_bytes(bytes)?;
    let (image_format, color, alpha) = if format == ExportFormat::Png {
        (
            pdfxi_render::ImageFormat::Png,
            pdfxi_render::ColorMode::Rgba,
            pdfxi_render::AlphaPolicy::Preserve,
        )
    } else if format == ExportFormat::Jpeg {
        (
            pdfxi_render::ImageFormat::Jpeg,
            pdfxi_render::ColorMode::Rgb,
            pdfxi_render::AlphaPolicy::Flatten {
                background: [255, 255, 255],
            },
        )
    } else {
        (
            pdfxi_render::ImageFormat::Tiff,
            pdfxi_render::ColorMode::Rgba,
            pdfxi_render::AlphaPolicy::Preserve,
        )
    };
    let mut one_based = Vec::with_capacity(pages.len());
    for &page in pages {
        let number =
            u32::try_from(page.saturating_add(1)).map_err(|_| PdfExportError::PageOutOfRange {
                page,
                page_count: pages.len(),
            })?;
        one_based.push(number);
    }
    let render_options = pdfxi_render::ExportOptions {
        format: image_format,
        color,
        alpha,
        selection: pdfxi_render::PageSelection::List(one_based),
        dpi: options.dpi,
        fit: pdfxi_render::FitMode::Actual,
        quality: options.jpeg_quality,
    };
    let images = pdfxi_render::export_pages(&renderer, &render_options, cancellation, &mut |_| {})?;
    let mut parts = Vec::with_capacity(images.len());
    for image in images {
        let number = image.page.get().saturating_add(1);
        // The renderer batches every selected page into one TIFF IFD chain.
        let name = if image_format == pdfxi_render::ImageFormat::Tiff {
            "document".to_owned()
        } else {
            format!("page-{number}")
        };
        parts.push(ExportedPart {
            name,
            bytes: image.bytes,
            pixel_size: Some((image.width, image.height)),
        });
    }
    Ok(ExportedFile {
        format,
        page_count: pages.len(),
        parts,
        unsupported: Vec::new(),
    })
}

/// Image export placeholder for builds without the native renderer.
#[cfg(not(feature = "pdfium"))]
fn export_dispatch_image(
    _document: &Document,
    format: ExportFormat,
    _options: &ExportOptions,
    _pages: &[usize],
    _cancellation: ExportToken<'_>,
) -> Result<ExportedFile, PdfExportError> {
    Err(PdfExportError::UnsupportedFormat { format })
}

/// Return true when `index` is part of the selected page set.
///
/// This only avoids extracting pages the caller asked to skip; the shared text
/// plan still applies the documented ordering, duplicate, and missing-page
/// rules to the extracted pairs.
fn selection_includes(
    selection: &TextPageSelection,
    explicit: &HashSet<usize>,
    index: usize,
    page_count: usize,
) -> bool {
    if index >= page_count {
        return false;
    }
    match selection {
        TextPageSelection::All => true,
        TextPageSelection::Range { first, last } => index >= *first && index <= *last,
        TextPageSelection::Pages(_) => explicit.contains(&index),
    }
}

/// Convert a PDF media-box length to positive `f32` page geometry.
fn export_geometry(value: f64, page: usize) -> Result<f32, PdfExportError> {
    if !value.is_finite() || value <= 0.0 || value > f64::from(f32::MAX) {
        return Err(PdfExportError::InvalidPageGeometry { page });
    }
    Ok(value as f32)
}

/// Split extracted page text into paragraph lines.
///
/// Both CRLF and lone CR are normalized to LF, trailing whitespace on a line is
/// removed, and empty lines are dropped. The function never allocates more than
/// the input length and is deterministic.
fn text_lines(text: &str) -> Vec<&str> {
    let mut lines = Vec::new();
    for raw in text.split('\n') {
        let line = raw.trim_end_matches('\r').trim_end();
        if !line.is_empty() {
            lines.push(line);
        }
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office_export::zip;
    use std::path::{Path, PathBuf};

    /// Workspace root derived from the crate manifest directory.
    fn workspace_root() -> PathBuf {
        Path::new(env!("CARGO_MANIFEST_DIR"))
            .parent()
            .and_then(Path::parent)
            .map(Path::to_path_buf)
            .unwrap_or_else(|| PathBuf::from(env!("CARGO_MANIFEST_DIR")))
    }

    /// `target/lanes/convert/t0239-out`, honouring `CARGO_TARGET_DIR`.
    fn output_dir() -> PathBuf {
        let root = workspace_root();
        let base = std::env::var_os("CARGO_TARGET_DIR")
            .map(PathBuf::from)
            .map(|path| {
                if path.is_absolute() {
                    path
                } else {
                    root.join(path)
                }
            })
            .unwrap_or_else(|| root.join("target").join("lanes").join("convert"));
        base.join("t0239-out")
    }

    fn fixture(name: &str) -> Document {
        let path = workspace_root()
            .join("fixtures")
            .join("realworld")
            .join(name);
        let bytes = std::fs::read(&path).expect("read fixture");
        Document::from_bytes(bytes).expect("fixture parses")
    }

    /// Assert that every `<w:t>` text node escapes `<` and every ampersand
    /// starts one of the five predefined entities. This is the minimal XML
    /// well-formedness check the round-A fixture test needs.
    fn assert_text_nodes_are_xml_safe(xml: &str) {
        assert!(xml.starts_with("<?xml"));
        let mut from = 0;
        while let Some(relative) = xml[from..].find("<w:t") {
            let start = from + relative;
            let after_name = start + 4;
            let Some(next) = xml.as_bytes().get(after_name) else {
                break;
            };
            if *next != b'>' && *next != b' ' {
                from = after_name;
                continue;
            }
            let tag_end = after_name + xml[after_name..].find('>').expect("text start tag end");
            let content_start = tag_end + 1;
            let content_end =
                content_start + xml[content_start..].find("</w:t>").expect("text end tag");
            let content = &xml[content_start..content_end];
            assert!(!content.contains('<'), "unescaped '<' in a text node");
            for (offset, ch) in content.char_indices() {
                if ch == '&' {
                    let rest = &content[offset..];
                    assert!(
                        ["&amp;", "&lt;", "&gt;", "&quot;", "&apos;"]
                            .iter()
                            .any(|entity| rest.starts_with(entity)),
                        "raw '&' in a text node: {rest:.24}"
                    );
                }
            }
            from = content_end + "</w:t>".len();
        }
    }

    fn docx_part<'a>(export: &'a OfficeExport, name: &str) -> &'a str {
        let views = zip::read_zip(&export.bytes).expect("docx is a readable zip");
        let view = views
            .iter()
            .find(|view| view.name == name)
            .unwrap_or_else(|| panic!("missing part {name}"));
        std::str::from_utf8(view.data).expect("part is UTF-8")
    }

    #[test]
    fn text_export_from_excel_fixture_has_markers_and_page_separator() {
        let document = fixture("microsoft-excel-2019-sample.pdf");
        let text = export_text(&document, &PdfTextOptions::new()).expect("text export");
        assert!(text.contains("PDFXI Real-World Sample"));
        assert!(text.contains("amber compass lantern"));
        assert!(text.contains("Second page marker: meadow harmonic"));
        assert!(text.contains('\n'), "pages must be separated");

        let headings = export_text(&document, &PdfTextOptions::new().with_page_headings(true))
            .expect("headed text");
        assert!(headings.contains("--- Page 1 ---"));
        assert!(headings.contains("--- Page 2 ---"));

        let form_feed = export_text(
            &document,
            &PdfTextOptions::new().with_separator(PdfPageSeparator::FormFeed),
        )
        .expect("form-feed text");
        assert!(form_feed.contains('\u{0c}'));
        assert!(form_feed.contains("Second page marker: meadow harmonic"));
    }

    #[test]
    fn text_export_from_powerpoint_fixture_has_three_pages_and_markers() {
        let document = fixture("powerpoint-slides.pdf");
        let text = export_text(&document, &PdfTextOptions::new()).expect("text export");
        for marker in [
            "lantern slide marker",
            "harbor slide marker",
            "meadow slide marker",
        ] {
            assert!(text.contains(marker), "missing marker {marker}");
        }
        assert!(
            text.matches('\n').count() >= 2,
            "at least two separators for three pages"
        );
    }

    #[test]
    fn docx_export_from_real_fixtures_has_required_parts_and_page_breaks() {
        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        for (name, expected_pages) in [
            ("microsoft-excel-2019-sample.pdf", 2usize),
            ("powerpoint-slides.pdf", 3),
        ] {
            let document = fixture(name);
            let export = export_docx(&document, &PdfDocxOptions::new()).expect("docx export");
            let views = zip::read_zip(&export.bytes).expect("docx is a readable zip");
            zip::require_parts(
                &views,
                &["[Content_Types].xml", "_rels/.rels", "word/document.xml"],
            )
            .expect("required OOXML parts");

            let xml = docx_part(&export, "word/document.xml");
            assert_text_nodes_are_xml_safe(xml);
            assert!(
                xml.contains("PDFXI Real-World Sample") || xml.contains("lantern slide marker")
            );
            assert_eq!(
                xml.matches("<w:br w:type=\"page\"/>").count(),
                expected_pages - 1,
                "one Word page break between pages"
            );
            assert!(xml.contains("<w:sectPr>"));

            let path = out.join(name).with_extension("docx");
            std::fs::write(&path, &export.bytes).expect("write docx");
            assert!(path.is_file());
            assert!(export.report.part_count >= 5);

            let text = export_text(&document, &PdfTextOptions::new()).expect("text export");
            let text_path = out.join(name).with_extension("txt");
            std::fs::write(&text_path, text.as_bytes()).expect("write txt");
            assert!(text_path.is_file());
        }
    }

    #[test]
    fn docx_export_contains_markers_for_both_fixtures() {
        let excel = fixture("microsoft-excel-2019-sample.pdf");
        let export = export_docx(&excel, &PdfDocxOptions::new()).expect("excel docx");
        let xml = docx_part(&export, "word/document.xml");
        for marker in [
            "PDFXI Real-World Sample",
            "amber compass lantern",
            "Second page marker: meadow harmonic",
        ] {
            assert!(xml.contains(marker), "missing {marker}");
        }

        let slides = fixture("powerpoint-slides.pdf");
        let export = export_docx(&slides, &PdfDocxOptions::new()).expect("slides docx");
        let xml = docx_part(&export, "word/document.xml");
        for marker in [
            "lantern slide marker",
            "harbor slide marker",
            "meadow slide marker",
        ] {
            assert!(xml.contains(marker), "missing {marker}");
        }
    }

    #[test]
    fn docx_text_is_xml_escaped_and_control_characters_are_replaced() {
        // Exercise the same writer path `export_docx` uses with hostile text.
        let hostile = "a & b < c > d \"quoted\" \u{0001}";
        let model = OfficeDocument::new(
            "T",
            vec![
                OfficePage::new(0, 612.0, 792.0).with_blocks(vec![OfficeBlock::paragraph(hostile)]),
            ],
        );
        let export = DocxExporter.write(&model).expect("docx");
        let xml = docx_part(&export, "word/document.xml");
        assert!(xml.contains("a &amp; b &lt; c &gt; d &quot;quoted&quot;"));
        assert!(!xml.contains("a & b"));
        assert!(!xml.contains('\u{0001}'));
        assert!(xml.contains('\u{fffd}'));
    }

    #[test]
    fn text_selection_extracts_only_the_requested_pages() {
        let document = fixture("microsoft-excel-2019-sample.pdf");
        let second = export_text(
            &document,
            &PdfTextOptions::new().with_selection(TextPageSelection::Pages(vec![1])),
        )
        .expect("page 2 only");
        assert!(!second.contains("PDFXI Real-World Sample"));
        assert!(second.contains("Second page marker: meadow harmonic"));

        let range = export_text(
            &document,
            &PdfTextOptions::new().with_selection(TextPageSelection::Range { first: 1, last: 1 }),
        )
        .expect("range");
        assert_eq!(range, second);

        let out_of_range = export_text(
            &document,
            &PdfTextOptions::new().with_selection(TextPageSelection::Pages(vec![99])),
        )
        .expect("out-of-range selection is empty");
        assert!(out_of_range.is_empty());
    }

    #[test]
    fn options_and_separators_are_stable() {
        assert_eq!(PdfPageSeparator::default(), PdfPageSeparator::LineFeed);
        assert_eq!(PdfPageSeparator::FormFeed.as_str(), "\u{0c}");
        assert_eq!(PdfPageSeparator::all().len(), 3);
        assert_eq!(PdfPageSeparator::BlankLine.to_string(), "blank-line");
        assert_eq!(PdfTextOptions::default(), PdfTextOptions::new());
        assert_eq!(PdfDocxOptions::default().title, DEFAULT_DOCX_TITLE);
    }

    #[test]
    fn empty_title_and_invalid_geometry_are_typed_errors() {
        let document = fixture("microsoft-excel-2019-sample.pdf");
        assert!(matches!(
            build_office_document(&document, "   "),
            Err(PdfExportError::EmptyTitle)
        ));
        // A structurally damaged document surfaces the core error unchanged.
        let damaged = Document::from_bytes(b"%PDF-1.7\ntruncated".to_vec())
            .expect("header-only document is accepted for deferred parsing");
        assert!(matches!(
            build_office_document(&damaged, "T"),
            Err(PdfExportError::Core(_))
        ));
        assert!(matches!(
            export_geometry(f64::NAN, 3),
            Err(PdfExportError::InvalidPageGeometry { page: 3 })
        ));
        assert!(matches!(
            export_geometry(0.0, 1),
            Err(PdfExportError::InvalidPageGeometry { page: 1 })
        ));
    }

    #[test]
    fn text_lines_normalizes_and_drops_empty_lines() {
        assert_eq!(
            text_lines("one\r\ntwo\n\n three \r\n"),
            vec!["one", "two", " three"]
        );
        assert!(text_lines("\n\r\n").is_empty());
    }

    /// The two accepted real fixtures with their page count and markers.
    fn fixture_specs() -> [(&'static str, usize, &'static [&'static str]); 2] {
        [
            (
                "microsoft-excel-2019-sample.pdf",
                2,
                &[
                    "PDFXI Real-World Sample",
                    "amber compass lantern",
                    "Second page marker: meadow harmonic",
                ],
            ),
            (
                "powerpoint-slides.pdf",
                3,
                &[
                    "lantern slide marker",
                    "harbor slide marker",
                    "meadow slide marker",
                ],
            ),
        ]
    }

    fn part_of<'a>(bytes: &'a [u8], name: &str) -> &'a str {
        let views = zip::read_zip(bytes).expect("export is a readable zip");
        let view = views
            .iter()
            .find(|view| view.name == name)
            .unwrap_or_else(|| panic!("missing part {name}"));
        std::str::from_utf8(view.data).expect("part is UTF-8")
    }

    #[test]
    fn implemented_formats_match_the_build_features() {
        let formats = ExportFormat::implemented();
        for format in [
            ExportFormat::Text,
            ExportFormat::Docx,
            ExportFormat::Xlsx,
            ExportFormat::Pptx,
            ExportFormat::Html,
            ExportFormat::Rtf,
        ] {
            assert!(formats.contains(&format), "{format} must be available");
            assert!(format.is_implemented());
        }
        let images = [ExportFormat::Png, ExportFormat::Jpeg, ExportFormat::Tiff];
        assert_eq!(formats.len(), if cfg!(feature = "pdfium") { 9 } else { 6 });
        for format in images {
            assert_eq!(format.is_implemented(), cfg!(feature = "pdfium"));
            assert_eq!(formats.contains(&format), cfg!(feature = "pdfium"));
        }
        assert_eq!(ExportFormat::ALL.len(), 9);
        assert_eq!(ExportFormat::Png.extension(), "png");
        assert_eq!(ExportFormat::Jpeg.extension(), "jpg");
        assert_eq!(ExportFormat::Tiff.extension(), "tiff");
        assert_eq!(ExportFormat::Tiff.media_type(), "image/tiff");
        assert_eq!(
            ExportFormat::Docx.media_type(),
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document"
        );
        assert!(ExportFormat::Png.is_image());
        assert!(ExportFormat::Tiff.is_image());
        assert!(!ExportFormat::Html.is_image());
        assert_eq!(ExportFormat::Rtf.to_string(), "RTF");
    }

    #[test]
    fn text_dispatch_from_real_fixtures_has_markers_and_counts() {
        for (name, pages, markers) in fixture_specs() {
            let document = fixture(name);
            let file = export_pdf(&document, ExportFormat::Text, &ExportOptions::new())
                .expect("text dispatch");
            assert_eq!(file.page_count, pages);
            let part = file.single().expect("text is a single file");
            let text = std::str::from_utf8(&part.bytes).expect("UTF-8 text");
            for marker in markers {
                assert!(text.contains(marker), "{name} missing {marker}");
            }
            assert_eq!(part.name, "document");
            assert_eq!(file.total_bytes(), part.bytes.len());
        }
    }

    #[test]
    fn xlsx_export_from_real_fixtures_has_sheets_markers_and_widths() {
        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        for (name, pages, markers) in fixture_specs() {
            let document = fixture(name);
            let file = export_pdf(&document, ExportFormat::Xlsx, &ExportOptions::new())
                .expect("xlsx dispatch");
            assert_eq!(file.format, ExportFormat::Xlsx);
            assert_eq!(file.page_count, pages);
            let part = file.single().expect("xlsx is a single file");
            let views = zip::read_zip(&part.bytes).expect("xlsx is a readable zip");
            zip::require_parts(
                &views,
                &[
                    "[Content_Types].xml",
                    "_rels/.rels",
                    "xl/workbook.xml",
                    "xl/_rels/workbook.xml.rels",
                    "xl/styles.xml",
                    "xl/worksheets/sheet1.xml",
                ],
            )
            .expect("required XLSX parts");
            assert!(
                views
                    .iter()
                    .any(|view| view.name == format!("xl/worksheets/sheet{pages}.xml")),
                "one worksheet per source page"
            );
            let workbook = part_of(&part.bytes, "xl/workbook.xml");
            for number in 1..=pages {
                assert!(
                    workbook.contains(&format!("name=\"Page {number}\"")),
                    "missing sheet Page {number}"
                );
            }
            let sheet1 = part_of(&part.bytes, "xl/worksheets/sheet1.xml");
            assert!(sheet1.contains("<cols>"), "column widths are declared");
            assert!(
                sheet1.contains(markers[0]),
                "{name} sheet 1 missing {}",
                markers[0]
            );
            assert_eq!(
                part.bytes,
                export_pdf(&document, ExportFormat::Xlsx, &ExportOptions::new())
                    .expect("repeat")
                    .single()
                    .expect("single")
                    .bytes,
                "XLSX output is deterministic"
            );
            std::fs::write(out.join(name).with_extension("xlsx"), &part.bytes).expect("write xlsx");
        }
    }

    #[test]
    fn pptx_export_from_real_fixtures_has_slides_titles_and_markers() {
        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        for (name, pages, markers) in fixture_specs() {
            let document = fixture(name);
            let file = export_pdf(&document, ExportFormat::Pptx, &ExportOptions::new())
                .expect("pptx dispatch");
            assert_eq!(file.format, ExportFormat::Pptx);
            assert_eq!(file.page_count, pages);
            let part = file.single().expect("pptx is a single file");
            let views = zip::read_zip(&part.bytes).expect("pptx is a readable zip");
            zip::require_parts(
                &views,
                &[
                    "[Content_Types].xml",
                    "_rels/.rels",
                    "ppt/presentation.xml",
                    "ppt/_rels/presentation.xml.rels",
                    "ppt/slides/slide1.xml",
                    "ppt/slideMasters/slideMaster1.xml",
                    "ppt/slideLayouts/slideLayout1.xml",
                    "ppt/theme/theme1.xml",
                ],
            )
            .expect("required PPTX parts");
            for number in 1..=pages {
                assert!(
                    views
                        .iter()
                        .any(|view| view.name == format!("ppt/slides/slide{number}.xml")),
                    "missing slide {number}"
                );
            }
            let slide1 = part_of(&part.bytes, "ppt/slides/slide1.xml");
            assert!(slide1.contains("<a:t>Page 1</a:t>"), "title shape");
            assert!(
                slide1.contains(markers[0]),
                "{name} slide 1 missing {}",
                markers[0]
            );
            let last = part_of(&part.bytes, &format!("ppt/slides/slide{pages}.xml"));
            assert!(last.contains(&format!("<a:t>Page {pages}</a:t>")));

            let page = document.page(0).expect("first page");
            let presentation = part_of(&part.bytes, "ppt/presentation.xml");
            let cx = (page.width() * 12_700.0) as i64;
            let cy = (page.height() * 12_700.0) as i64;
            assert!(
                presentation.contains(&format!("<p:sldSz cx=\"{cx}\" cy=\"{cy}\"/>")),
                "slide size follows the source page"
            );
            std::fs::write(out.join(name).with_extension("pptx"), &part.bytes).expect("write pptx");
        }
    }

    #[test]
    fn html_export_from_real_fixtures_is_semantic_escaped_and_self_contained() {
        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        for (name, pages, markers) in fixture_specs() {
            let document = fixture(name);
            let file = export_pdf(
                &document,
                ExportFormat::Html,
                &ExportOptions::new().with_title("PDFXI export & <test>"),
            )
            .expect("html dispatch");
            assert_eq!(file.page_count, pages);
            let part = file.single().expect("html is a single file");
            let html = std::str::from_utf8(&part.bytes).expect("UTF-8 html");
            assert!(html.starts_with("<!DOCTYPE html>"));
            assert_eq!(html.matches("<section class=\"page\"").count(), pages);
            for number in 1..=pages {
                assert!(html.contains(&format!("<h2>Page {number}</h2>")));
            }
            for marker in markers {
                assert!(html.contains(marker), "{name} missing {marker}");
            }
            assert!(html.contains("<style>"), "inline CSS is present");
            assert!(!html.contains("<link"));
            assert!(!html.contains("<script"));
            assert!(!html.contains("<img"));
            assert!(!html.contains("http://"));
            assert!(!html.contains("https://"));
            assert!(html.contains("<h1>PDFXI export &amp; &lt;test&gt;</h1>"));
            std::fs::write(out.join(name).with_extension("html"), &part.bytes).expect("write html");
        }
    }

    #[test]
    fn html_text_is_escaped() {
        let html = build_pdf_html("T & <x>", &[(0, "a & b < c > d \"quoted\"".to_owned())]);
        assert!(html.contains("<title>T &amp; &lt;x&gt;</title>"));
        assert!(html.contains("<p>a &amp; b &lt; c &gt; d &quot;quoted&quot;</p>"));
        assert!(!html.contains("<x>"));
        assert!(!html.contains("a & b"));
    }

    #[test]
    fn rtf_export_from_real_fixtures_is_ascii_with_page_breaks_and_markers() {
        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        for (name, pages, markers) in fixture_specs() {
            let document = fixture(name);
            let file = export_pdf(&document, ExportFormat::Rtf, &ExportOptions::new())
                .expect("rtf dispatch");
            assert_eq!(file.page_count, pages);
            let part = file.single().expect("rtf is a single file");
            assert!(part.bytes.is_ascii(), "RTF output is ASCII");
            let rtf = std::str::from_utf8(&part.bytes).expect("ASCII is UTF-8");
            assert!(rtf.starts_with("{\\rtf1\\ansi\\deff0"));
            assert!(rtf.ends_with('}'));
            assert_eq!(rtf.matches("\\page").count(), pages - 1);
            assert!(rtf.contains("\\par\n"), "paragraph breaks are present");
            for marker in markers {
                assert!(rtf.contains(marker), "{name} missing {marker}");
            }
            std::fs::write(out.join(name).with_extension("rtf"), &part.bytes).expect("write rtf");
        }
    }

    #[test]
    fn invalid_export_options_are_rejected_before_writing() {
        let document = fixture("microsoft-excel-2019-sample.pdf");
        assert!(matches!(
            export_pdf(
                &document,
                ExportFormat::Docx,
                &ExportOptions::new().with_title("   ")
            ),
            Err(PdfExportError::EmptyTitle)
        ));
        assert!(matches!(
            export_pdf(
                &document,
                ExportFormat::Text,
                &ExportOptions::new().with_selection(TextPageSelection::Pages(Vec::new()))
            ),
            Err(PdfExportError::EmptyPageSelection)
        ));
        assert!(matches!(
            export_pdf(
                &document,
                ExportFormat::Xlsx,
                &ExportOptions::new()
                    .with_selection(TextPageSelection::Range { first: 1, last: 0 })
            ),
            Err(PdfExportError::InvalidPageRange { first: 1, last: 0 })
        ));
        assert!(matches!(
            export_pdf(
                &document,
                ExportFormat::Rtf,
                &ExportOptions::new().with_selection(TextPageSelection::Pages(vec![99]))
            ),
            Err(PdfExportError::PageOutOfRange {
                page: 99,
                page_count: 2
            })
        ));
        assert!(matches!(
            export_pdf(
                &document,
                ExportFormat::Pptx,
                &ExportOptions::new()
                    .with_selection(TextPageSelection::Range { first: 0, last: 2 })
            ),
            Err(PdfExportError::PageOutOfRange {
                page: 2,
                page_count: 2
            })
        ));

        #[cfg(feature = "pdfium")]
        {
            assert!(matches!(
                export_pdf(
                    &document,
                    ExportFormat::Png,
                    &ExportOptions::new().with_dpi(0)
                ),
                Err(PdfExportError::InvalidDpi(0))
            ));
            assert!(matches!(
                export_pdf(
                    &document,
                    ExportFormat::Jpeg,
                    &ExportOptions::new().with_jpeg_quality(0)
                ),
                Err(PdfExportError::InvalidJpegQuality(0))
            ));
            assert!(matches!(
                export_pdf(
                    &document,
                    ExportFormat::Png,
                    &ExportOptions::new().with_dpi(MAX_EXPORT_DPI + 1)
                ),
                Err(PdfExportError::InvalidDpi(_))
            ));
        }
        #[cfg(not(feature = "pdfium"))]
        {
            assert!(matches!(
                export_pdf(&document, ExportFormat::Png, &ExportOptions::new()),
                Err(PdfExportError::UnsupportedFormat {
                    format: ExportFormat::Png
                })
            ));
        }
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn tiff_export_batches_all_selected_pages_into_one_file() {
        let document = fixture("powerpoint-slides.pdf");
        let file = export_pdf(
            &document,
            ExportFormat::Tiff,
            &ExportOptions::new().with_dpi(36),
        )
        .expect("tiff dispatch");
        assert_eq!(file.format, ExportFormat::Tiff);
        assert_eq!(file.page_count, 3);
        assert_eq!(file.parts.len(), 1, "TIFF is one multi-page file");
        let part = &file.parts[0];
        assert_eq!(part.name, "document");
        assert_eq!(&part.bytes[..4], b"II*\0", "little-endian TIFF header");
        assert!(part.bytes.len() > 10_000, "three pages encode more");
        assert!(file.is_single_file());
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn image_export_from_real_fixture_matches_geometry_and_is_not_blank() {
        use pdfxi_render::{PageIndex, Renderer};

        let out = output_dir();
        std::fs::create_dir_all(&out).expect("create output directory");
        let document = fixture("microsoft-excel-2019-sample.pdf");
        let page = document.page(0).expect("first page");
        let selection = TextPageSelection::Pages(vec![0]);

        for (format, dpi) in [(ExportFormat::Png, 72u32), (ExportFormat::Jpeg, 144)] {
            let file = export_pdf(
                &document,
                format,
                &ExportOptions::new()
                    .with_selection(selection.clone())
                    .with_dpi(dpi),
            )
            .expect("image dispatch");
            assert_eq!(file.page_count, 1);
            assert_eq!(file.parts.len(), 1);
            let part = &file.parts[0];
            assert_eq!(part.name, "page-1");
            let (width, height) = part.pixel_size.expect("pixel size");
            let expected_width = page.width() * f64::from(dpi) / 72.0;
            let expected_height = page.height() * f64::from(dpi) / 72.0;
            assert!(
                (f64::from(width) - expected_width).abs() <= 1.0,
                "{format} width {width} vs {expected_width}"
            );
            assert!(
                (f64::from(height) - expected_height).abs() <= 1.0,
                "{format} height {height} vs {expected_height}"
            );
            if format == ExportFormat::Png {
                assert_eq!(
                    &part.bytes[..8],
                    &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
                );
                let ihdr_width = u32::from_be_bytes([
                    part.bytes[16],
                    part.bytes[17],
                    part.bytes[18],
                    part.bytes[19],
                ]);
                let ihdr_height = u32::from_be_bytes([
                    part.bytes[20],
                    part.bytes[21],
                    part.bytes[22],
                    part.bytes[23],
                ]);
                assert_eq!((ihdr_width, ihdr_height), (width, height));
            } else {
                assert_eq!(&part.bytes[..2], &[0xFF, 0xD8]);
                assert_eq!(&part.bytes[part.bytes.len() - 2..], &[0xFF, 0xD9]);
            }
            assert!(part.bytes.len() > 1_000, "an empty page encodes shorter");
            let path = out
                .join(format!("microsoft-excel-2019-sample-p1-{dpi}dpi"))
                .with_extension(file.extension());
            std::fs::write(&path, &part.bytes).expect("write image");
            assert!(path.is_file());
        }

        let all = export_pdf(
            &document,
            ExportFormat::Png,
            &ExportOptions::new().with_dpi(36),
        )
        .expect("all pages");
        assert_eq!(all.parts.len(), 2);
        assert_eq!(all.parts[0].name, "page-1");
        assert_eq!(all.parts[1].name, "page-2");
        assert_ne!(all.parts[0].bytes, all.parts[1].bytes);

        // Independent non-blank check: rasterize the same page and require
        // more than one distinct RGBA value.
        let renderer = pdfxi_render::PdfiumRenderer::from_bytes(document.save().expect("save"))
            .expect("open fixture in PDFium");
        let rendered = renderer
            .render_page(
                PageIndex::new(0),
                1.0,
                &CancellationToken::new(),
                &mut |_| {},
            )
            .expect("render page");
        let unique: std::collections::HashSet<[u8; 4]> = rendered
            .pixels()
            .as_chunks::<4>()
            .0
            .iter()
            .copied()
            .collect();
        assert!(
            unique.len() > 1,
            "the rendered fixture page must not be a uniform colour"
        );
    }
}
