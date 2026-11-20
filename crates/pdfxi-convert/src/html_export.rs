//! Deterministic, dependency-free HTML export from a positioned page/text model.
//!
//! This module turns caller-supplied, pre-extracted page data into HTML with
//! absolutely positioned text spans. It performs no parsing, no rendering, and
//! no file I/O: the caller maps its own text extraction output (for example a
//! `pdfxi_text::TextSpan`) into [`HtmlSpan`] and hands the assembled
//! [`HtmlDocument`] to [`export_html`].
//!
//! # Output contract (all deterministic)
//!
//! 1. Output is UTF-8, uses a fixed `UTF-8` charset declaration, and joins every
//!    structural line with a single `\n`. No script element and no `on*`
//!    handler is ever emitted, so the result is inert.
//! 2. Pages are written in ascending zero-based index order, regardless of the
//!    order in which they were supplied. Duplicate page indices are rejected.
//! 3. Each page is a `<section class="pdfxi-page" id="page-N">` whose size is
//!    written inline from the page geometry. PDF page coordinates use a
//!    bottom-left origin, so a span's CSS `top` is
//!    `page_height - (y + height)`. One PDF point maps to one CSS pixel.
//! 4. Text is HTML-escaped (`& < > " '`) both in content and in attributes.
//!    Non-finite coordinates are typed errors, never `NaN`/`inf` in the output.
//! 5. Links are emitted as positioned anchors. Only relative, `http`, `https`,
//!    `mailto`, and in-page (`#...`) targets are allowed; active schemes such as
//!    `javascript:` are rejected with a typed error.
//! 6. Images are *referenced*, never embedded or fetched: an `<img>` is emitted
//!    only when the caller supplies a safe, already-extracted local filename.
//!    Paths with separators, traversal, or URI/drive schemes are rejected.
//! 7. [`HtmlOutputMode::SingleFile`] yields one self-contained document with an
//!    in-page navigation index; [`HtmlOutputMode::PageSet`] yields an
//!    `index.html` plus one standalone `page-NNNN.html` per page.
//! 8. Input sizes are bounded by documented constants (title, document-wide
//!    span/link totals, stylesheet, class prefix) so a hostile model cannot make
//!    the writer allocate without limit.
//! 9. [`HtmlStyle`] exposes style hooks (a CSS class prefix and bounded extra
//!    CSS), validated before emission. [`export_html_with_report`] returns a
//!    deterministic [`HtmlExportReport`] describing mode, counts, and notes.

use crate::export_options::LineEnding;
use std::collections::HashSet;
use thiserror::Error;

/// Maximum number of pages a single export accepts.
pub const MAX_PAGES: usize = 100_000;
/// Maximum number of positioned spans on one page.
pub const MAX_SPANS_PER_PAGE: usize = 1_000_000;
/// Maximum number of links on one page.
pub const MAX_LINKS_PER_PAGE: usize = 1_000_000;
/// Maximum number of UTF-8 bytes in a document title.
pub const MAX_TITLE_BYTES: usize = 16 * 1024;
/// Maximum number of spans across the whole document.
pub const MAX_TOTAL_SPANS: usize = 2_000_000;
/// Maximum number of links across the whole document.
pub const MAX_TOTAL_LINKS: usize = 2_000_000;
/// Maximum number of UTF-8 bytes in a caller-supplied extra stylesheet.
pub const MAX_STYLESHEET_BYTES: usize = 16 * 1024;
/// Maximum number of bytes in a CSS class prefix.
pub const MAX_CLASS_PREFIX_BYTES: usize = 64;

/// A typed failure while validating or writing an HTML export.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum HtmlExportError {
    /// The document contains no pages.
    #[error("empty document: at least one page is required")]
    EmptyDocument,
    /// The page count exceeds [`MAX_PAGES`].
    #[error("document has more than {limit} pages")]
    TooManyPages {
        /// The enforced page limit.
        limit: usize,
    },
    /// The same zero-based page index appears more than once.
    #[error("duplicate page index {page}")]
    DuplicatePage {
        /// The duplicated page index.
        page: usize,
    },
    /// A page has a non-positive or non-finite width or height.
    #[error("invalid geometry on page {page}: width and height must be finite and positive")]
    InvalidPageGeometry {
        /// The offending page index.
        page: usize,
    },
    /// A numeric field is not finite.
    #[error("non-finite {field} on page {page}")]
    NonFinite {
        /// The offending page index.
        page: usize,
        /// Name of the field that failed validation.
        field: &'static str,
    },
    /// A page has more than [`MAX_SPANS_PER_PAGE`] spans.
    #[error("page {page} has more than {limit} spans")]
    TooManySpans {
        /// The offending page index.
        page: usize,
        /// The enforced span limit.
        limit: usize,
    },
    /// A page has more than [`MAX_LINKS_PER_PAGE`] links.
    #[error("page {page} has more than {limit} links")]
    TooManyLinks {
        /// The offending page index.
        page: usize,
        /// The enforced link limit.
        limit: usize,
    },
    /// A link references a page that does not exist in the document.
    #[error("link references unknown page {page}")]
    UnknownLinkPage {
        /// The referenced page index.
        page: usize,
    },
    /// A link URI uses a scheme the exporter deliberately does not allow.
    #[error("unsupported link URI scheme '{scheme}' on page {page}")]
    UnsupportedUriScheme {
        /// The offending page index.
        page: usize,
        /// The extracted scheme (for example `javascript`).
        scheme: String,
    },
    /// A caller-supplied image reference is not a safe local filename.
    #[error("unsafe image reference '{reference}' on page {page}: {reason}")]
    UnsafeImageReference {
        /// The offending page index.
        page: usize,
        /// The rejected reference.
        reference: String,
        /// Why the reference was rejected.
        reason: &'static str,
    },
    /// The document title exceeds [`MAX_TITLE_BYTES`].
    #[error("document title exceeds {limit} bytes")]
    DocumentTitleTooLarge {
        /// The enforced title limit.
        limit: usize,
    },
    /// A document-wide content budget was exceeded.
    #[error("document has more than {limit} {what} in total")]
    TooMuchContent {
        /// The aggregated item kind (`spans` or `links`).
        what: &'static str,
        /// The enforced document-wide limit.
        limit: usize,
    },
    /// A caller-supplied stylesheet is unsafe or too large.
    #[error("unsafe stylesheet: {reason}")]
    UnsafeStylesheet {
        /// Why the stylesheet was rejected.
        reason: &'static str,
    },
    /// A CSS class prefix is not a safe identifier fragment.
    #[error("unsafe CSS class prefix: {reason}")]
    UnsafeClassPrefix {
        /// Why the prefix was rejected.
        reason: &'static str,
    },
}

/// One absolutely positioned run of recovered text.
#[derive(Debug, Clone, PartialEq)]
pub struct HtmlSpan {
    /// The recovered UTF-8 text of the run.
    pub text: String,
    /// Left edge of the run in PDF page coordinates.
    pub x: f32,
    /// Baseline of the run in PDF page coordinates.
    pub y: f32,
    /// Width of the run in page coordinates.
    pub width: f32,
    /// Height of the run in page coordinates.
    pub height: f32,
    /// Font size of the run in page coordinates.
    pub font_size: f32,
}

impl HtmlSpan {
    /// Creates a span anchored at `(x, y)` with zero metrics and font size.
    #[must_use]
    pub fn new(text: impl Into<String>, x: f32, y: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            width: 0.0,
            height: 0.0,
            font_size: 0.0,
        }
    }

    /// Attaches bounding-box and font metrics to this span.
    #[must_use]
    pub fn with_metrics(mut self, width: f32, height: f32, font_size: f32) -> Self {
        self.width = width;
        self.height = height;
        self.font_size = font_size;
        self
    }
}

/// A rectangular hyperlink overlay anchored to one page.
#[derive(Debug, Clone, PartialEq)]
pub struct HtmlLink {
    /// Zero-based index of the page that carries the link.
    pub page: usize,
    /// Link target URI (relative, `http`, `https`, `mailto`, or `#anchor`).
    pub uri: String,
    /// Left edge of the link rectangle in page coordinates.
    pub x: f32,
    /// Bottom edge of the link rectangle in page coordinates.
    pub y: f32,
    /// Width of the link rectangle in page coordinates.
    pub width: f32,
    /// Height of the link rectangle in page coordinates.
    pub height: f32,
}

impl HtmlLink {
    /// Creates a link rectangle on `page` pointing at `uri`.
    #[must_use]
    pub fn new(
        page: usize,
        uri: impl Into<String>,
        x: f32,
        y: f32,
        width: f32,
        height: f32,
    ) -> Self {
        Self {
            page,
            uri: uri.into(),
            x,
            y,
            width,
            height,
        }
    }
}

/// One page of positioned content for HTML export.
#[derive(Debug, Clone, PartialEq)]
pub struct HtmlPage {
    /// Zero-based page index.
    pub page: usize,
    /// Width of the page in PDF points.
    pub width: f32,
    /// Height of the page in PDF points.
    pub height: f32,
    /// Positioned text spans in reading order.
    pub spans: Vec<HtmlSpan>,
    /// Optional local image filename already extracted by the caller.
    ///
    /// When `None`, no `<img>` element is produced. The exporter never opens,
    /// fetches, or embeds image bytes; it only references the named file.
    pub image_ref: Option<String>,
}

impl HtmlPage {
    /// Creates an empty page with the given index and geometry.
    #[must_use]
    pub fn new(page: usize, width: f32, height: f32) -> Self {
        Self {
            page,
            width,
            height,
            spans: Vec::new(),
            image_ref: None,
        }
    }

    /// Attaches positioned spans to this page.
    #[must_use]
    pub fn with_spans(mut self, spans: Vec<HtmlSpan>) -> Self {
        self.spans = spans;
        self
    }

    /// Attaches an already-extracted local image filename to this page.
    #[must_use]
    pub fn with_image_ref(mut self, image_ref: impl Into<String>) -> Self {
        self.image_ref = Some(image_ref.into());
        self
    }
}

/// A complete document ready for HTML export.
#[derive(Debug, Clone, PartialEq)]
pub struct HtmlDocument {
    /// Document title, written (escaped) into `<title>`.
    pub title: String,
    /// Pages of the document.
    pub pages: Vec<HtmlPage>,
    /// Hyperlink overlays across all pages.
    pub links: Vec<HtmlLink>,
}

impl HtmlDocument {
    /// Creates a document from a title, its pages, and its links.
    #[must_use]
    pub fn new(title: impl Into<String>, pages: Vec<HtmlPage>, links: Vec<HtmlLink>) -> Self {
        Self {
            title: title.into(),
            pages,
            links,
        }
    }

    /// Returns the number of supplied pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// Selects the shape of an HTML export.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HtmlOutputMode {
    /// Produce one self-contained document with an in-page navigation index.
    #[default]
    SingleFile,
    /// Produce an `index.html` plus one standalone file per page.
    PageSet,
}

/// Deterministic style hooks for the generated HTML.
///
/// The `class_prefix` is prepended to every class the writer emits
/// (`page`, `span`, `link`, `nav`, `image`), so a host stylesheet can target the
/// output with its own namespace. The default is `pdfxi-`, which reproduces the
/// historical class names. `extra_css` is appended verbatim inside the single
/// `<style>` block. Both values are validated by [`export_html`]: the prefix
/// must be a short CSS identifier fragment and the stylesheet must not contain
/// markup, imports, or remote URLs, so hostile input can never break out of the
/// style element or fetch a resource.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlStyle {
    class_prefix: String,
    extra_css: String,
}

impl HtmlStyle {
    /// Returns style hooks with the default `pdfxi-` prefix and no extra CSS.
    #[must_use]
    pub fn new() -> Self {
        Self {
            class_prefix: "pdfxi-".to_owned(),
            extra_css: String::new(),
        }
    }

    /// Replaces the CSS class prefix (validated during export).
    #[must_use]
    pub fn with_class_prefix(mut self, prefix: impl Into<String>) -> Self {
        self.class_prefix = prefix.into();
        self
    }

    /// Appends caller-supplied CSS inside the generated `<style>` block.
    #[must_use]
    pub fn with_extra_css(mut self, css: impl Into<String>) -> Self {
        self.extra_css = css.into();
        self
    }

    /// Returns the configured class prefix.
    #[must_use]
    pub fn class_prefix(&self) -> &str {
        &self.class_prefix
    }

    /// Returns the caller-supplied extra CSS.
    #[must_use]
    pub fn extra_css(&self) -> &str {
        &self.extra_css
    }
}

impl Default for HtmlStyle {
    fn default() -> Self {
        Self::new()
    }
}

/// Options for an HTML export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlOptions {
    mode: HtmlOutputMode,
    include_navigation: bool,
    line_ending: LineEnding,
    style: HtmlStyle,
}

impl Default for HtmlOptions {
    fn default() -> Self {
        Self {
            mode: HtmlOutputMode::SingleFile,
            include_navigation: true,
            line_ending: LineEnding::Lf,
            style: HtmlStyle::default(),
        }
    }
}

impl HtmlOptions {
    /// Creates options with conservative defaults.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Selects single-file or page-set output.
    #[must_use]
    pub fn with_mode(mut self, mode: HtmlOutputMode) -> Self {
        self.mode = mode;
        self
    }

    /// Enables or disables the navigation index (single-file mode only).
    #[must_use]
    pub fn with_navigation(mut self, include: bool) -> Self {
        self.include_navigation = include;
        self
    }

    /// Replaces the structural line separator.
    #[must_use]
    pub fn with_line_ending(mut self, line_ending: LineEnding) -> Self {
        self.line_ending = line_ending;
        self
    }

    /// Replaces the style hooks used for generated classes and CSS.
    #[must_use]
    pub fn with_style(mut self, style: HtmlStyle) -> Self {
        self.style = style;
        self
    }

    /// Returns the configured style hooks.
    #[must_use]
    pub fn style(&self) -> &HtmlStyle {
        &self.style
    }

    /// Returns the output mode.
    #[must_use]
    pub const fn mode(&self) -> HtmlOutputMode {
        self.mode
    }

    /// Returns whether the navigation index is emitted.
    #[must_use]
    pub const fn include_navigation(&self) -> bool {
        self.include_navigation
    }

    /// Returns the configured line ending.
    #[must_use]
    pub const fn line_ending(&self) -> LineEnding {
        self.line_ending
    }
}

/// One named HTML output file in a page set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NamedHtml {
    /// Relative output filename (for example `page-0001.html`).
    pub name: String,
    /// UTF-8 contents of the file.
    pub contents: String,
}

impl NamedHtml {
    /// Creates a named output file.
    #[must_use]
    pub fn new(name: impl Into<String>, contents: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            contents: contents.into(),
        }
    }
}

/// One stable, deterministic note recorded by an HTML export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlFinding {
    code: &'static str,
    detail: String,
}

impl HtmlFinding {
    /// Creates a finding with a stable code and a deterministic detail string.
    #[must_use]
    pub fn new(code: &'static str, detail: impl Into<String>) -> Self {
        Self {
            code,
            detail: detail.into(),
        }
    }

    /// Returns the stable machine code (for example `REPORT.MODE`).
    #[must_use]
    pub fn code(&self) -> &'static str {
        self.code
    }

    /// Returns the human-readable detail.
    #[must_use]
    pub fn detail(&self) -> &str {
        &self.detail
    }
}

/// A deterministic report describing what an HTML export produced.
///
/// The report is advisory and never claims that text was extracted from PDF
/// bytes or that the output is a faithful rendering. It records the output mode,
/// the style hooks in use, aggregate counts, and one note per referenced image,
/// all in a canonical order so repeated exports compare byte-for-byte.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HtmlExportReport {
    files: usize,
    pages: usize,
    spans: usize,
    links: usize,
    images: usize,
    findings: Vec<HtmlFinding>,
}

impl HtmlExportReport {
    fn new(
        files: usize,
        pages: usize,
        spans: usize,
        links: usize,
        images: usize,
        mut findings: Vec<HtmlFinding>,
    ) -> Self {
        findings.sort_by(|a, b| a.code.cmp(b.code).then_with(|| a.detail.cmp(&b.detail)));
        Self {
            files,
            pages,
            spans,
            links,
            images,
            findings,
        }
    }

    /// Returns the number of named output files.
    #[must_use]
    pub const fn files(&self) -> usize {
        self.files
    }

    /// Returns the number of pages written.
    #[must_use]
    pub const fn pages(&self) -> usize {
        self.pages
    }

    /// Returns the number of positioned spans written.
    #[must_use]
    pub const fn spans(&self) -> usize {
        self.spans
    }

    /// Returns the number of hyperlink overlays written.
    #[must_use]
    pub const fn links(&self) -> usize {
        self.links
    }

    /// Returns the number of referenced (but never embedded) images.
    #[must_use]
    pub const fn images(&self) -> usize {
        self.images
    }

    /// Returns the canonical findings.
    #[must_use]
    pub fn findings(&self) -> &[HtmlFinding] {
        &self.findings
    }

    /// Returns `true` when the export produced no advisory findings.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    /// Returns a stable, human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "html: {} files, {} pages, {} spans, {} links, {} images ({} findings)",
            self.files,
            self.pages,
            self.spans,
            self.links,
            self.images,
            self.findings.len()
        )
    }
}

/// Validated, sorted view of a document used by both output modes.
struct Validated<'a> {
    pages: Vec<&'a HtmlPage>,
    links: Vec<&'a HtmlLink>,
}

fn fmt_num(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".to_owned()
    }
}

/// Escapes text for use in HTML element content.
fn escape_text(value: &str) -> String {
    escape_with(value, false)
}

/// Escapes text for use inside a double-quoted HTML attribute.
fn escape_attr(value: &str) -> String {
    escape_with(value, true)
}

fn escape_with(value: &str, attribute: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&#39;"),
            '\0' => out.push('\u{fffd}'),
            c if attribute && (c == '\n' || c == '\r' || c == '\t') => {
                out.push_str("&#");
                out.push_str(&(c as u32).to_string());
                out.push(';');
            }
            c => out.push(c),
        }
    }
    out
}

/// Extracts a URI scheme (the part before the first `:`, `/`, `?`, or `#`).
fn uri_scheme(uri: &str) -> Option<&str> {
    for (index, ch) in uri.char_indices() {
        match ch {
            ':' if index > 0 => return Some(&uri[..index]),
            '/' | '?' | '#' => return None,
            _ => {}
        }
    }
    None
}

fn validate_link(link: &HtmlLink, page: usize) -> Result<(), HtmlExportError> {
    for value in [link.x, link.y, link.width, link.height] {
        if !value.is_finite() {
            return Err(HtmlExportError::NonFinite {
                page,
                field: "link coordinate",
            });
        }
    }
    if let Some(scheme) = uri_scheme(&link.uri) {
        let allowed = scheme.eq_ignore_ascii_case("http")
            || scheme.eq_ignore_ascii_case("https")
            || scheme.eq_ignore_ascii_case("mailto");
        if !allowed {
            return Err(HtmlExportError::UnsupportedUriScheme {
                page,
                scheme: scheme.to_owned(),
            });
        }
    }
    Ok(())
}

fn validate_image_ref(reference: &str, page: usize) -> Result<(), HtmlExportError> {
    let fail = |reason: &'static str| HtmlExportError::UnsafeImageReference {
        page,
        reference: reference.to_owned(),
        reason,
    };
    if reference.is_empty() {
        return Err(fail("the reference is empty"));
    }
    if reference.contains('/') || reference.contains('\\') {
        return Err(fail("only a bare filename is accepted"));
    }
    if reference.contains(':') {
        return Err(fail("URI schemes and drive letters are not accepted"));
    }
    if reference.split("..").count() > 1 {
        return Err(fail("parent-directory traversal is not accepted"));
    }
    let extension = reference
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase());
    match extension.as_deref() {
        Some("png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp") => Ok(()),
        _ => Err(fail("the image extension is not an accepted raster format")),
    }
}

/// Enforces the document-wide span and link budgets.
fn check_document_totals(spans: usize, links: usize) -> Result<(), HtmlExportError> {
    if spans > MAX_TOTAL_SPANS {
        return Err(HtmlExportError::TooMuchContent {
            what: "spans",
            limit: MAX_TOTAL_SPANS,
        });
    }
    if links > MAX_TOTAL_LINKS {
        return Err(HtmlExportError::TooMuchContent {
            what: "links",
            limit: MAX_TOTAL_LINKS,
        });
    }
    Ok(())
}

/// Validates the CSS class prefix as a short, safe identifier fragment.
fn validate_class_prefix(prefix: &str) -> Result<(), HtmlExportError> {
    let fail = |reason: &'static str| HtmlExportError::UnsafeClassPrefix { reason };
    if prefix.is_empty() || prefix.len() > MAX_CLASS_PREFIX_BYTES {
        return Err(fail("the prefix must be between 1 and 64 ASCII bytes"));
    }
    let mut chars = prefix.chars();
    let first = chars.next().unwrap_or('\0');
    if !first.is_ascii_alphabetic() {
        return Err(fail("the prefix must start with an ASCII letter"));
    }
    if !chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_') {
        return Err(fail("only ASCII letters, digits, '-' and '_' are accepted"));
    }
    Ok(())
}

/// Validates caller-supplied CSS so it cannot break out of `<style>` or fetch.
fn validate_stylesheet(css: &str) -> Result<(), HtmlExportError> {
    let fail = |reason: &'static str| HtmlExportError::UnsafeStylesheet { reason };
    if css.len() > MAX_STYLESHEET_BYTES {
        return Err(fail("the stylesheet exceeds the byte limit"));
    }
    let lowered = css.to_ascii_lowercase();
    if css.contains('<') || css.contains('\0') {
        return Err(fail("markup and NUL characters are not accepted"));
    }
    if lowered.contains("@import") || lowered.contains("url(") {
        return Err(fail("remote imports and URLs are not accepted"));
    }
    Ok(())
}

fn validate<'a>(
    doc: &'a HtmlDocument,
    options: &HtmlOptions,
) -> Result<Validated<'a>, HtmlExportError> {
    if doc.title.len() > MAX_TITLE_BYTES {
        return Err(HtmlExportError::DocumentTitleTooLarge {
            limit: MAX_TITLE_BYTES,
        });
    }
    validate_class_prefix(options.style.class_prefix())?;
    validate_stylesheet(options.style.extra_css())?;
    if doc.pages.is_empty() {
        return Err(HtmlExportError::EmptyDocument);
    }
    if doc.pages.len() > MAX_PAGES {
        return Err(HtmlExportError::TooManyPages { limit: MAX_PAGES });
    }

    let mut total_spans = 0usize;
    let mut pages: Vec<&HtmlPage> = doc.pages.iter().collect();
    pages.sort_by_key(|page| page.page);
    let mut seen: HashSet<usize> = HashSet::with_capacity(pages.len());
    for page in &pages {
        if !seen.insert(page.page) {
            return Err(HtmlExportError::DuplicatePage { page: page.page });
        }
        if !(page.width.is_finite()
            && page.width > 0.0
            && page.height.is_finite()
            && page.height > 0.0)
        {
            return Err(HtmlExportError::InvalidPageGeometry { page: page.page });
        }
        if page.spans.len() > MAX_SPANS_PER_PAGE {
            return Err(HtmlExportError::TooManySpans {
                page: page.page,
                limit: MAX_SPANS_PER_PAGE,
            });
        }
        total_spans = total_spans.saturating_add(page.spans.len());
        if let Some(reference) = &page.image_ref {
            validate_image_ref(reference, page.page)?;
        }
        for span in &page.spans {
            for (value, field) in [
                (span.x, "span x"),
                (span.y, "span y"),
                (span.width, "span width"),
                (span.height, "span height"),
                (span.font_size, "span font size"),
            ] {
                if !value.is_finite() {
                    return Err(HtmlExportError::NonFinite {
                        page: page.page,
                        field,
                    });
                }
            }
            if span.width < 0.0 || span.height < 0.0 || span.font_size < 0.0 {
                return Err(HtmlExportError::NonFinite {
                    page: page.page,
                    field: "negative span metric",
                });
            }
        }
    }

    check_document_totals(total_spans, doc.links.len())?;
    let mut links: Vec<&HtmlLink> = doc.links.iter().collect();
    links.sort_by_key(|link| (link.page, link.uri.clone()));
    for link in &links {
        if !seen.contains(&link.page) {
            return Err(HtmlExportError::UnknownLinkPage { page: link.page });
        }
        validate_link(link, link.page)?;
    }
    // Per-page link counts are bounded even though the document-level list is
    // validated once, keeping each page body's allocation proportional to its
    // own content.
    for page in &pages {
        let count = links.iter().filter(|link| link.page == page.page).count();
        if count > MAX_LINKS_PER_PAGE {
            return Err(HtmlExportError::TooManyLinks {
                page: page.page,
                limit: MAX_LINKS_PER_PAGE,
            });
        }
    }

    Ok(Validated { pages, links })
}

fn page_anchor(page: usize) -> String {
    format!("page-{}", page + 1)
}

fn page_filename(page: usize) -> String {
    format!("page-{:04}.html", page + 1)
}

fn navigation_lines(
    pages: &[&HtmlPage],
    prefix: &str,
    href_for: impl Fn(usize) -> String,
) -> Vec<String> {
    let mut lines = vec![format!(
        "<nav class=\"{prefix}nav\" aria-label=\"Page navigation\">"
    )];
    for page in pages {
        lines.push(format!(
            "<a href=\"{href}\">{number}</a>",
            href = escape_attr(&href_for(page.page)),
            number = page.page + 1,
        ));
    }
    lines.push("</nav>".to_owned());
    lines
}

fn page_body_lines(page: &HtmlPage, prefix: &str, links: &[&HtmlLink]) -> Vec<String> {
    let width = fmt_num(page.width);
    let height = fmt_num(page.height);
    let anchor = page_anchor(page.page);
    let mut lines = vec![format!(
        "<section class=\"{prefix}page\" id=\"{anchor}\" style=\"width:{width}px;height:{height}px\">"
    )];

    if let Some(reference) = &page.image_ref {
        lines.push(format!(
            "<img class=\"{prefix}image\" src=\"{src}\" alt=\"Extracted page image\" style=\"position:absolute;inset:0;width:100%;height:100%\">",
            src = escape_attr(reference),
        ));
    }

    for span in &page.spans {
        let left = fmt_num(span.x);
        let top = fmt_num(page.height - (span.y + span.height));
        let span_width = fmt_num(span.width);
        let span_height = fmt_num(span.height);
        let font_size = fmt_num(span.font_size);
        lines.push(format!(
            "<span class=\"{prefix}span\" style=\"left:{left}px;top:{top}px;width:{span_width}px;height:{span_height}px;font-size:{font_size}px\">{}</span>",
            escape_text(&span.text),
        ));
    }

    for link in links.iter().filter(|link| link.page == page.page) {
        let left = fmt_num(link.x);
        let top = fmt_num(page.height - (link.y + link.height));
        let link_width = fmt_num(link.width);
        let link_height = fmt_num(link.height);
        lines.push(format!(
            "<a class=\"{prefix}link\" href=\"{href}\" aria-label=\"Link\" style=\"left:{left}px;top:{top}px;width:{link_width}px;height:{link_height}px\"></a>",
            href = escape_attr(&link.uri),
        ));
    }

    lines.push("</section>".to_owned());
    lines
}

/// Returns the fixed `<style>` block, with class names derived from `prefix`.
fn style_lines(style: &HtmlStyle) -> Vec<String> {
    let prefix = style.class_prefix();
    let mut lines = vec![
        "<style>".to_owned(),
        format!(
            ".{prefix}page{{position:relative;overflow:hidden;border:1px solid #ccc;margin:0 auto 1em}}"
        ),
        format!(".{prefix}span{{position:absolute;white-space:pre}}"),
        format!(".{prefix}link{{position:absolute;outline:1px solid #06c}}"),
        format!(".{prefix}nav a{{margin-right:.75em}}"),
    ];
    if !style.extra_css.is_empty() {
        lines.push(style.extra_css.clone());
    }
    lines.push("</style>".to_owned());
    lines
}

fn html_document(
    title: &str,
    body_lines: &[String],
    style: &HtmlStyle,
    line_ending: LineEnding,
) -> String {
    let mut lines = vec![
        "<!DOCTYPE html>".to_owned(),
        "<html lang=\"en\">".to_owned(),
        "<head>".to_owned(),
        "<meta charset=\"utf-8\">".to_owned(),
        format!("<title>{}</title>", escape_text(title)),
    ];
    lines.extend(style_lines(style));
    lines.push("</head>".to_owned());
    lines.push("<body>".to_owned());
    lines.extend(body_lines.iter().cloned());
    lines.push("</body>".to_owned());
    lines.push("</html>".to_owned());
    line_ending.join(lines) + line_ending.as_str()
}

/// Exports a document as one or more named HTML files.
///
/// In [`HtmlOutputMode::SingleFile`] mode the returned vector holds exactly one
/// entry named `document.html`. In [`HtmlOutputMode::PageSet`] mode it holds an
/// `index.html` followed by one `page-NNNN.html` per page, in ascending order.
///
/// # Errors
///
/// Returns a typed [`HtmlExportError`] when the document is empty or oversized,
/// a page index is duplicated, geometry or coordinates are invalid, a link uses
/// a disallowed scheme or unknown page, or an image reference is unsafe.
pub fn export_html(
    doc: &HtmlDocument,
    options: &HtmlOptions,
) -> Result<Vec<NamedHtml>, HtmlExportError> {
    let (files, _) = export_html_with_report(doc, options)?;
    Ok(files)
}

/// Exports a document and returns a deterministic report alongside the files.
///
/// The report records the output mode, style hooks, aggregate counts, and one
/// note per referenced image in canonical order. It does not claim that text was
/// extracted from PDF bytes or that the HTML is a faithful rendering; see
/// [`HtmlExportReport`].
///
/// # Errors
///
/// Propagates every [`HtmlExportError`] from validation, including unsafe style
/// hooks and document-wide content budgets.
pub fn export_html_with_report(
    doc: &HtmlDocument,
    options: &HtmlOptions,
) -> Result<(Vec<NamedHtml>, HtmlExportReport), HtmlExportError> {
    let validated = validate(doc, options)?;
    let line_ending = options.line_ending;
    let prefix = options.style.class_prefix();

    let span_count: usize = validated.pages.iter().map(|page| page.spans.len()).sum();
    let image_count = validated
        .pages
        .iter()
        .filter(|page| page.image_ref.is_some())
        .count();
    let navigation_detail = if options.mode == HtmlOutputMode::PageSet {
        "page-set index"
    } else if options.include_navigation {
        "single-file anchors"
    } else {
        "disabled"
    };
    let mut findings = vec![
        HtmlFinding::new(
            "REPORT.CONTENT",
            format!(
                "{} pages, {span_count} spans, {} links, {image_count} images",
                validated.pages.len(),
                validated.links.len()
            ),
        ),
        HtmlFinding::new(
            "REPORT.MODE",
            if options.mode == HtmlOutputMode::PageSet {
                "page-set"
            } else {
                "single-file"
            },
        ),
        HtmlFinding::new("REPORT.NAVIGATION", navigation_detail),
        HtmlFinding::new(
            "REPORT.STYLE",
            format!(
                "prefix={prefix} extra_css_bytes={}",
                options.style.extra_css().len()
            ),
        ),
    ];
    for page in validated
        .pages
        .iter()
        .filter(|page| page.image_ref.is_some())
    {
        findings.push(HtmlFinding::new(
            "REPORT.IMAGE_REFERENCED",
            format!("page {}", page.page),
        ));
    }

    let files = if options.mode == HtmlOutputMode::PageSet {
        let mut files = Vec::with_capacity(validated.pages.len() + 1);

        let mut index_lines = vec![
            "<h1>".to_owned(),
            escape_text(&doc.title),
            "</h1>".to_owned(),
        ];
        index_lines.extend(navigation_lines(&validated.pages, prefix, page_filename));
        files.push(NamedHtml::new(
            "index.html",
            html_document(&doc.title, &index_lines, &options.style, line_ending),
        ));

        for page in &validated.pages {
            let mut body = vec!["<p><a href=\"index.html\">&larr; Index</a></p>".to_owned()];
            body.extend(page_body_lines(page, prefix, &validated.links));
            files.push(NamedHtml::new(
                page_filename(page.page),
                html_document(&doc.title, &body, &options.style, line_ending),
            ));
        }
        files
    } else {
        let mut body: Vec<String> = Vec::new();
        body.push(format!("<h1>{}</h1>", escape_text(&doc.title)));
        if options.include_navigation {
            body.extend(navigation_lines(&validated.pages, prefix, |page| {
                format!("#{}", page_anchor(page))
            }));
        }
        body.push("<main>".to_owned());
        for page in &validated.pages {
            body.extend(page_body_lines(page, prefix, &validated.links));
        }
        body.push("</main>".to_owned());

        vec![NamedHtml::new(
            "document.html",
            html_document(&doc.title, &body, &options.style, line_ending),
        )]
    };

    let report = HtmlExportReport::new(
        files.len(),
        validated.pages.len(),
        span_count,
        validated.links.len(),
        image_count,
        findings,
    );
    Ok((files, report))
}

/// Convenience export that returns the single-file document as a string.
///
/// # Errors
///
/// Propagates every [`HtmlExportError`] from [`export_html`].
pub fn export_html_string(doc: &HtmlDocument) -> Result<String, HtmlExportError> {
    let files = export_html(doc, &HtmlOptions::new())?;
    Ok(files
        .into_iter()
        .next()
        .map(|file| file.contents)
        .unwrap_or_default())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_page(index: usize) -> HtmlPage {
        HtmlPage::new(index, 200.0, 300.0).with_spans(vec![
            HtmlSpan::new("Hi", 10.0, 280.0).with_metrics(40.0, 12.0, 12.0),
        ])
    }

    #[test]
    fn escape_text_escapes_all_special_characters() {
        assert_eq!(escape_text("<&>\"'\0"), "&lt;&amp;&gt;&quot;&#39;\u{fffd}");
        assert_eq!(escape_text("plain text"), "plain text");
        assert_eq!(escape_attr("a\"b\nc"), "a&quot;b&#10;c");
    }

    #[test]
    fn single_file_has_doctype_nav_and_pages_in_ascending_order() {
        let doc = HtmlDocument::new(
            "Doc",
            vec![sample_page(2), sample_page(0), sample_page(1)],
            Vec::new(),
        );
        let html = export_html_string(&doc).expect("export");
        assert!(html.starts_with("<!DOCTYPE html>"));
        assert!(html.contains("<title>Doc</title>"));
        let p0 = html.find("id=\"page-1\"").expect("page 1");
        let p1 = html.find("id=\"page-2\"").expect("page 2");
        let p2 = html.find("id=\"page-3\"").expect("page 3");
        assert!(p0 < p1 && p1 < p2);
        assert_eq!(html.matches("class=\"pdfxi-nav\"").count(), 1);
        assert_eq!(html.matches("<a href=\"#page-").count(), 3);
    }

    #[test]
    fn positioned_spans_invert_y_and_escape_text() {
        let page = HtmlPage::new(0, 200.0, 300.0).with_spans(vec![
            HtmlSpan::new("<b>&", 10.0, 280.0).with_metrics(40.0, 12.0, 12.0),
        ]);
        let doc = HtmlDocument::new("T", vec![page], Vec::new());
        let html = export_html_string(&doc).expect("export");
        assert!(html.contains("left:10px"));
        // top = 300 - (280 + 12) = 8
        assert!(html.contains("top:8px"));
        assert!(html.contains("width:40px;height:12px;font-size:12px"));
        assert!(html.contains("&lt;b&gt;&amp;</span>"));
    }

    #[test]
    fn links_emit_anchors_and_reject_script_schemes() {
        let doc = HtmlDocument::new(
            "T",
            vec![sample_page(0)],
            vec![HtmlLink::new(
                0,
                "https://example.test/a?x=1&y=2",
                5.0,
                10.0,
                20.0,
                8.0,
            )],
        );
        let html = export_html_string(&doc).expect("export");
        assert!(html.contains("<a class=\"pdfxi-link\""));
        assert!(html.contains("https://example.test/a?x=1&amp;y=2"));

        let bad = HtmlDocument::new(
            "T",
            vec![sample_page(0)],
            vec![HtmlLink::new(0, "javascript:alert(1)", 0.0, 0.0, 1.0, 1.0)],
        );
        let err = export_html_string(&bad).expect_err("must reject");
        assert!(matches!(
            err,
            HtmlExportError::UnsupportedUriScheme { ref scheme, .. } if scheme == "javascript"
        ));
    }

    #[test]
    fn image_reference_policy_allows_safe_files_and_rejects_unsafe_ones() {
        let good = HtmlDocument::new(
            "T",
            vec![sample_page(0).with_image_ref("img-1.png")],
            Vec::new(),
        );
        let html = export_html_string(&good).expect("export");
        assert!(html.contains("<img class=\"pdfxi-image\" src=\"img-1.png\""));

        let no_image = HtmlDocument::new("T", vec![sample_page(0)], Vec::new());
        assert!(
            !export_html_string(&no_image)
                .expect("export")
                .contains("<img")
        );

        for bad in [
            "../secret.png",
            "sub/a.png",
            "C:\\a.png",
            "http://x/a.png",
            "a.svg",
            "a.exe",
        ] {
            let doc = HtmlDocument::new("T", vec![sample_page(0).with_image_ref(bad)], Vec::new());
            let err = export_html_string(&doc).expect_err("must reject");
            assert!(
                matches!(err, HtmlExportError::UnsafeImageReference { .. }),
                "expected unsafe reference error for {bad}"
            );
        }
    }

    #[test]
    fn html_output_is_deterministic() {
        let doc = HtmlDocument::new("T", vec![sample_page(1), sample_page(0)], Vec::new());
        let a = export_html_string(&doc).expect("export");
        let b = export_html_string(&doc).expect("export");
        assert_eq!(a, b);
    }

    #[test]
    fn page_set_output_has_index_and_one_file_per_page() {
        let doc = HtmlDocument::new(
            "T",
            vec![sample_page(2), sample_page(0), sample_page(1)],
            Vec::new(),
        );
        let files = export_html(&doc, &HtmlOptions::new().with_mode(HtmlOutputMode::PageSet))
            .expect("export");
        assert_eq!(files.len(), 4);
        assert_eq!(files[0].name, "index.html");
        assert_eq!(files[1].name, "page-0001.html");
        assert_eq!(files[2].name, "page-0002.html");
        assert_eq!(files[3].name, "page-0003.html");
        assert!(files[0].contents.contains("href=\"page-0002.html\""));
        assert!(files[1].contents.contains("href=\"index.html\""));
    }

    #[test]
    fn non_finite_and_invalid_geometry_are_typed_errors() {
        let nan_page = HtmlPage::new(0, f32::NAN, 10.0);
        let doc = HtmlDocument::new("T", vec![nan_page], Vec::new());
        assert!(matches!(
            export_html_string(&doc),
            Err(HtmlExportError::InvalidPageGeometry { .. })
        ));

        let page =
            HtmlPage::new(0, 200.0, 300.0).with_spans(vec![HtmlSpan::new("x", f32::INFINITY, 0.0)]);
        let doc = HtmlDocument::new("T", vec![page], Vec::new());
        assert!(matches!(
            export_html_string(&doc),
            Err(HtmlExportError::NonFinite {
                field: "span x",
                ..
            })
        ));
    }

    #[test]
    fn duplicate_pages_and_unknown_link_pages_are_errors() {
        let dup = HtmlDocument::new("T", vec![sample_page(0), sample_page(0)], Vec::new());
        assert!(matches!(
            export_html_string(&dup),
            Err(HtmlExportError::DuplicatePage { .. })
        ));

        let unknown = HtmlDocument::new(
            "T",
            vec![sample_page(0)],
            vec![HtmlLink::new(5, "#x", 0.0, 0.0, 1.0, 1.0)],
        );
        let err = export_html_string(&unknown).expect_err("must reject");
        assert!(matches!(err, HtmlExportError::UnknownLinkPage { page: 5 }));
    }

    #[test]
    fn no_script_or_event_handlers_are_emitted() {
        let doc = HtmlDocument::new(
            "<img onerror=x>",
            vec![sample_page(0)],
            vec![HtmlLink::new(0, "https://example.test", 0.0, 0.0, 1.0, 1.0)],
        );
        let html = export_html_string(&doc).expect("export");
        assert!(!html.contains("<script"));
        // The angle brackets are escaped, so the would-be tag is inert text.
        assert!(html.contains("&lt;img onerror=x&gt;"));
    }

    #[test]
    fn empty_document_is_rejected() {
        let doc = HtmlDocument::new("T", Vec::new(), Vec::new());
        assert!(matches!(
            export_html_string(&doc),
            Err(HtmlExportError::EmptyDocument)
        ));
    }

    #[test]
    fn html_escaping_and_structure_survive_shuffled_input() {
        // Page and link order in the input must not affect the emitted bytes.
        let shuffled = HtmlDocument::new(
            "A & <B>",
            vec![sample_page(1), sample_page(0)],
            vec![
                HtmlLink::new(0, "https://example.test/?a=1&b=2", 1.0, 2.0, 3.0, 4.0),
                HtmlLink::new(1, "mailto:a@example.test", 1.0, 2.0, 3.0, 4.0),
            ],
        );
        let ordered = HtmlDocument::new(
            "A & <B>",
            vec![sample_page(0), sample_page(1)],
            vec![
                HtmlLink::new(1, "mailto:a@example.test", 1.0, 2.0, 3.0, 4.0),
                HtmlLink::new(0, "https://example.test/?a=1&b=2", 1.0, 2.0, 3.0, 4.0),
            ],
        );
        let a = export_html_string(&shuffled).expect("shuffled export");
        let b = export_html_string(&ordered).expect("ordered export");
        assert_eq!(
            a, b,
            "structure ordering must be independent of input order"
        );

        assert!(a.contains("<title>A &amp; &lt;B&gt;</title>"));
        assert!(a.contains("a=1&amp;b=2"));
        assert!(
            a.find("id=\"page-1\"").expect("page 1") < a.find("id=\"page-2\"").expect("page 2")
        );
        assert!(a.find("class=\"pdfxi-nav\"").expect("nav") < a.find("<main>").expect("main"));
    }

    #[test]
    fn html_export_empty_and_malformed_models_are_typed_errors() {
        assert_eq!(
            export_html_string(&HtmlDocument::new("T", Vec::new(), Vec::new())),
            Err(HtmlExportError::EmptyDocument)
        );
        let duplicate = HtmlDocument::new("T", vec![sample_page(0), sample_page(0)], Vec::new());
        assert_eq!(
            export_html_string(&duplicate),
            Err(HtmlExportError::DuplicatePage { page: 0 })
        );
        let malformed = HtmlDocument::new("T", vec![HtmlPage::new(0, 0.0, 300.0)], Vec::new());
        assert_eq!(
            export_html_string(&malformed),
            Err(HtmlExportError::InvalidPageGeometry { page: 0 })
        );
    }

    #[test]
    fn html_report_counts_content_and_is_deterministic() {
        let doc = HtmlDocument::new(
            "T",
            vec![sample_page(0).with_image_ref("p.png"), sample_page(1)],
            vec![HtmlLink::new(0, "https://example.test", 0.0, 0.0, 1.0, 1.0)],
        );
        let (files, report) = export_html_with_report(&doc, &HtmlOptions::new()).expect("export");
        assert_eq!(files.len(), 1);
        assert_eq!(report.files(), 1);
        assert_eq!(report.pages(), 2);
        assert_eq!(report.spans(), 2);
        assert_eq!(report.links(), 1);
        assert_eq!(report.images(), 1);
        assert!(!report.is_clean());
        let codes: Vec<&str> = report.findings().iter().map(HtmlFinding::code).collect();
        assert!(codes.contains(&"REPORT.IMAGE_REFERENCED"));
        assert!(codes.contains(&"REPORT.STYLE"));
        let mut sorted = codes.clone();
        sorted.sort_unstable();
        assert_eq!(codes, sorted, "findings are canonical");
        assert!(report.summary().contains("2 pages"));

        let (again, report_again) =
            export_html_with_report(&doc, &HtmlOptions::new()).expect("re-export");
        assert_eq!(files, again);
        assert_eq!(report, report_again);
    }

    #[test]
    fn style_hooks_change_classes_and_emit_extra_css() {
        let doc = HtmlDocument::new("T", vec![sample_page(0)], Vec::new());
        let options = HtmlOptions::new().with_style(
            HtmlStyle::new()
                .with_class_prefix("host-")
                .with_extra_css(".host-page{color:red}"),
        );
        let files = export_html(&doc, &options).expect("export");
        let html = &files[0].contents;
        assert!(html.contains("class=\"host-page\""));
        assert!(html.contains("class=\"host-span\""));
        assert!(html.contains(".host-page{position:relative"));
        assert!(html.contains(".host-page{color:red}"));
        assert!(!html.contains("class=\"pdfxi-page\""));
        assert_eq!(options.style().class_prefix(), "host-");
        assert_eq!(options.style().extra_css(), ".host-page{color:red}");
    }

    #[test]
    fn hostile_style_hooks_are_typed_errors() {
        let doc = HtmlDocument::new("T", vec![sample_page(0)], Vec::new());
        for css in [
            "</style><script>alert(1)</script>",
            "a{background:url(http://example.test/x)}",
            "@import 'remote.css';",
            "a\0b",
        ] {
            let options = HtmlOptions::new().with_style(HtmlStyle::new().with_extra_css(css));
            assert!(
                matches!(
                    export_html(&doc, &options),
                    Err(HtmlExportError::UnsafeStylesheet { .. })
                ),
                "expected stylesheet rejection for {css}"
            );
        }
        let long = "a".repeat(MAX_CLASS_PREFIX_BYTES + 1);
        for prefix in ["", "1bad", "a\"b", "a<b", long.as_str()] {
            let options = HtmlOptions::new().with_style(HtmlStyle::new().with_class_prefix(prefix));
            assert!(
                matches!(
                    export_html(&doc, &options),
                    Err(HtmlExportError::UnsafeClassPrefix { .. })
                ),
                "expected prefix rejection for {prefix}"
            );
        }
    }

    #[test]
    fn document_wide_content_budgets_are_enforced() {
        assert!(check_document_totals(MAX_TOTAL_SPANS, MAX_TOTAL_LINKS).is_ok());
        assert!(matches!(
            check_document_totals(MAX_TOTAL_SPANS + 1, 0),
            Err(HtmlExportError::TooMuchContent { what: "spans", .. })
        ));
        assert!(matches!(
            check_document_totals(0, MAX_TOTAL_LINKS + 1),
            Err(HtmlExportError::TooMuchContent { what: "links", .. })
        ));
    }

    #[test]
    fn oversized_title_is_a_typed_error() {
        let doc = HtmlDocument::new(
            "x".repeat(MAX_TITLE_BYTES + 1),
            vec![sample_page(0)],
            Vec::new(),
        );
        assert!(matches!(
            export_html_string(&doc),
            Err(HtmlExportError::DocumentTitleTooLarge { .. })
        ));
    }
}
