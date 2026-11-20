//! Convert ordinary files into one PDF (acceptance item A-28).
//!
//! This module is the single document-level entry point for "Convert to PDF".
//! It classifies each input by its file name and by content sniffing, routes it
//! to the existing creation engine in [`crate::create`], and joins the results
//! into one PDF through `pdfxi-core`'s page planner and writer. It does not
//! re-implement text layout, HTML parsing, or image embedding: those rules stay
//! in [`crate::create`], and the assembled bytes are produced by
//! `pdfxi_core::Document`, which keeps ADR 0010's one-write-path rule.
//!
//! # Supported inputs
//!
//! | Input | Extensions | Notes |
//! |---|---|---|
//! | Plain text | `.txt`, `.text`, `.log` | UTF-8; line breaks preserved, long lines wrapped |
//! | Markdown | `.md`, `.markdown`, `.mdown`, `.mkd` | The documented ATX-heading, paragraph, bullet-list, and fenced-code subset |
//! | HTML | `.html`, `.htm`, `.xhtml` | Headings, paragraphs, lists, bold/italic, tables, local PNG/JPEG images |
//! | PNG | `.png` | Non-interlaced 8-bit grayscale or RGB; one page per image |
//! | JPEG | `.jpg`, `.jpeg`, `.jpe` | 8-bit, one or three components; one page per image |
//!
//! Each source produces its own pages, and the sources are placed in the order
//! they appear in [`ConvertInput`]. A request may mix any supported kinds; for
//! example two text files and a scanned page image become one three-page PDF.
//!
//! # Content sniffing
//!
//! Binary signatures win over the file name: a file named `notes.txt` whose
//! bytes start with the PNG signature is treated as PNG, and a ZIP container
//! carrying `word/`, `xl/`, or `ppt/` parts is reported as Office input. When no
//! signature matches, the extension decides; when the extension is missing or
//! unrecognized, UTF-8 content falls back to plain text and HTML documents
//! (recognized by a leading doctype, `<html>`, `<head>`, `<body>`, or `<table>`)
//! become HTML. Markdown can only be selected by extension, because no reliable
//! content signature exists for it.
//!
//! # Office documents
//!
//! `.docx`, `.xlsx`, `.pptx` and the wider Word/Excel/PowerPoint family are
//! recognized and refused with [`ConvertError::OfficeUnavailable`] in this
//! build. The library never launches Office or a COM server; a shell or a
//! feature-gated adapter does that work outside this contract. The error names
//! the detected format and says what to do instead, so a UI can show an
//! actionable message rather than "unsupported file".
//!
//! # Fidelity limits
//!
//! - Text, Markdown, and HTML are rendered with the base-14 Helvetica and
//!   Courier fonts, WinAnsi/Latin-1 encoding, and a fixed 1.2x line height by
//!   default. Characters outside Latin-1 become `?`; this is a text-layout
//!   engine, not a font-embedding pipeline.
//! - Markdown inline emphasis, links, images, tables, ordered lists, and
//!   blockquotes are not interpreted and appear literally.
//! - Images are embedded without re-encoding. The page size comes from the
//!   options, and the embedded `pHYs`/JFIF density decides the intrinsic point
//!   size that is fitted into the content area; images are never enlarged.
//! - The assembled document is a classic-xref PDF 1.x file. Cross-reference
//!   streams, tagging, and incremental history belong to later stages.
//!
//! # Determinism and bounds
//!
//! The same sources and options produce byte-for-byte identical output: no
//! timestamp, random identifier, or locale value enters the file. Inputs are
//! bounded by [`CONVERT_MAX_SOURCES`] and [`CONVERT_MAX_TOTAL_BYTES`], and each
//! format keeps its own limits (for example [`crate::HTML_MAX_INPUT_BYTES`]).
//! Every path is caller-supplied; HTML images resolve only inside the source
//! file's directory and escape attempts are rejected by the HTML importer.

use std::fmt;
use std::path::{Path, PathBuf};

use pdfxi_core::metadata::DocumentInfo;
use pdfxi_core::{Document, PagePlan};

use crate::create::{
    CreateError, CreateImageFormat, CreateOptions, HtmlOptions, Margins, PageSize,
    create_from_html_with_options, create_from_image, create_from_markdown, create_from_text,
};

/// Maximum number of source documents in one conversion request.
pub const CONVERT_MAX_SOURCES: usize = 512;

/// Maximum total input size, in bytes, across every source in one request.
pub const CONVERT_MAX_TOTAL_BYTES: usize = 64 * 1024 * 1024;

/// Message recorded in [`ConvertError::OfficeUnavailable`] for Office input.
pub const OFFICE_UNAVAILABLE_MESSAGE: &str =
    "this build has no Office converter; export the document to PDF or HTML and convert that";

/// Bytes of a container that are inspected when detecting Office families.
const SNIFF_WINDOW_BYTES: usize = 64 * 1024;

/// `/Creator` value written into the converted document.
const CREATOR: &str = "pdfxi-convert";

/// `/Producer` value written into the converted document.
const PRODUCER: &str = "PDFXI";

/// Errors returned while converting inputs into a PDF.
#[derive(Debug, thiserror::Error)]
pub enum ConvertError {
    /// The request contained no source documents.
    #[error("no input documents to convert")]
    EmptyInput,
    /// The request exceeded [`CONVERT_MAX_SOURCES`].
    #[error("too many input documents: {count} (limit {limit})")]
    TooManySources {
        /// Number of sources supplied.
        count: usize,
        /// The configured limit.
        limit: usize,
    },
    /// The sources exceeded [`CONVERT_MAX_TOTAL_BYTES`].
    #[error("convert-to-PDF input is too large: {bytes} bytes (limit {limit})")]
    InputTooLarge {
        /// Total input bytes supplied.
        bytes: usize,
        /// The configured limit.
        limit: usize,
    },
    /// The format of a source is not convertible in this build.
    #[error(
        "unsupported convert-to-PDF source format '{format}' (supported: plain text, Markdown, HTML, PNG, JPEG)"
    )]
    UnsupportedFormat {
        /// Lower-cased extension or sniffed family name that was rejected.
        format: String,
    },
    /// The source is an Office document, which this build cannot convert.
    #[error("Office conversion is unavailable for '{format}': {message}")]
    OfficeUnavailable {
        /// Lower-cased Office format label, for example `docx`.
        format: String,
        /// What the caller can do instead.
        message: String,
    },
    /// A text-like source was not valid UTF-8.
    #[error("{format} input is not valid UTF-8")]
    NotUtf8 {
        /// The kind of source that failed decoding, for example `text`.
        format: String,
    },
    /// The creation options were rejected before any source was rendered.
    #[error("invalid conversion options: {0}")]
    Options(#[from] CreateError),
    /// One source failed to render; the message names it by position and file.
    #[error("source {name} could not be converted: {source}")]
    Source {
        /// Positional label such as `#2 (notes.md)`.
        name: String,
        /// The underlying creation error.
        #[source]
        source: CreateError,
    },
    /// Joining or writing the assembled PDF through `pdfxi-core` failed.
    #[error("PDF assembly failed: {0}")]
    Pdf(#[from] pdfxi_core::Error),
    /// Reading an input file or writing the output file failed.
    #[error("I/O error during conversion: {0}")]
    Io(#[from] std::io::Error),
}

/// The input family a [`ConvertSource`] holds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConvertSourceKind {
    /// Plain UTF-8 text.
    Text,
    /// The documented Markdown subset.
    Markdown,
    /// The bounded HTML subset.
    Html,
    /// A PNG image embedded without re-encoding.
    ImagePng,
    /// A JPEG image embedded without re-encoding.
    ImageJpeg,
}

impl ConvertSourceKind {
    /// Return a short, stable label suitable for reports and diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Text => "text",
            Self::Markdown => "markdown",
            Self::Html => "html",
            Self::ImagePng => "png",
            Self::ImageJpeg => "jpeg",
        }
    }

    /// Return `true` for the raster image kinds.
    #[must_use]
    pub const fn is_image(self) -> bool {
        matches!(self, Self::ImagePng | Self::ImageJpeg)
    }
}

impl fmt::Display for ConvertSourceKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.label())
    }
}

/// One source document in a conversion request.
///
/// Construct instances with [`ConvertSource::text`], [`ConvertSource::markdown`],
/// [`ConvertSource::html`], or [`ConvertSource::image`], then attach a display
/// name with [`ConvertSource::with_name`]. A [`ConvertInput`] holds an ordered
/// list of sources.
#[derive(Debug, Clone, PartialEq)]
pub enum ConvertSource {
    /// Plain text, one page flow with preserved line breaks.
    Text {
        /// Display name, usually the source file name.
        name: Option<String>,
        /// The text to lay out.
        text: String,
    },
    /// Markdown in the documented subset.
    Markdown {
        /// Display name, usually the source file name.
        name: Option<String>,
        /// The Markdown source.
        text: String,
    },
    /// HTML markup plus the directory local images resolve against.
    Html {
        /// Display name, usually the source file name.
        name: Option<String>,
        /// The HTML markup.
        markup: String,
        /// Directory for local image references; `None` uses the process
        /// current directory, matching [`crate::create_from_html`].
        base_dir: Option<PathBuf>,
    },
    /// A PNG or JPEG image, embedded verbatim.
    Image {
        /// Display name, usually the source file name.
        name: Option<String>,
        /// The encoded image bytes.
        bytes: Vec<u8>,
        /// The raster format of `bytes`.
        format: CreateImageFormat,
    },
}

impl ConvertSource {
    /// Build a plain-text source.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self::Text {
            name: None,
            text: text.into(),
        }
    }

    /// Build a Markdown source.
    #[must_use]
    pub fn markdown(text: impl Into<String>) -> Self {
        Self::Markdown {
            name: None,
            text: text.into(),
        }
    }

    /// Build an HTML source that resolves images against `base_dir`.
    #[must_use]
    pub fn html(markup: impl Into<String>, base_dir: Option<PathBuf>) -> Self {
        Self::Html {
            name: None,
            markup: markup.into(),
            base_dir,
        }
    }

    /// Build an image source from already-read bytes.
    #[must_use]
    pub fn image(bytes: Vec<u8>, format: CreateImageFormat) -> Self {
        Self::Image {
            name: None,
            bytes,
            format,
        }
    }

    /// Attach a display name, replacing any existing one.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        let name = name.into();
        match &mut self {
            Self::Text { name: slot, .. }
            | Self::Markdown { name: slot, .. }
            | Self::Html { name: slot, .. }
            | Self::Image { name: slot, .. } => *slot = Some(name),
        }
        self
    }

    /// Return the display name, when one was set.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        match self {
            Self::Text { name, .. }
            | Self::Markdown { name, .. }
            | Self::Html { name, .. }
            | Self::Image { name, .. } => name.as_deref(),
        }
    }

    /// Return the input family of this source.
    #[must_use]
    pub const fn kind(&self) -> ConvertSourceKind {
        match self {
            Self::Text { .. } => ConvertSourceKind::Text,
            Self::Markdown { .. } => ConvertSourceKind::Markdown,
            Self::Html { .. } => ConvertSourceKind::Html,
            Self::Image { format, .. } => match format {
                CreateImageFormat::Png => ConvertSourceKind::ImagePng,
                CreateImageFormat::Jpeg => ConvertSourceKind::ImageJpeg,
            },
        }
    }

    /// Return the number of input bytes this source carries.
    #[must_use]
    pub fn byte_len(&self) -> usize {
        match self {
            Self::Text { text, .. } | Self::Markdown { text, .. } => text.len(),
            Self::Html { markup, .. } => markup.len(),
            Self::Image { bytes, .. } => bytes.len(),
        }
    }
}

/// An ordered list of source documents to convert into one PDF.
///
/// The order of the sources is the page order of the result: every source
/// contributes its pages in sequence. Build one with the constructors, then add
/// sources with [`ConvertInput::with`] or [`ConvertInput::push`]; read files
/// with [`ConvertInput::from_path`] or [`ConvertInput::from_paths`]; use
/// [`ConvertInput::from_bytes`] when the caller already holds the bytes.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ConvertInput {
    sources: Vec<ConvertSource>,
}

impl ConvertInput {
    /// Create an empty request.
    #[must_use]
    pub fn new() -> Self {
        Self {
            sources: Vec::new(),
        }
    }

    /// Build a request from one source.
    #[must_use]
    pub fn from_source(source: ConvertSource) -> Self {
        Self {
            sources: vec![source],
        }
    }

    /// Build a request from an ordered list of sources.
    #[must_use]
    pub fn from_sources(sources: Vec<ConvertSource>) -> Self {
        Self { sources }
    }

    /// Append a source, returning the updated request.
    #[must_use]
    pub fn with(mut self, source: ConvertSource) -> Self {
        self.sources.push(source);
        self
    }

    /// Append a source in place.
    pub fn push(&mut self, source: ConvertSource) {
        self.sources.push(source);
    }

    /// Return the sources in page order.
    #[must_use]
    pub fn sources(&self) -> &[ConvertSource] {
        &self.sources
    }

    /// Return the number of sources.
    #[must_use]
    pub fn len(&self) -> usize {
        self.sources.len()
    }

    /// Return `true` when the request holds no sources.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.sources.is_empty()
    }

    /// Read one file and classify it by name and content.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError::Io`] when the file cannot be read,
    /// [`ConvertError::UnsupportedFormat`] for a format this build does not
    /// convert, [`ConvertError::OfficeUnavailable`] for Office input, and
    /// [`ConvertError::NotUtf8`] for text-like files that are not UTF-8.
    pub fn from_path(path: impl AsRef<Path>) -> Result<Self, ConvertError> {
        let path = path.as_ref();
        let bytes = std::fs::read(path)?;
        let name = path
            .file_name()
            .map(|name| name.to_string_lossy().into_owned());
        let mut input = Self::from_bytes(name.as_deref(), bytes)?;
        let base_dir = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(Path::to_path_buf);
        if let Some(base_dir) = base_dir {
            for source in &mut input.sources {
                if let ConvertSource::Html { base_dir: slot, .. } = source
                    && slot.is_none()
                {
                    *slot = Some(base_dir.clone());
                }
            }
        }
        Ok(input)
    }

    /// Read several files into one request, preserving the given order.
    ///
    /// # Errors
    ///
    /// Returns the first error produced by [`ConvertInput::from_path`].
    pub fn from_paths<P: AsRef<Path>>(
        paths: impl IntoIterator<Item = P>,
    ) -> Result<Self, ConvertError> {
        let mut sources = Vec::new();
        for path in paths {
            let input = Self::from_path(path)?;
            sources.extend(input.sources);
        }
        Ok(Self { sources })
    }

    /// Classify already-read bytes into a request.
    ///
    /// `name` selects the extension and is kept as the display name; `None`
    /// makes classification rely on content sniffing alone.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError::UnsupportedFormat`] for a format this build does
    /// not convert, [`ConvertError::OfficeUnavailable`] for Office input, and
    /// [`ConvertError::NotUtf8`] for text-like bytes that are not UTF-8.
    pub fn from_bytes(name: Option<&str>, bytes: Vec<u8>) -> Result<Self, ConvertError> {
        let detected = classify(name, &bytes)?;
        let display = name.map(str::to_owned);
        let source = match detected {
            Detected::Text => ConvertSource::Text {
                name: display,
                text: decode_text(bytes, "text")?,
            },
            Detected::Markdown => ConvertSource::Markdown {
                name: display,
                text: decode_text(bytes, "markdown")?,
            },
            Detected::Html => ConvertSource::Html {
                name: display,
                markup: decode_text(bytes, "html")?,
                base_dir: None,
            },
            Detected::Image(format) => ConvertSource::Image {
                name: display,
                bytes,
                format,
            },
        };
        Ok(Self {
            sources: vec![source],
        })
    }
}

/// Layout and metadata options shared by every source in a request.
#[derive(Debug, Clone, PartialEq)]
pub struct ConvertOptions {
    page_size: PageSize,
    margins: Margins,
    font_size: f64,
    line_height: f64,
    title: Option<String>,
}

impl ConvertOptions {
    /// Create options with US Letter pages, one-inch margins, 11-point body
    /// text, 1.2x line spacing, and no title.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page_size: PageSize::LETTER,
            margins: Margins::default(),
            font_size: 11.0,
            line_height: 1.2,
            title: None,
        }
    }

    /// Replace the page size applied to every page.
    #[must_use]
    pub fn with_page_size(mut self, page_size: PageSize) -> Self {
        self.page_size = page_size;
        self
    }

    /// Replace the margins applied to every page.
    #[must_use]
    pub fn with_margins(mut self, margins: Margins) -> Self {
        self.margins = margins;
        self
    }

    /// Replace the body font size in points.
    #[must_use]
    pub fn with_font_size(mut self, font_size: f64) -> Self {
        self.font_size = font_size;
        self
    }

    /// Replace the line-height multiplier.
    #[must_use]
    pub fn with_line_height(mut self, line_height: f64) -> Self {
        self.line_height = line_height;
        self
    }

    /// Set the document title written to the `/Info` dictionary.
    #[must_use]
    pub fn with_title(mut self, title: impl Into<String>) -> Self {
        self.title = Some(title.into());
        self
    }

    /// Return the configured page size.
    #[must_use]
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }

    /// Return the configured margins.
    #[must_use]
    pub const fn margins(&self) -> Margins {
        self.margins
    }

    /// Return the configured body font size.
    #[must_use]
    pub const fn font_size(&self) -> f64 {
        self.font_size
    }

    /// Return the configured line-height multiplier.
    #[must_use]
    pub const fn line_height(&self) -> f64 {
        self.line_height
    }

    /// Return the configured title, when one was set.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Validate the geometry and typography settings.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError::Options`] wrapping the creation-engine
    /// validation error when the page size, margins, font size, or line height
    /// is out of range, or when the margins leave no content area.
    pub fn validate(&self) -> Result<(), ConvertError> {
        self.create_options().validate()?;
        Ok(())
    }

    /// Build the creation-engine options this request uses.
    fn create_options(&self) -> CreateOptions {
        CreateOptions::new()
            .with_page_size(self.page_size)
            .with_margins(self.margins)
            .with_font_size(self.font_size)
            .with_line_height(self.line_height)
    }
}

impl Default for ConvertOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// Where one source landed in the assembled document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConvertedPart {
    name: Option<String>,
    kind: ConvertSourceKind,
    first_page: usize,
    page_count: usize,
}

impl ConvertedPart {
    /// Return the source display name, when one was set.
    #[must_use]
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Return the input family of the source.
    #[must_use]
    pub const fn kind(&self) -> ConvertSourceKind {
        self.kind
    }

    /// Return the zero-based index of this source's first page.
    #[must_use]
    pub const fn first_page(&self) -> usize {
        self.first_page
    }

    /// Return how many pages this source contributed.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }
}

/// A converted PDF plus the report a caller needs to describe it.
///
/// The bytes are a complete classic-xref PDF. Write them with
/// [`ConvertedPdf::write_to`] or hand them to the document session; this type
/// never touches the caller's existing files.
#[derive(Debug, Clone)]
pub struct ConvertedPdf {
    bytes: Vec<u8>,
    page_count: usize,
    page_size: PageSize,
    title: Option<String>,
    parts: Vec<ConvertedPart>,
}

impl ConvertedPdf {
    /// Return the PDF bytes.
    #[must_use]
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume the value and return the PDF bytes.
    #[must_use]
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Return the number of pages in the assembled document.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Return the page size applied to every page.
    #[must_use]
    pub const fn page_size(&self) -> PageSize {
        self.page_size
    }

    /// Return the document title, when one was requested.
    #[must_use]
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Return one entry per input source, in page order.
    #[must_use]
    pub fn parts(&self) -> &[ConvertedPart] {
        &self.parts
    }

    /// Write the PDF to `path`.
    ///
    /// This is a plain write for a caller-owned output file. Atomic
    /// replacement and external-change detection belong to the document
    /// session (ADR 0010), not to this conversion result.
    ///
    /// # Errors
    ///
    /// Returns [`ConvertError::Io`] when the file cannot be written.
    pub fn write_to(&self, path: impl AsRef<Path>) -> Result<(), ConvertError> {
        std::fs::write(path, &self.bytes)?;
        Ok(())
    }
}

/// Convert the sources in `input` into one PDF.
///
/// Sources are rendered in order with the creation engine, every result is
/// parsed back through `pdfxi-core`, and the pages are joined with a merge page
/// plan. The caller's title, when set, is written to the `/Info` dictionary
/// together with fixed creator and producer values, so the output is
/// byte-stable for the same input.
///
/// # Errors
///
/// Returns [`ConvertError::EmptyInput`] for an empty request,
/// [`ConvertError::TooManySources`] or [`ConvertError::InputTooLarge`] when a
/// bound is exceeded, [`ConvertError::Options`] for invalid geometry,
/// [`ConvertError::Source`] when one source fails to render (the message names
/// it), and [`ConvertError::Pdf`] when assembly fails.
pub fn convert_to_pdf(
    input: ConvertInput,
    options: ConvertOptions,
) -> Result<ConvertedPdf, ConvertError> {
    options.validate()?;
    let create_options = options.create_options();
    if input.sources.is_empty() {
        return Err(ConvertError::EmptyInput);
    }
    if input.sources.len() > CONVERT_MAX_SOURCES {
        return Err(ConvertError::TooManySources {
            count: input.sources.len(),
            limit: CONVERT_MAX_SOURCES,
        });
    }
    let total_bytes: usize = input.sources.iter().map(ConvertSource::byte_len).sum();
    if total_bytes > CONVERT_MAX_TOTAL_BYTES {
        return Err(ConvertError::InputTooLarge {
            bytes: total_bytes,
            limit: CONVERT_MAX_TOTAL_BYTES,
        });
    }

    let mut parts = Vec::with_capacity(input.sources.len());
    let mut documents = Vec::with_capacity(input.sources.len());
    let mut first_page = 0usize;
    for (index, source) in input.sources.iter().enumerate() {
        let label = source_label(index, source.name());
        let rendered =
            render_source(source, &create_options).map_err(|source| ConvertError::Source {
                name: label,
                source,
            })?;
        let document = Document::from_bytes(rendered)?;
        let page_count = document.page_count()?;
        if page_count == 0 {
            return Err(ConvertError::Source {
                name: source_label(index, source.name()),
                source: CreateError::EmptyDocument,
            });
        }
        parts.push(ConvertedPart {
            name: source.name().map(str::to_owned),
            kind: source.kind(),
            first_page,
            page_count,
        });
        first_page = first_page.saturating_add(page_count);
        documents.push(document);
    }

    let mut documents = documents.into_iter();
    let primary = documents.next().ok_or(ConvertError::EmptyInput)?;
    let extras: Vec<Document> = documents.collect();
    let plan = if extras.is_empty() {
        PagePlan::identity()
    } else {
        PagePlan::merge(extras)
    };
    let merged = primary.apply_page_plan(&plan)?;

    let mut document = Document::from_bytes(merged)?;
    let page_count = document.page_count()?;
    let info = DocumentInfo {
        title: options.title.clone(),
        creator: Some(CREATOR.to_owned()),
        producer: Some(PRODUCER.to_owned()),
        ..DocumentInfo::default()
    };
    document.set_info(&info)?;
    let bytes = document.save()?;

    Ok(ConvertedPdf {
        bytes,
        page_count,
        page_size: options.page_size,
        title: options.title.clone(),
        parts,
    })
}

/// Convert `input` and write the result to `path`.
///
/// A convenience wrapper over [`convert_to_pdf`] and [`ConvertedPdf::write_to`]
/// for command-line callers and tests.
///
/// # Errors
///
/// Returns any error from [`convert_to_pdf`] or [`ConvertedPdf::write_to`].
pub fn convert_to_pdf_file(
    input: ConvertInput,
    options: ConvertOptions,
    path: impl AsRef<Path>,
) -> Result<ConvertedPdf, ConvertError> {
    let converted = convert_to_pdf(input, options)?;
    converted.write_to(path)?;
    Ok(converted)
}

/// Render one source with the creation engine, reusing its format rules.
fn render_source(source: &ConvertSource, options: &CreateOptions) -> Result<Vec<u8>, CreateError> {
    match source {
        ConvertSource::Text { text, .. } => create_from_text(text, options),
        ConvertSource::Markdown { text, .. } => create_from_markdown(text, options),
        ConvertSource::Html {
            markup, base_dir, ..
        } => {
            let mut html_options = HtmlOptions::new();
            if let Some(base_dir) = base_dir {
                html_options = html_options.with_base_dir(base_dir.clone());
            }
            create_from_html_with_options(markup, &html_options, options)
        }
        ConvertSource::Image { bytes, format, .. } => create_from_image(bytes, *format, options),
    }
}

/// Format a positional, human-readable label for a source.
fn source_label(index: usize, name: Option<&str>) -> String {
    let position = index + 1;
    match name {
        Some(name) => format!("#{position} ({name})"),
        None => format!("#{position}"),
    }
}

/// A classified input family produced by extension and content sniffing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Detected {
    /// Plain UTF-8 text.
    Text,
    /// The Markdown subset.
    Markdown,
    /// The HTML subset.
    Html,
    /// A raster image with the detected format.
    Image(CreateImageFormat),
}

/// Classify an input by file name extension, then by content.
fn classify(name: Option<&str>, bytes: &[u8]) -> Result<Detected, ConvertError> {
    if let Some(detected) = sniff_binary(bytes)? {
        return Ok(detected);
    }
    let extension = name.and_then(extension_of);
    classify_zip(extension.as_deref(), bytes)?;
    if let Some(extension) = extension.as_deref() {
        match extension {
            "txt" | "text" | "log" => return Ok(Detected::Text),
            "md" | "markdown" | "mdown" | "mkd" => return Ok(Detected::Markdown),
            "html" | "htm" | "xhtml" => return Ok(Detected::Html),
            "png" => return Ok(Detected::Image(CreateImageFormat::Png)),
            "jpg" | "jpeg" | "jpe" => return Ok(Detected::Image(CreateImageFormat::Jpeg)),
            other => {
                if is_office_extension(other) {
                    return Err(office_unavailable(other));
                }
                if is_known_unsupported(other) {
                    return Err(unsupported_format(other));
                }
            }
        }
    }
    if let Some(detected) = sniff_text(bytes) {
        return Ok(detected);
    }
    let label = extension.unwrap_or_else(|| "binary".to_owned());
    Err(unsupported_format(&label))
}

/// Return the lower-cased extension of `name`, if it has one.
fn extension_of(name: &str) -> Option<String> {
    Path::new(name)
        .extension()
        .and_then(|extension| extension.to_str())
        .map(str::to_ascii_lowercase)
}

/// Detect formats that carry an unambiguous binary signature.
///
/// Binary signatures take precedence over the file name so a misnamed file is
/// still handled correctly. A PDF payload is rejected by name; ZIP containers
/// are classified by [`classify`], which pairs the container parts with the
/// file name because Office packages share one signature.
fn sniff_binary(bytes: &[u8]) -> Result<Option<Detected>, ConvertError> {
    if bytes.starts_with(b"\x89PNG\r\n\x1a\n") {
        return Ok(Some(Detected::Image(CreateImageFormat::Png)));
    }
    if bytes.starts_with(&[0xFF, 0xD8, 0xFF]) {
        return Ok(Some(Detected::Image(CreateImageFormat::Jpeg)));
    }
    if bytes.starts_with(b"%PDF-") {
        return Err(unsupported_format("pdf"));
    }
    Ok(None)
}

/// Classify a ZIP container as an Office family or a generic archive.
///
/// The extension decides first because Word, Excel, and PowerPoint packages all
/// begin with the same local-file-header signature. When the name is unknown,
/// the member directory inside the first [`SNIFF_WINDOW_BYTES`] identifies the
/// family.
fn classify_zip(extension: Option<&str>, bytes: &[u8]) -> Result<(), ConvertError> {
    if !bytes.starts_with(b"PK\x03\x04") {
        return Ok(());
    }
    if let Some(extension) = extension.filter(|extension| is_office_extension(extension)) {
        return Err(office_unavailable(extension));
    }
    let window = &bytes[..bytes.len().min(SNIFF_WINDOW_BYTES)];
    if contains(window, b"word/") {
        return Err(office_unavailable("docx"));
    }
    if contains(window, b"xl/") {
        return Err(office_unavailable("xlsx"));
    }
    if contains(window, b"ppt/") {
        return Err(office_unavailable("pptx"));
    }
    Err(unsupported_format("zip"))
}

/// Detect HTML or plain text from UTF-8 content.
fn sniff_text(bytes: &[u8]) -> Option<Detected> {
    let text = std::str::from_utf8(bytes).ok()?;
    let trimmed = text.trim_start_matches('\u{feff}').trim_start();
    let prefix: String = trimmed
        .chars()
        .take(64)
        .collect::<String>()
        .to_ascii_lowercase();
    const HTML_PREFIXES: [&str; 6] = ["<!doctype html", "<html", "<head", "<body", "<table", "<p>"];
    if HTML_PREFIXES
        .iter()
        .any(|marker| prefix.starts_with(marker))
    {
        return Some(Detected::Html);
    }
    Some(Detected::Text)
}

/// Return `true` when `haystack` contains `needle`.
fn contains(haystack: &[u8], needle: &[u8]) -> bool {
    if needle.is_empty() || haystack.len() < needle.len() {
        return false;
    }
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

/// Return `true` for the Office extensions this build recognizes but cannot
/// convert.
fn is_office_extension(extension: &str) -> bool {
    matches!(
        extension,
        "doc"
            | "docx"
            | "docm"
            | "dot"
            | "dotx"
            | "odt"
            | "xls"
            | "xlsx"
            | "xlsm"
            | "xltx"
            | "ods"
            | "ppt"
            | "pptx"
            | "pptm"
            | "odp"
            | "ppsx"
    )
}

/// Return `true` for formats the crate or its shell adapters know about but
/// this entry point does not convert.
fn is_known_unsupported(extension: &str) -> bool {
    matches!(
        extension,
        "pdf"
            | "rtf"
            | "gif"
            | "bmp"
            | "tif"
            | "tiff"
            | "webp"
            | "svg"
            | "ps"
            | "eps"
            | "eml"
            | "mht"
            | "mhtml"
            | "csv"
            | "zip"
            | "7z"
            | "rar"
            | "bin"
    )
}

/// Build the typed Office-unavailable error for a format label.
fn office_unavailable(format: &str) -> ConvertError {
    ConvertError::OfficeUnavailable {
        format: format.to_owned(),
        message: OFFICE_UNAVAILABLE_MESSAGE.to_owned(),
    }
}

/// Build the typed unsupported-format error for a format label.
fn unsupported_format(format: &str) -> ConvertError {
    ConvertError::UnsupportedFormat {
        format: format.to_owned(),
    }
}

/// Decode text-like input as UTF-8, dropping a leading byte-order mark.
fn decode_text(bytes: Vec<u8>, format: &str) -> Result<String, ConvertError> {
    let text = String::from_utf8(bytes).map_err(|_| ConvertError::NotUtf8 {
        format: format.to_owned(),
    })?;
    match text.strip_prefix('\u{feff}') {
        Some(rest) => Ok(rest.to_owned()),
        None => Ok(text),
    }
}
