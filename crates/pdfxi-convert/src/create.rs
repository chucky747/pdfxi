//! Deterministic PDF creation from plain text, a small Markdown subset, and
//! raster images.
//!
//! This module is the write-side counterpart to the export modules. It
//! produces complete, classic-xref PDF byte streams without invoking an
//! external converter, decoding a compressed image, or adding a runtime image
//! crate. Every function is total for hostile input: malformed images and
//! invalid geometry return a typed [`CreateError`] instead of panicking.
//!
//! # Text and Markdown
//!
//! [`create_from_text`] preserves input line breaks and wraps long lines to the
//! content width. [`create_from_markdown`] recognises a deliberately small
//! subset of Markdown:
//!
//! - ATX headings (`#` … `######`) render in Helvetica-Bold, with larger sizes
//!   for higher levels;
//! - paragraphs (consecutive non-blank lines) are joined with a single space
//!   and word-wrapped;
//! - bullet lists (`- `, `* ` or `+ `) render each item as an indented
//!   `- `-prefixed line;
//! - fenced code blocks (lines delimited by ```` ``` ````) render verbatim in
//!   Courier with wrapping disabled.
//!
//! Inline emphasis, links, images, tables, and blockquotes are intentionally
//! *not* interpreted; their characters are emitted literally. This keeps the
//! parser small and fully deterministic.
//!
//! # Fonts, metrics and encoding
//!
//! Pages use the standard PDF base-14 fonts Helvetica (regular/bold) and
//! Courier. Word wrapping uses the published Helvetica AFM advance widths for
//! ASCII and a conservative fallback for other characters. Text is encoded as
//! a PDF literal string with WinAnsi/Latin-1 bytes: characters in the Latin-1
//! range are preserved, a handful of common punctuation code points are mapped
//! to their Windows-1252 bytes, and any other code point becomes `?`.
//! Delimiters and non-printable bytes use PDF escape sequences.
//!
//! # Images and DPI/page-fit rules
//!
//! PNG and JPEG files are embedded without decoding:
//!
//! - baseline/progressive JPEG scan bytes are embedded with `/DCTDecode`;
//! - non-interlaced 8-bit grayscale or RGB PNG `IDAT` data is embedded with
//!   `/FlateDecode` and a `/Predictor 15` decode parameter.
//!
//! PNG `pHYs` and JPEG JFIF density metadata set the effective DPI; when it is
//! absent, [`DEFAULT_DPI`] (72) is used. The intrinsic point size is
//! `pixels / dpi * 72`. The image is then scaled by `min(1, area_w / w,
//! area_h / h)` so it is never enlarged, and centred inside the page's content
//! area (page size minus margins). The configured page size is always used;
//! images larger than the content area are scaled down uniformly.
//!
//! PNGs with alpha, palettes, interlacing, or bit depths other than 8, and
//! JPEGs with unsupported component counts or precision, are rejected with
//! [`CreateError::UnsupportedImageFeature`] rather than silently misrendered.
//!
//! # Determinism
//!
//! Object numbering, dictionary key order, xref layout, and floating-point
//! formatting are fixed, so the same input and options always produce
//! byte-for-byte identical output. A small ASCII binary comment follows the
//! header so the output is a well-formed PDF 1.4 file. Every emitted stream,
//! including image XObjects, carries an exact `/Length` for its embedded bytes
//! as required by ISO 32000-1.

use std::path::Path;

/// Default image resolution, in dots per inch, when a file carries no density
/// metadata.
pub const DEFAULT_DPI: f64 = 72.0;

/// Errors returned while creating a PDF from text, Markdown, or an image.
#[derive(Debug, thiserror::Error)]
pub enum CreateError {
    /// The text or Markdown input contained no content to place on a page.
    #[error("cannot create a PDF from empty input")]
    EmptyDocument,
    /// A general creation option was out of range.
    #[error("invalid creation option: {0}")]
    InvalidOption(String),
    /// A page size was not finite or not positive.
    #[error("invalid page size: {0}")]
    InvalidPageSize(String),
    /// Margins did not fit inside the configured page size.
    #[error("invalid margins: {0}")]
    InvalidMargins(String),
    /// A file extension cannot be routed to a creation function.
    #[error("unsupported source extension '{extension}' for PDF creation")]
    UnsupportedSourceExtension {
        /// The lower-cased extension that could not be routed.
        extension: String,
    },
    /// A byte buffer cannot be routed to an image decoder-free embedder.
    #[error("unsupported image format '{extension}'")]
    UnsupportedImageFormat {
        /// The extension that could not be handled.
        extension: String,
    },
    /// The image bytes were structurally malformed.
    #[error("malformed {format} image: {message}")]
    MalformedImage {
        /// `"PNG"` or `"JPEG"`.
        format: &'static str,
        /// A human-readable reason.
        message: String,
    },
    /// The image is valid but uses a feature this embedder deliberately does not
    /// support.
    #[error("unsupported {format} feature: {message}")]
    UnsupportedImageFeature {
        /// `"PNG"` or `"JPEG"`.
        format: &'static str,
        /// A human-readable description of the missing feature.
        message: String,
    },
    /// Reading an input file failed.
    #[error("I/O error while creating PDF: {0}")]
    Io(#[from] std::io::Error),
}

/// A page size in PDF points (1 point = 1/72 inch).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct PageSize {
    /// Page width in points.
    pub width: f64,
    /// Page height in points.
    pub height: f64,
}

impl PageSize {
    /// US Letter, 612 by 792 points.
    pub const LETTER: Self = Self {
        width: 612.0,
        height: 792.0,
    };

    /// ISO A4, approximately 595.28 by 841.89 points.
    pub const A4: Self = Self {
        width: 595.28,
        height: 841.89,
    };

    /// Create a page size, validating that both dimensions are finite and
    /// positive.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InvalidPageSize`] when a dimension is not finite,
    /// is not positive, or exceeds 20,000 points.
    pub fn new(width: f64, height: f64) -> Result<Self, CreateError> {
        if !width.is_finite() || !height.is_finite() {
            return Err(CreateError::InvalidPageSize(
                "dimensions must be finite".to_owned(),
            ));
        }
        if width <= 0.0 || height <= 0.0 {
            return Err(CreateError::InvalidPageSize(
                "dimensions must be positive".to_owned(),
            ));
        }
        if width > 20_000.0 || height > 20_000.0 {
            return Err(CreateError::InvalidPageSize(
                "dimensions must not exceed 20000 points".to_owned(),
            ));
        }
        Ok(Self { width, height })
    }
}

impl Default for PageSize {
    fn default() -> Self {
        Self::LETTER
    }
}

/// Page margins in PDF points.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Margins {
    /// Left margin.
    pub left: f64,
    /// Top margin.
    pub top: f64,
    /// Right margin.
    pub right: f64,
    /// Bottom margin.
    pub bottom: f64,
}

impl Margins {
    /// No margins.
    pub const NONE: Self = Self {
        left: 0.0,
        top: 0.0,
        right: 0.0,
        bottom: 0.0,
    };

    /// Create margins with one value on every side.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InvalidMargins`] when `value` is not finite or is
    /// negative.
    pub fn uniform(value: f64) -> Result<Self, CreateError> {
        Self::new(value, value, value, value)
    }

    /// Create margins, validating that every value is finite and non-negative.
    ///
    /// # Errors
    ///
    /// Returns [`CreateError::InvalidMargins`] when any value is not finite or
    /// is negative. Whether the margins fit a page is checked by
    /// [`CreateOptions::validate`].
    pub fn new(left: f64, top: f64, right: f64, bottom: f64) -> Result<Self, CreateError> {
        for (name, value) in [
            ("left", left),
            ("top", top),
            ("right", right),
            ("bottom", bottom),
        ] {
            if !value.is_finite() || value < 0.0 {
                return Err(CreateError::InvalidMargins(format!(
                    "{name} margin must be finite and non-negative"
                )));
            }
        }
        Ok(Self {
            left,
            top,
            right,
            bottom,
        })
    }
}

impl Default for Margins {
    fn default() -> Self {
        Self {
            left: 72.0,
            top: 72.0,
            right: 72.0,
            bottom: 72.0,
        }
    }
}

/// Options shared by every creation function.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CreateOptions {
    page_size: PageSize,
    margins: Margins,
    font_size: f64,
    line_height_factor: f64,
}

impl CreateOptions {
    /// Create options with US Letter pages, one-inch margins, 11-point body
    /// text, and 1.2x line spacing.
    #[must_use]
    pub fn new() -> Self {
        Self {
            page_size: PageSize::LETTER,
            margins: Margins::default(),
            font_size: 11.0,
            line_height_factor: 1.2,
        }
    }

    /// Replace the page size.
    #[must_use]
    pub const fn with_page_size(mut self, page_size: PageSize) -> Self {
        self.page_size = page_size;
        self
    }

    /// Replace the margins.
    #[must_use]
    pub const fn with_margins(mut self, margins: Margins) -> Self {
        self.margins = margins;
        self
    }

    /// Replace the body font size in points.
    #[must_use]
    pub const fn with_font_size(mut self, font_size: f64) -> Self {
        self.font_size = font_size;
        self
    }

    /// Replace the line-height multiplier.
    #[must_use]
    pub const fn with_line_height(mut self, factor: f64) -> Self {
        self.line_height_factor = factor;
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
        self.line_height_factor
    }

    /// Validate the geometry and typography settings.
    ///
    /// # Errors
    ///
    /// Returns a typed error when the page size, margins, font size, or line
    /// height is invalid, or when the margins leave no usable content area.
    pub fn validate(&self) -> Result<(), CreateError> {
        PageSize::new(self.page_size.width, self.page_size.height)?;
        Margins::new(
            self.margins.left,
            self.margins.top,
            self.margins.right,
            self.margins.bottom,
        )?;
        if !self.font_size.is_finite() || self.font_size <= 0.0 || self.font_size > 144.0 {
            return Err(CreateError::InvalidOption(
                "font size must be in (0, 144]".to_owned(),
            ));
        }
        if !self.line_height_factor.is_finite() || !(1.0..=10.0).contains(&self.line_height_factor)
        {
            return Err(CreateError::InvalidOption(
                "line height must be in [1, 10]".to_owned(),
            ));
        }
        if self.page_size.width - self.margins.left - self.margins.right < 1.0 {
            return Err(CreateError::InvalidMargins(
                "left and right margins leave no horizontal space".to_owned(),
            ));
        }
        if self.page_size.height - self.margins.top - self.margins.bottom < 1.0 {
            return Err(CreateError::InvalidMargins(
                "top and bottom margins leave no vertical space".to_owned(),
            ));
        }
        Ok(())
    }
}

impl Default for CreateOptions {
    fn default() -> Self {
        Self::new()
    }
}

/// A raster input format that can be embedded without a decoder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CreateImageFormat {
    /// A Portable Network Graphics file.
    Png,
    /// A Joint Photographic Experts Group file.
    Jpeg,
}

impl CreateImageFormat {
    /// Classify a file-name extension, ignoring an optional leading dot and
    /// ASCII case.
    #[must_use]
    pub fn from_extension(extension: &str) -> Option<Self> {
        let ext = extension.strip_prefix('.').unwrap_or(extension);
        match ext.to_ascii_lowercase().as_str() {
            "png" => Some(Self::Png),
            "jpg" | "jpeg" => Some(Self::Jpeg),
            _ => None,
        }
    }

    /// Return a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Png => "PNG",
            Self::Jpeg => "JPEG",
        }
    }
}

impl std::fmt::Display for CreateImageFormat {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(self.label())
    }
}

/// Create a PDF from a named input file's bytes, routing by extension.
///
/// `.txt`, `.text` and `.log` are plain text; `.md` and `.markdown` use the
/// Markdown parser; `.png`, `.jpg` and `.jpeg` are embedded as images. The file
/// is not opened by this function: callers that need a path-based API can read
/// the bytes themselves and use [`CreateError::Io`] for diagnostics, or use
/// [`create_pdf_from_file`].
///
/// # Errors
///
/// Returns [`CreateError::UnsupportedSourceExtension`] for an unknown
/// extension, [`CreateError::InvalidOption`] when text bytes are not UTF-8, and
/// any error from the selected creation function.
pub fn create_pdf_from_path(
    path: &Path,
    input: &[u8],
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    let extension = path
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    match extension.as_str() {
        "txt" | "text" | "log" => {
            let text = std::str::from_utf8(input).map_err(|error| {
                CreateError::InvalidOption(format!("text input is not UTF-8: {error}"))
            })?;
            create_from_text(text, options)
        }
        "md" | "markdown" => {
            let text = std::str::from_utf8(input).map_err(|error| {
                CreateError::InvalidOption(format!("Markdown input is not UTF-8: {error}"))
            })?;
            create_from_markdown(text, options)
        }
        "png" => create_from_image(input, CreateImageFormat::Png, options),
        "jpg" | "jpeg" => create_from_image(input, CreateImageFormat::Jpeg, options),
        other => Err(CreateError::UnsupportedSourceExtension {
            extension: other.to_owned(),
        }),
    }
}

/// Read `path` and create a PDF from its bytes, routing by extension.
///
/// # Errors
///
/// Returns [`CreateError::Io`] when the file cannot be read and otherwise the
/// same errors as [`create_pdf_from_path`].
pub fn create_pdf_from_file(path: &Path, options: &CreateOptions) -> Result<Vec<u8>, CreateError> {
    let input = std::fs::read(path)?;
    create_pdf_from_path(path, &input, options)
}

/// Create a PDF from plain text, preserving input line breaks and wrapping long
/// lines to the content width.
///
/// Whitespace-only input yields [`CreateError::EmptyDocument`].
///
/// # Errors
///
/// Returns [`CreateError::EmptyDocument`] for blank input and any error from
/// [`CreateOptions::validate`].
pub fn create_from_text(text: &str, options: &CreateOptions) -> Result<Vec<u8>, CreateError> {
    options.validate()?;
    let mut lines = Vec::new();
    for source_line in text.lines() {
        lines.push(StyledLine::regular(
            encode_text(source_line),
            options.font_size,
        ));
    }
    if lines
        .iter()
        .all(|line| line.bytes.iter().all(|byte| byte.is_ascii_whitespace()))
    {
        return Err(CreateError::EmptyDocument);
    }
    Ok(build_text_pdf(&lines, options))
}

/// Create a PDF from the documented Markdown subset.
///
/// See the [module documentation](crate::create) for the supported constructs.
/// Whitespace-only input yields [`CreateError::EmptyDocument`].
///
/// # Errors
///
/// Returns [`CreateError::EmptyDocument`] for blank input and any error from
/// [`CreateOptions::validate`].
pub fn create_from_markdown(
    markdown: &str,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    options.validate()?;
    let lines = parse_markdown(markdown, options);
    if lines
        .iter()
        .all(|line| line.bytes.iter().all(|byte| byte.is_ascii_whitespace()))
    {
        return Err(CreateError::EmptyDocument);
    }
    Ok(build_text_pdf(&lines, options))
}

/// Create a one-page PDF that embeds a PNG or JPEG image.
///
/// The image bytes are embedded verbatim (no pixel decoding). See the
/// [module documentation](crate::create) for DPI and page-fit rules.
///
/// # Errors
///
/// Returns [`CreateError::MalformedImage`] for invalid bytes,
/// [`CreateError::UnsupportedImageFeature`] for valid images using unsupported
/// features, and any error from [`CreateOptions::validate`].
pub fn create_from_image(
    bytes: &[u8],
    format: CreateImageFormat,
    options: &CreateOptions,
) -> Result<Vec<u8>, CreateError> {
    options.validate()?;
    let source = match format {
        CreateImageFormat::Png => parse_png(bytes)?,
        CreateImageFormat::Jpeg => parse_jpeg(bytes)?,
    };
    Ok(build_image_pdf(&source, options))
}

// ---------------------------------------------------------------------------
// Font metrics and text encoding
// ---------------------------------------------------------------------------

/// Helvetica AFM advance widths for ASCII 0x20 through 0x7E, in 1/1000 em.
const HELVETICA_WIDTHS: [u16; 95] = [
    278, 278, 355, 556, 556, 889, 667, 191, 333, 333, //  ! " # $ % & ' ( )
    389, 584, 278, 333, 278, 278, 556, 556, 556, 556, // * + , - . / 0 1 2 3
    556, 556, 556, 556, 556, 556, 278, 278, 584, 584, 584, // 4 5 6 7 8 9 : ; < = >
    556, 1015, 667, 667, 722, 722, 667, 611, 778, 722, // ? @ A B C D E F G H
    278, 500, 667, 556, 833, 722, 778, 667, 778, 722, // I J K L M N O P Q R
    667, 611, 722, 667, 944, 667, 667, 611, 278, 278, // S T U V W X Y Z [ \
    278, 469, 556, 333, 556, 556, 500, 556, 556, 278, // ] ^ _ ` a b c d e f
    556, 556, 222, 222, 500, 222, 833, 556, 556, 556, // g h i j k l m n o p
    556, 333, 500, 278, 556, 500, 722, 500, 500, 500, // q r s t u v w x y z
    334, 260, 334, 584, // { | } ~
];

/// Courier is monospaced; every glyph advances 600/1000 em.
const COURIER_WIDTH: u16 = 600;

/// The PDF resource family used for a line.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum FontKind {
    Regular,
    Bold,
    Mono,
}

impl FontKind {
    /// Name of the font resource on the page.
    const fn resource(self) -> &'static str {
        match self {
            Self::Regular => "F1",
            Self::Bold => "F2",
            Self::Mono => "F3",
        }
    }
}

/// Map one character to a WinAnsi/Latin-1 byte.
fn encoded_byte(character: char) -> u8 {
    match character {
        '\u{20AC}' => 0x80, // euro
        '\u{2018}' => 0x91, // left single quote
        '\u{2019}' => 0x92, // right single quote
        '\u{201C}' => 0x93, // left double quote
        '\u{201D}' => 0x94, // right double quote
        '\u{2022}' => 0x95, // bullet
        '\u{2013}' => 0x96, // en dash
        '\u{2014}' => 0x97, // em dash
        value if (value as u32) <= 0xFF => value as u8,
        _ => b'?',
    }
}

/// Encode a string as WinAnsi/Latin-1 bytes for a PDF literal string.
fn encode_text(text: &str) -> Vec<u8> {
    text.chars().map(encoded_byte).collect()
}

/// Advance width of one encoded byte at `size` points.
fn byte_width(byte: u8, size: f64, mono: bool) -> f64 {
    let units = if mono {
        COURIER_WIDTH
    } else {
        match byte.checked_sub(0x20) {
            Some(index) => HELVETICA_WIDTHS
                .get(usize::from(index))
                .copied()
                .unwrap_or(556),
            None => 556,
        }
    };
    f64::from(units) / 1000.0 * size
}

/// Total advance width of an encoded byte slice at `size` points.
fn text_width(bytes: &[u8], size: f64, mono: bool) -> f64 {
    bytes.iter().map(|&byte| byte_width(byte, size, mono)).sum()
}

/// Escape bytes for inclusion in a PDF literal string.
///
/// `(` and `)` and the backslash are backslash-escaped; printable ASCII is
/// emitted verbatim; a few common control bytes use short escapes; all other
/// bytes use exact three-digit octal escapes.
fn pdf_escape(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() + 8);
    for &byte in bytes {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            0x20..=0x7E => out.push(byte),
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            _ => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
        }
    }
    out
}

/// A single styled input line, already encoded to literal-string bytes.
#[derive(Debug, Clone)]
struct StyledLine {
    bytes: Vec<u8>,
    font: FontKind,
    size: f64,
    indent: f64,
    wrap: bool,
}

impl StyledLine {
    fn regular(bytes: Vec<u8>, size: f64) -> Self {
        Self {
            bytes,
            font: FontKind::Regular,
            size,
            indent: 0.0,
            wrap: true,
        }
    }
}

/// A positioned line ready for a page content stream.
#[derive(Debug, Clone)]
struct PlacedLine {
    bytes: Vec<u8>,
    font: FontKind,
    size: f64,
    x: f64,
    y: f64,
}

/// Greedy word wrap over encoded bytes, breaking words longer than `max_width`.
fn wrap_bytes(bytes: &[u8], max_width: f64, size: f64, mono: bool) -> Vec<Vec<u8>> {
    if bytes.is_empty() {
        return Vec::new();
    }
    let space_width = byte_width(b' ', size, mono);
    let mut lines: Vec<Vec<u8>> = Vec::new();
    let mut current: Vec<u8> = Vec::new();
    let mut current_width = 0.0;

    for word in bytes.split(|&byte| byte == b' ') {
        if word.is_empty() {
            continue;
        }
        let word_width = text_width(word, size, mono);
        let separator = if current.is_empty() { 0.0 } else { space_width };

        if current_width + separator + word_width <= max_width {
            if !current.is_empty() {
                current.push(b' ');
                current_width += separator;
            }
            current.extend_from_slice(word);
            current_width += word_width;
            continue;
        }

        if !current.is_empty() {
            lines.push(std::mem::take(&mut current));
            current_width = 0.0;
        }

        if word_width <= max_width {
            current.extend_from_slice(word);
            current_width = word_width;
            continue;
        }

        // A single word wider than the content area is split by byte.
        for &byte in word {
            let width = byte_width(byte, size, mono);
            if current_width + width > max_width && !current.is_empty() {
                lines.push(std::mem::take(&mut current));
                current_width = 0.0;
            }
            current.push(byte);
            current_width += width;
        }
    }

    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(bytes.to_vec());
    }
    lines
}

/// Lay styled lines out into pages of positioned lines.
fn layout_lines(lines: &[StyledLine], options: &CreateOptions) -> Vec<Vec<PlacedLine>> {
    let content_width = options.page_size.width - options.margins.left - options.margins.right;
    let mut pages: Vec<Vec<PlacedLine>> = Vec::new();
    let mut current: Vec<PlacedLine> = Vec::new();
    let mut y = options.page_size.height - options.margins.top;

    for line in lines {
        let available = (content_width - line.indent).max(1.0);
        let visual_lines = if line.wrap && !line.bytes.is_empty() {
            wrap_bytes(
                &line.bytes,
                available,
                line.size,
                line.font == FontKind::Mono,
            )
        } else if line.bytes.is_empty() {
            vec![Vec::new()]
        } else {
            vec![line.bytes.clone()]
        };

        for segment in visual_lines {
            let line_height = line.size * options.line_height_factor;
            if y - line_height < options.margins.bottom && !current.is_empty() {
                pages.push(std::mem::take(&mut current));
                y = options.page_size.height - options.margins.top;
            }
            // `y` is the top of the line box; the baseline sits one font size
            // below it. Clamp so the baseline never falls below the margin.
            let baseline = (y - line.size).max(options.margins.bottom);
            current.push(PlacedLine {
                bytes: segment,
                font: line.font,
                size: line.size,
                x: options.margins.left + line.indent,
                y: baseline,
            });
            y -= line_height;
        }
    }

    pages.push(current);
    pages
}

/// Render positioned lines as a PDF content stream.
fn render_text_content(lines: &[PlacedLine]) -> Vec<u8> {
    let mut out = Vec::new();
    out.extend_from_slice(b"BT\n");
    for line in lines {
        if line.bytes.is_empty() {
            continue;
        }
        out.extend_from_slice(
            format!(
                "/{} {:.2} Tf\n1 0 0 1 {:.2} {:.2} Tm\n",
                line.font.resource(),
                line.size,
                line.x,
                line.y
            )
            .as_bytes(),
        );
        out.push(b'(');
        out.extend_from_slice(&pdf_escape(&line.bytes));
        // A literal newline escape ends each shown string. It is invisible in
        // a viewer and gives text extractors one line per visual line.
        out.extend_from_slice(b"\\n) Tj\n");
    }
    out.extend_from_slice(b"ET\n");
    out
}

// ---------------------------------------------------------------------------
// Markdown parsing
// ---------------------------------------------------------------------------

/// Scale applied to the body font size for a heading level (1 through 6).
fn heading_scale(level: usize) -> f64 {
    match level {
        1 => 2.0,
        2 => 1.6,
        3 => 1.35,
        4 => 1.15,
        5 => 1.05,
        _ => 1.0,
    }
}

/// Return `(level, content)` when `line` is an ATX heading.
fn split_heading(line: &str) -> Option<(usize, &str)> {
    let hashes = line.bytes().take_while(|&byte| byte == b'#').count();
    if hashes == 0 || hashes > 6 {
        return None;
    }
    let rest = line.get(hashes..)?;
    if !rest.starts_with(' ') {
        return None;
    }
    Some((hashes, rest.trim()))
}

/// Return the item text when `line` is a Markdown bullet.
fn bullet_content(line: &str) -> Option<&str> {
    let bytes = line.as_bytes();
    if bytes.len() >= 2 && matches!(bytes[0], b'-' | b'*' | b'+') && bytes[1] == b' ' {
        Some(line.get(2..)?.trim())
    } else {
        None
    }
}

/// Parse the supported Markdown subset into styled lines.
fn parse_markdown(markdown: &str, options: &CreateOptions) -> Vec<StyledLine> {
    fn flush_paragraph(out: &mut Vec<StyledLine>, paragraph: &mut Vec<String>, size: f64) {
        if paragraph.is_empty() {
            return;
        }
        let joined = paragraph.join(" ");
        out.push(StyledLine::regular(encode_text(&joined), size));
        paragraph.clear();
    }

    let mut out = Vec::new();
    let mut paragraph: Vec<String> = Vec::new();
    let mut in_code = false;
    let mut code_lines: Vec<String> = Vec::new();
    let code_size = options.font_size * 0.9;

    for source_line in markdown.lines() {
        let trimmed_start = source_line.trim_start();
        if trimmed_start.starts_with("```") {
            if in_code {
                for code_line in &code_lines {
                    out.push(StyledLine {
                        bytes: encode_text(code_line),
                        font: FontKind::Mono,
                        size: code_size,
                        indent: 18.0,
                        wrap: false,
                    });
                }
                code_lines.clear();
                in_code = false;
            } else {
                flush_paragraph(&mut out, &mut paragraph, options.font_size);
                in_code = true;
            }
            continue;
        }
        if in_code {
            code_lines.push(source_line.to_owned());
            continue;
        }

        let trimmed = source_line.trim();
        if trimmed.is_empty() {
            flush_paragraph(&mut out, &mut paragraph, options.font_size);
            continue;
        }
        if let Some((level, heading)) = split_heading(trimmed) {
            flush_paragraph(&mut out, &mut paragraph, options.font_size);
            out.push(StyledLine {
                bytes: encode_text(heading),
                font: FontKind::Bold,
                size: options.font_size * heading_scale(level),
                indent: 0.0,
                wrap: true,
            });
            continue;
        }
        if let Some(item) = bullet_content(trimmed) {
            flush_paragraph(&mut out, &mut paragraph, options.font_size);
            out.push(StyledLine {
                bytes: encode_text(&format!("- {item}")),
                font: FontKind::Regular,
                size: options.font_size,
                indent: 18.0,
                wrap: true,
            });
            continue;
        }
        paragraph.push(trimmed.to_owned());
    }

    flush_paragraph(&mut out, &mut paragraph, options.font_size);
    if in_code {
        for code_line in &code_lines {
            out.push(StyledLine {
                bytes: encode_text(code_line),
                font: FontKind::Mono,
                size: code_size,
                indent: 18.0,
                wrap: false,
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Minimal deterministic PDF writer
// ---------------------------------------------------------------------------

/// Accumulates indirect objects and emits a classic-xref PDF 1.4 file.
#[derive(Debug, Default)]
struct PdfWriter {
    objects: Vec<Vec<u8>>,
}

impl PdfWriter {
    fn new() -> Self {
        Self::default()
    }

    /// Allocate an object number and reserve its slot.
    fn reserve(&mut self) -> u32 {
        self.objects.push(Vec::new());
        self.objects.len() as u32
    }

    /// Fill a previously reserved slot.
    fn set(&mut self, id: u32, body: Vec<u8>) {
        let index = usize::try_from(id).unwrap_or_default().saturating_sub(1);
        if let Some(slot) = self.objects.get_mut(index) {
            *slot = body;
        }
    }

    /// Serialize the document with `root` as the catalog object number.
    fn finish(&self, root: u32) -> Vec<u8> {
        let mut out = b"%PDF-1.4\n%\xE2\xE3\xCF\xD3\n".to_vec();
        let mut offsets = Vec::with_capacity(self.objects.len() + 1);
        for (index, body) in self.objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref_offset = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", self.objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root {} 0 R >>\nstartxref\n{}\n%%EOF\n",
                self.objects.len() + 1,
                root,
                xref_offset
            )
            .as_bytes(),
        );
        out
    }
}

/// The five objects shared by every generated document.
type CommonIds = (u32, u32, u32, u32, u32);

/// Reserve the catalog, page tree, and three font objects shared by all pages.
fn reserve_common(writer: &mut PdfWriter) -> CommonIds {
    (
        writer.reserve(), // catalog
        writer.reserve(), // pages
        writer.reserve(), // F1 Helvetica
        writer.reserve(), // F2 Helvetica-Bold
        writer.reserve(), // F3 Courier
    )
}

/// Write the shared catalog, page tree, and font objects.
fn write_common(writer: &mut PdfWriter, ids: CommonIds, page_ids: &[u32]) {
    let (catalog, pages, f1, f2, f3) = ids;
    writer.set(
        catalog,
        format!("<< /Type /Catalog /Pages {pages} 0 R >>").into_bytes(),
    );
    let kids = page_ids
        .iter()
        .map(|id| format!("{id} 0 R"))
        .collect::<Vec<_>>()
        .join(" ");
    writer.set(
        pages,
        format!(
            "<< /Type /Pages /Kids [ {kids} ] /Count {} >>",
            page_ids.len()
        )
        .into_bytes(),
    );
    writer.set(
        f1,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    writer.set(
        f2,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica-Bold /Encoding /WinAnsiEncoding >>"
            .to_vec(),
    );
    writer.set(
        f3,
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Courier /Encoding /WinAnsiEncoding >>".to_vec(),
    );
}

/// Font resource fragment shared by every page.
fn font_resources(f1: u32, f2: u32, f3: u32) -> String {
    format!("/Font << /F1 {f1} 0 R /F2 {f2} 0 R /F3 {f3} 0 R >>")
}

/// Wrap `data` in a stream object with an exact `/Length`.
fn stream_object(data: &[u8]) -> Vec<u8> {
    let mut body = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    body.extend_from_slice(data);
    body.extend_from_slice(b"\nendstream");
    body
}

/// Page dictionary body for the given geometry and resources.
fn page_object(
    pages_id: u32,
    options: &CreateOptions,
    resources: &str,
    content_id: u32,
) -> Vec<u8> {
    format!(
        "<< /Type /Page /Parent {pages_id} 0 R /MediaBox [0 0 {:.2} {:.2}] \
         /Resources << {resources} >> /Contents {content_id} 0 R >>",
        options.page_size.width, options.page_size.height
    )
    .into_bytes()
}

/// Build a text/Markdown PDF from styled lines.
fn build_text_pdf(lines: &[StyledLine], options: &CreateOptions) -> Vec<u8> {
    let pages = layout_lines(lines, options);
    let mut writer = PdfWriter::new();
    let common = reserve_common(&mut writer);
    let (catalog, pages_id, f1, f2, f3) = common;

    let mut page_ids = Vec::with_capacity(pages.len());
    let mut planned = Vec::with_capacity(pages.len());
    for page_lines in &pages {
        let page_id = writer.reserve();
        let content_id = writer.reserve();
        page_ids.push(page_id);
        planned.push((page_id, content_id, render_text_content(page_lines)));
    }
    write_common(&mut writer, common, &page_ids);

    for (page_id, content_id, content) in planned {
        let resources = font_resources(f1, f2, f3);
        writer.set(
            page_id,
            page_object(pages_id, options, &resources, content_id),
        );
        writer.set(content_id, stream_object(&content));
    }
    writer.finish(catalog)
}

// ---------------------------------------------------------------------------
// Image parsing and embedding
// ---------------------------------------------------------------------------

/// The embedded representation of an image, independent of its source format.
struct ImageSource {
    width: u32,
    height: u32,
    dpi: Option<f64>,
    stream_dictionary: String,
    data: Vec<u8>,
}

/// Read a big-endian u32 from the first four bytes of `slice`.
fn be32(slice: &[u8]) -> u32 {
    u32::from_be_bytes([slice[0], slice[1], slice[2], slice[3]])
}

/// Read a big-endian u16 from the first two bytes of `slice`.
fn be16(slice: &[u8]) -> u16 {
    u16::from_be_bytes([slice[0], slice[1]])
}

/// Maximum combined `IDAT` payload accepted (256 MiB).
const MAX_IMAGE_STREAM: usize = 256 * 1024 * 1024;

fn malformed(format: &'static str, message: &str) -> CreateError {
    CreateError::MalformedImage {
        format,
        message: message.to_owned(),
    }
}

fn unsupported(format: &'static str, message: &str) -> CreateError {
    CreateError::UnsupportedImageFeature {
        format,
        message: message.to_owned(),
    }
}

/// Parse the metadata and compressed payload of a PNG file.
fn parse_png(bytes: &[u8]) -> Result<ImageSource, CreateError> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 8 || bytes.get(..8) != Some(SIGNATURE.as_slice()) {
        return Err(malformed("PNG", "bad or missing signature"));
    }

    let mut position = 8usize;
    let mut header: Option<(u32, u32, u8, u8, u8, u8, u8)> = None;
    let mut idat: Vec<u8> = Vec::new();
    let mut dpi = None;
    let mut saw_end = false;

    loop {
        if bytes.len().saturating_sub(position) < 8 {
            break;
        }
        let length = be32(&bytes[position..position + 4]) as usize;
        let kind = [
            bytes[position + 4],
            bytes[position + 5],
            bytes[position + 6],
            bytes[position + 7],
        ];
        let data_start = match position.checked_add(8) {
            Some(value) => value,
            None => return Err(malformed("PNG", "chunk offset overflow")),
        };
        let data_end = match data_start.checked_add(length) {
            Some(value) => value,
            None => return Err(malformed("PNG", "chunk length overflow")),
        };
        let chunk_end = match data_end.checked_add(4) {
            Some(value) => value,
            None => return Err(malformed("PNG", "chunk checksum overflow")),
        };
        if chunk_end > bytes.len() {
            return Err(malformed("PNG", "truncated chunk data"));
        }
        let data = &bytes[data_start..data_end];

        match &kind {
            b"IHDR" => {
                if data.len() != 13 {
                    return Err(malformed("PNG", "IHDR must be 13 bytes"));
                }
                header = Some((
                    be32(&data[0..4]),
                    be32(&data[4..8]),
                    data[8],
                    data[9],
                    data[10],
                    data[11],
                    data[12],
                ));
            }
            b"IDAT" => {
                if idat
                    .len()
                    .checked_add(data.len())
                    .is_none_or(|len| len > MAX_IMAGE_STREAM)
                {
                    return Err(malformed("PNG", "IDAT payload too large"));
                }
                idat.extend_from_slice(data);
            }
            b"pHYs" => {
                if data.len() == 9 && data[8] == 1 {
                    let pixels_per_metre = be32(&data[0..4]);
                    if pixels_per_metre > 0 {
                        dpi = Some(f64::from(pixels_per_metre) * 0.0254);
                    }
                }
            }
            b"IEND" => {
                saw_end = true;
                break;
            }
            _ => {}
        }
        position = chunk_end;
    }

    let (width, height, bit_depth, color_type, compression, filter, interlace) =
        header.ok_or_else(|| malformed("PNG", "missing IHDR chunk"))?;
    if !saw_end {
        return Err(malformed("PNG", "missing IEND chunk"));
    }
    if width == 0 || height == 0 {
        return Err(malformed("PNG", "zero image dimension"));
    }
    if idat.is_empty() {
        return Err(malformed("PNG", "no IDAT payload"));
    }
    if bit_depth != 8 {
        return Err(unsupported("PNG", "only 8-bit samples are supported"));
    }
    if compression != 0 || filter != 0 {
        return Err(malformed("PNG", "unknown compression or filter method"));
    }
    if interlace != 0 {
        return Err(unsupported("PNG", "interlaced images are not supported"));
    }
    if color_type != 0 && color_type != 2 {
        return Err(unsupported(
            "PNG",
            "only grayscale and RGB images without alpha are supported",
        ));
    }

    let (colors, color_space) = if color_type == 0 {
        (1u8, "/DeviceGray")
    } else {
        (3u8, "/DeviceRGB")
    };
    // The exact IDAT payload is what gets written between `stream\n` and
    // `\nendstream`, so it is also the mandatory /Length of this image stream.
    let stream_length = idat.len();
    let stream_dictionary = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace {color_space} /BitsPerComponent 8 /Filter /FlateDecode \
         /Length {stream_length} /DecodeParms << /Predictor 15 /Colors {colors} \
         /BitsPerComponent 8 /Columns {width} >> >>"
    );

    Ok(ImageSource {
        width,
        height,
        dpi,
        stream_dictionary,
        data: idat,
    })
}

/// Return `true` for a JPEG start-of-frame marker (excluding DHT/JPG/DAC).
fn is_start_of_frame(marker: u8) -> bool {
    matches!(
        marker,
        0xC0..=0xC3 | 0xC5..=0xC7 | 0xC9..=0xCB | 0xCD..=0xCF
    )
}

/// Parse the metadata of a JPEG file; the compressed bytes are embedded whole.
fn parse_jpeg(bytes: &[u8]) -> Result<ImageSource, CreateError> {
    if bytes.len() < 4 || bytes.get(..2) != Some([0xFF, 0xD8].as_slice()) {
        return Err(malformed("JPEG", "bad or missing SOI marker"));
    }
    let mut position = 2usize;
    let mut dimensions: Option<(u16, u16, u8, u8)> = None;
    let mut dpi = None;

    while position < bytes.len() {
        while bytes.get(position) == Some(&0xFF) {
            position += 1;
        }
        let marker = match bytes.get(position) {
            Some(marker) => *marker,
            None => break,
        };
        position += 1;
        if marker == 0x00 || marker == 0x01 || marker == 0xD8 {
            continue;
        }
        if marker == 0xD9 || marker == 0xDA {
            break;
        }
        let length_slice = match bytes.get(position..position + 2) {
            Some(slice) if slice.len() == 2 => slice,
            _ => return Err(malformed("JPEG", "truncated segment length")),
        };
        let length = usize::from(be16(length_slice));
        if length < 2 {
            return Err(malformed("JPEG", "invalid segment length"));
        }
        let segment_start = match position.checked_add(2) {
            Some(value) => value,
            None => return Err(malformed("JPEG", "segment offset overflow")),
        };
        let segment_end = match segment_start.checked_add(length - 2) {
            Some(value) => value,
            None => return Err(malformed("JPEG", "segment length overflow")),
        };
        if segment_end > bytes.len() {
            return Err(malformed("JPEG", "truncated segment data"));
        }
        let segment = &bytes[segment_start..segment_end];

        if is_start_of_frame(marker) {
            if segment.len() < 6 {
                return Err(malformed("JPEG", "short SOF segment"));
            }
            dimensions = Some((
                be16(&segment[3..5]),
                be16(&segment[1..3]),
                segment[5],
                segment[0],
            ));
        } else if marker == 0xE0 && segment.len() >= 12 && segment.get(..5) == Some(b"JFIF\0") {
            let units = segment[7];
            let density_x = be16(&segment[8..10]);
            if units == 1 && density_x > 0 {
                dpi = Some(f64::from(density_x));
            } else if units == 2 && density_x > 0 {
                dpi = Some(f64::from(density_x) * 2.54);
            }
        }
        position = segment_end;
    }

    let (width, height, components, precision) =
        dimensions.ok_or_else(|| malformed("JPEG", "missing SOF marker"))?;
    if width == 0 || height == 0 {
        return Err(malformed("JPEG", "zero image dimension"));
    }
    if precision != 8 {
        return Err(unsupported("JPEG", "only 8-bit precision is supported"));
    }
    if components != 1 && components != 3 {
        return Err(unsupported(
            "JPEG",
            "only grayscale and RGB component counts are supported",
        ));
    }
    let color_space = if components == 1 {
        "/DeviceGray"
    } else {
        "/DeviceRGB"
    };
    // The whole JPEG file is embedded verbatim between `stream\n` and
    // `\nendstream`; its byte count is the mandatory /Length of the stream.
    let stream_length = bytes.len();
    let stream_dictionary = format!(
        "<< /Type /XObject /Subtype /Image /Width {width} /Height {height} \
         /ColorSpace {color_space} /BitsPerComponent 8 /Filter /DCTDecode \
         /Length {stream_length} >>"
    );
    Ok(ImageSource {
        width: u32::from(width),
        height: u32::from(height),
        dpi,
        stream_dictionary,
        data: bytes.to_vec(),
    })
}

/// Build a one-page PDF that places an image inside the content area.
fn build_image_pdf(source: &ImageSource, options: &CreateOptions) -> Vec<u8> {
    let dpi = source
        .dpi
        .filter(|value| value.is_finite() && *value > 0.0)
        .unwrap_or(DEFAULT_DPI);
    let native_width = f64::from(source.width) / dpi * 72.0;
    let native_height = f64::from(source.height) / dpi * 72.0;
    let area_width = options.page_size.width - options.margins.left - options.margins.right;
    let area_height = options.page_size.height - options.margins.top - options.margins.bottom;
    let scale = (area_width / native_width)
        .min(area_height / native_height)
        .min(1.0);
    let draw_width = native_width * scale;
    let draw_height = native_height * scale;
    let x = options.margins.left + (area_width - draw_width) / 2.0;
    let y = options.margins.bottom + (area_height - draw_height) / 2.0;

    let content =
        format!("q {draw_width:.2} 0 0 {draw_height:.2} {x:.2} {y:.2} cm /Im0 Do Q\n").into_bytes();

    let mut writer = PdfWriter::new();
    let common = reserve_common(&mut writer);
    let (catalog, pages_id, f1, f2, f3) = common;
    let page_id = writer.reserve();
    let content_id = writer.reserve();
    let image_id = writer.reserve();

    write_common(&mut writer, common, &[page_id]);
    let resources = format!(
        "{} /XObject << /Im0 {image_id} 0 R >>",
        font_resources(f1, f2, f3)
    );
    writer.set(
        page_id,
        page_object(pages_id, options, &resources, content_id),
    );
    writer.set(content_id, stream_object(&content));
    let mut image_body = format!("{}\nstream\n", source.stream_dictionary).into_bytes();
    image_body.extend_from_slice(&source.data);
    image_body.extend_from_slice(b"\nendstream");
    writer.set(image_id, image_body);
    writer.finish(catalog)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Build a minimal valid non-interlaced RGB PNG with a pHYs chunk.
    fn rgb_png(width: u32, height: u32, dpi: Option<u32>) -> Vec<u8> {
        fn crc(data: &[u8]) -> u32 {
            let mut value = 0xFFFF_FFFFu32;
            for &byte in data {
                value ^= u32::from(byte);
                for _ in 0..8 {
                    let mask = 0u32.wrapping_sub(value & 1);
                    value = (value >> 1) ^ (0xEDB8_8320 & mask);
                }
            }
            value ^ 0xFFFF_FFFF
        }
        fn adler(data: &[u8]) -> u32 {
            let (mut a, mut b) = (1u32, 0u32);
            for &byte in data {
                a = (a + u32::from(byte)) % 65_521;
                b = (b + a) % 65_521;
            }
            (b << 16) | a
        }
        fn chunk(out: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
            out.extend_from_slice(&(data.len() as u32).to_be_bytes());
            out.extend_from_slice(kind);
            out.extend_from_slice(data);
            let mut checked = Vec::from(*kind);
            checked.extend_from_slice(data);
            out.extend_from_slice(&crc(&checked).to_be_bytes());
        }

        let mut ihdr = Vec::with_capacity(13);
        ihdr.extend_from_slice(&width.to_be_bytes());
        ihdr.extend_from_slice(&height.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        let row_len = (width as usize) * 3 + 1;
        let raw_len = row_len * (height as usize);
        let raw = vec![0u8; raw_len];
        let mut z = vec![0x78, 0x01, 0x01];
        z.extend_from_slice(&(raw_len as u16).to_le_bytes());
        z.extend_from_slice(&(!(raw_len as u16)).to_le_bytes());
        z.extend_from_slice(&raw);
        z.extend_from_slice(&adler(&raw).to_be_bytes());

        let mut out = vec![0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
        chunk(&mut out, b"IHDR", &ihdr);
        if let Some(dpi) = dpi {
            let ppm = ((f64::from(dpi) / 0.0254).round() as u32).to_be_bytes();
            let mut phys = Vec::from(ppm);
            phys.extend_from_slice(&ppm);
            phys.push(1);
            chunk(&mut out, b"pHYs", &phys);
        }
        chunk(&mut out, b"IDAT", &z);
        chunk(&mut out, b"IEND", &[]);
        out
    }

    /// Build a small JPEG-shaped byte buffer with a JFIF density marker.
    fn jpeg(width: u16, height: u16, dpi: Option<u16>) -> Vec<u8> {
        let mut out = vec![0xFF, 0xD8];
        if let Some(dpi) = dpi {
            let mut app0 = Vec::from(&b"JFIF\0"[..]);
            app0.extend_from_slice(&[1, 1]);
            app0.extend_from_slice(&dpi.to_be_bytes());
            app0.extend_from_slice(&dpi.to_be_bytes());
            app0.extend_from_slice(&[0, 0]);
            out.push(0xFF);
            out.push(0xE0);
            out.extend_from_slice(&((app0.len() + 2) as u16).to_be_bytes());
            out.extend_from_slice(&app0);
        }
        let mut sof = vec![8];
        sof.extend_from_slice(&height.to_be_bytes());
        sof.extend_from_slice(&width.to_be_bytes());
        sof.extend_from_slice(&[3, 1, 0x11, 0, 2, 0x11, 1, 0x11, 1]);
        out.push(0xFF);
        out.push(0xC0);
        out.extend_from_slice(&((sof.len() + 2) as u16).to_be_bytes());
        out.extend_from_slice(&sof);
        out.extend_from_slice(&[0xFF, 0xDA, 0x00, 0x02, 0xFF, 0xD9]);
        out
    }

    fn as_text(pdf: &[u8]) -> String {
        String::from_utf8_lossy(pdf).into_owned()
    }

    #[test]
    fn empty_text_returns_typed_error() {
        let error = create_from_text("", &CreateOptions::new()).expect_err("empty input");
        assert!(matches!(error, CreateError::EmptyDocument));
        assert!(error.to_string().contains("empty"));
    }

    #[test]
    fn whitespace_only_markdown_returns_typed_error() {
        let error = create_from_markdown("  \n\n\t\n", &CreateOptions::new())
            .expect_err("whitespace input");
        assert!(matches!(error, CreateError::EmptyDocument));
    }

    #[test]
    fn long_text_paginates_across_pages() {
        let text = (0..400)
            .map(|index| format!("line {index} with a few words"))
            .collect::<Vec<_>>()
            .join("\n");
        let options = CreateOptions::new();
        let pdf = create_from_text(&text, &options).expect("long text");
        let rendered = as_text(&pdf);
        let expected_lines: Vec<StyledLine> = text
            .lines()
            .map(|line| StyledLine::regular(encode_text(line), options.font_size))
            .collect();
        let expected_pages = layout_lines(&expected_lines, &options).len();
        assert!(expected_pages >= 2, "400 lines must span multiple pages");
        assert!(rendered.contains(&format!("/Count {expected_pages}")));
        assert_eq!(rendered.matches(" /Type /Page ").count(), expected_pages);
    }

    #[test]
    fn markdown_constructs_render_expected_fonts_and_bullets() {
        let markdown = "# Heading\n\nA paragraph.\n\n- first\n- second\n\n```\nlet x = 1;\n```\n";
        let pdf = create_from_markdown(markdown, &CreateOptions::new()).expect("markdown");
        let rendered = as_text(&pdf);
        assert!(rendered.contains("/F2 22.00 Tf"), "heading uses bold font");
        assert!(rendered.contains("/F3 9.90 Tf"), "code uses Courier");
        assert!(rendered.contains("(- first\\n)"), "bullet prefix");
        assert!(rendered.contains("let x = 1;"), "verbatim code line");
    }

    #[test]
    fn non_ascii_text_is_embedded_as_octal_escapes() {
        let pdf = create_from_text("café — naïve", &CreateOptions::new()).expect("accented text");
        let rendered = as_text(&pdf);
        assert!(rendered.contains("\\351"), "é is encoded as \\351");
        assert!(
            rendered.contains("\\227"),
            "em dash maps to Windows-1252 0x97"
        );
    }

    #[test]
    fn malformed_image_returns_typed_error() {
        let error = create_from_image(b"not a png", CreateImageFormat::Png, &CreateOptions::new())
            .expect_err("bad PNG");
        assert!(matches!(error, CreateError::MalformedImage { .. }));
        assert!(error.to_string().contains("PNG"));

        let error = create_from_image(
            b"not a jpeg",
            CreateImageFormat::Jpeg,
            &CreateOptions::new(),
        )
        .expect_err("bad JPEG");
        assert!(matches!(error, CreateError::MalformedImage { .. }));
    }

    #[test]
    fn deterministic_bytes_are_stable() {
        let options = CreateOptions::new().with_page_size(PageSize::A4);
        let first = create_from_markdown("# Title\n\nBody text.\n", &options).expect("first");
        let second = create_from_markdown("# Title\n\nBody text.\n", &options).expect("second");
        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.4"));
        assert!(first.ends_with(b"%%EOF\n"));
    }

    #[test]
    fn page_count_and_size_are_respected() {
        let options = CreateOptions::new().with_page_size(PageSize::A4);
        let pdf = create_from_text("hello", &options).expect("text");
        let rendered = as_text(&pdf);
        assert!(rendered.contains("/Count 1"));
        assert!(rendered.contains("/MediaBox [0 0 595.28 841.89]"));
        assert_eq!(rendered.matches("startxref").count(), 1);
    }

    #[test]
    fn margin_validation_rejects_impossible_geometry() {
        let options = CreateOptions::new()
            .with_margins(Margins::new(400.0, 10.0, 300.0, 10.0).expect("margins"));
        let error = create_from_text("hello", &options).expect_err("no horizontal space");
        assert!(matches!(error, CreateError::InvalidMargins(_)));

        let negative = Margins::new(-1.0, 0.0, 0.0, 0.0);
        assert!(matches!(negative, Err(CreateError::InvalidMargins(_))));
    }

    #[test]
    fn unsupported_extension_is_reported() {
        let error = create_pdf_from_path(Path::new("image.gif"), b"bytes", &CreateOptions::new())
            .expect_err("gif is not supported");
        assert!(matches!(
            error,
            CreateError::UnsupportedSourceExtension { ref extension } if extension == "gif"
        ));
        assert_eq!(
            CreateImageFormat::from_extension(".JPG"),
            Some(CreateImageFormat::Jpeg)
        );
        assert_eq!(CreateImageFormat::from_extension("bmp"), None);
    }

    #[test]
    fn png_is_embedded_with_flate_and_predictor() {
        let png = rgb_png(40, 20, Some(72));
        let pdf = create_from_image(&png, CreateImageFormat::Png, &CreateOptions::new())
            .expect("PNG embed");
        let rendered = as_text(&pdf);
        assert!(rendered.contains("/Filter /FlateDecode"));
        assert!(rendered.contains("/Predictor 15"));
        assert!(rendered.contains("/ColorSpace /DeviceRGB"));
        assert!(rendered.contains("/Subtype /Image"));
        assert!(rendered.contains("/Count 1"));
    }

    #[test]
    fn jpeg_is_embedded_with_dctdecode() {
        let image = jpeg(640, 480, Some(144));
        let pdf = create_from_image(&image, CreateImageFormat::Jpeg, &CreateOptions::new())
            .expect("JPEG embed");
        let rendered = as_text(&pdf);
        assert!(rendered.contains("/Filter /DCTDecode"));
        assert!(rendered.contains("/Width 640"));
        assert!(rendered.contains("/Height 480"));
    }

    #[test]
    fn unsupported_png_alpha_is_rejected() {
        let mut png = rgb_png(2, 2, None);
        // IHDR data begins at 8 (signature) + 8 (length/type); byte 9 is the
        // colour type. Rewrite RGB (2) to RGBA (6).
        png[8 + 8 + 9] = 6;
        let error = create_from_image(&png, CreateImageFormat::Png, &CreateOptions::new())
            .expect_err("alpha PNG");
        assert!(matches!(error, CreateError::UnsupportedImageFeature { .. }));
    }

    #[test]
    fn page_size_validation_rejects_invalid_dimensions() {
        assert!(matches!(
            PageSize::new(f64::NAN, 10.0),
            Err(CreateError::InvalidPageSize(_))
        ));
        assert!(matches!(
            PageSize::new(0.0, 10.0),
            Err(CreateError::InvalidPageSize(_))
        ));
    }

    #[test]
    fn pdf_writer_offsets_point_at_objects() {
        let pdf = create_from_text("offset test", &CreateOptions::new()).expect("pdf");
        let marker = b"startxref\n";
        let position = pdf
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("startxref marker");
        let digits_start = position + marker.len();
        let digits_end = pdf[digits_start..]
            .iter()
            .position(|byte| *byte == b'\n')
            .map(|offset| digits_start + offset)
            .expect("newline after startxref");
        let offset: usize = std::str::from_utf8(&pdf[digits_start..digits_end])
            .expect("numeric offset")
            .parse()
            .expect("parse offset");
        assert_eq!(&pdf[offset..offset + 4], b"xref");
    }

    #[test]
    fn escaped_text_round_trips_through_literal_string_lexer() {
        let pdf = create_from_text("(paren) \\ slash", &CreateOptions::new()).expect("escaped");
        let rendered = as_text(&pdf);
        assert!(
            rendered.contains("\\(paren\\) \\\\ slash"),
            "delimiters escaped"
        );
    }

    /// Read the positive integer following `/Length` in a dictionary slice.
    fn declared_length(dictionary: &[u8]) -> Option<usize> {
        let key = b"/Length";
        let at = dictionary
            .windows(key.len())
            .position(|window| window == key)?;
        let mut digits = Vec::new();
        for &byte in &dictionary[at + key.len()..] {
            if byte.is_ascii_whitespace() {
                if digits.is_empty() {
                    continue;
                }
                break;
            }
            if !byte.is_ascii_digit() {
                break;
            }
            digits.push(byte);
        }
        std::str::from_utf8(&digits).ok()?.parse().ok()
    }

    /// For every image XObject, assert that its dictionary declares a `/Length`
    /// equal to the exact bytes between `stream\n` and `\nendstream`.
    ///
    /// This parses the emitted PDF bytes directly rather than relying on the
    /// core parser's lenient missing-`/Length` fallback.
    fn image_stream_lengths(pdf: &[u8]) -> Vec<usize> {
        let marker = b"/Subtype /Image";
        let mut verified = Vec::new();
        let mut search = 0usize;
        while let Some(relative) = pdf[search..]
            .windows(marker.len())
            .position(|window| window == marker)
        {
            let image_at = search + relative;
            let dict_start = pdf[..image_at]
                .windows(2)
                .rposition(|window| window == b"<<")
                .expect("image dictionary opener");
            let dict_end = pdf[image_at..]
                .windows(2)
                .position(|window| window == b">>")
                .map(|offset| image_at + offset + 2)
                .expect("image dictionary closer");
            let length = declared_length(&pdf[dict_start..dict_end])
                .expect("image stream dictionary must declare /Length");
            let data_start = pdf[dict_end..]
                .windows(b"stream\n".len())
                .position(|window| window == b"stream\n")
                .map(|offset| dict_end + offset + b"stream\n".len())
                .expect("stream keyword");
            let data_len = pdf[data_start..]
                .windows(b"\nendstream".len())
                .position(|window| window == b"\nendstream")
                .expect("endstream keyword");
            assert_eq!(
                length, data_len,
                "image /Length {length} must equal the {data_len} embedded bytes"
            );
            assert!(data_len > 0, "image stream must not be empty");
            verified.push(data_len);
            search = dict_end;
        }
        verified
    }

    #[test]
    fn png_image_stream_declares_exact_length() {
        let png = rgb_png(40, 20, Some(72));
        let pdf = create_from_image(&png, CreateImageFormat::Png, &CreateOptions::new())
            .expect("PNG embed");
        let lengths = image_stream_lengths(&pdf);
        assert_eq!(lengths.len(), 1, "exactly one image XObject");
        assert_eq!(lengths[0], png_idat_payload_len(&png));
    }

    /// Sum the payload lengths of every PNG chunk with type `IDAT`.
    fn png_idat_payload_len(png: &[u8]) -> usize {
        let mut position = 8usize;
        let mut total = 0usize;
        while png.len().saturating_sub(position) >= 8 {
            let length = be32(&png[position..position + 4]) as usize;
            let kind = [
                png[position + 4],
                png[position + 5],
                png[position + 6],
                png[position + 7],
            ];
            let data_end = position + 8 + length;
            if kind == *b"IDAT" {
                total += length;
            }
            position = data_end + 4;
        }
        total
    }

    #[test]
    fn jpeg_image_stream_declares_exact_length() {
        let image = jpeg(640, 480, Some(144));
        let pdf = create_from_image(&image, CreateImageFormat::Jpeg, &CreateOptions::new())
            .expect("JPEG embed");
        let lengths = image_stream_lengths(&pdf);
        assert_eq!(lengths.len(), 1, "exactly one image XObject");
        assert_eq!(lengths[0], image.len(), "the whole JPEG file is embedded");
    }

    #[test]
    fn image_stream_length_prevents_endstream_truncation() {
        // A JPEG comment segment carrying the literal byte sequence that a
        // naive scanner would mistake for the end of the stream.
        let mut image = jpeg(8, 8, None);
        let mut comment = Vec::from(&b"\nendstream"[..]);
        comment.extend_from_slice(&[0, 1, 2, 3]);
        let mut com = vec![0xFF, 0xFE];
        com.extend_from_slice(&((comment.len() + 2) as u16).to_be_bytes());
        com.extend_from_slice(&comment);
        // Insert the COM segment just after SOI.
        image.splice(2..2, com);
        assert!(image.windows(b"endstream".len()).any(|w| w == b"endstream"));

        let pdf = create_from_image(&image, CreateImageFormat::Jpeg, &CreateOptions::new())
            .expect("JPEG with embedded endstream bytes");
        let dictionary_at = pdf
            .windows(b"/Subtype /Image".len())
            .position(|w| w == b"/Subtype /Image")
            .expect("image dictionary");
        let dict_start = pdf[..dictionary_at]
            .windows(2)
            .rposition(|w| w == b"<<")
            .expect("image dictionary opener");
        let dict_end = pdf[dictionary_at..]
            .windows(2)
            .position(|w| w == b">>")
            .map(|offset| dictionary_at + offset + 2)
            .expect("image dictionary closer");
        let length = declared_length(&pdf[dict_start..dict_end]).expect("/Length");
        assert_eq!(
            length,
            image.len(),
            "/Length covers the whole image payload"
        );
        assert!(
            pdf.windows(image.len())
                .any(|window| window == image.as_slice()),
            "the full image payload, including 'endstream', must be embedded verbatim"
        );
    }
}
