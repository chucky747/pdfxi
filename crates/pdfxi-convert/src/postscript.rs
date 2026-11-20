//! Deterministic, dependency-free PostScript export from a page/content model.
//!
//! This module emits a small, DSC-conformant PostScript Level 2 program from a
//! caller-supplied [`PsDocument`]. It performs no PDF parsing and launches no
//! external converter: the caller maps its page content into [`PsContent`]
//! values and receives deterministic program bytes from [`export_postscript`].
//!
//! # Output contract (all deterministic)
//!
//! 1. The program starts with `%!PS-Adobe-3.0` and carries `%%Creator`,
//!    `%%Title`, `%%BoundingBox`, `%%Pages`, `%%LanguageLevel`, `%%DocumentData`,
//!    `%%Orientation`, `%%DocumentFonts`, `%%EndComments`, a prolog/setup pair,
//!    one `%%Page:` block per page, a `%%Trailer`, and `%%EOF`. The bounding box
//!    is the ceiling of the largest page extent.
//! 2. Pages are written in ascending zero-based index order; duplicate indices
//!    and invalid geometry are typed errors. Each page emits exactly one
//!    `showpage`.
//! 3. Only text, rectangles, and lines are drawn, using standard document
//!    fonts ([`PsFont`]). Text strings escape `\`, `(`, `)`, control characters,
//!    and encode Latin-1 bytes as octal escapes. Code points outside Latin-1
//!    cannot be shown with standard fonts and are omitted, with a note in
//!    [`PsExportReport`].
//! 4. Images have no raster path in this writer: an [`PsContent::Image`] is
//!    skipped and reported rather than silently faked. Callers can inspect the
//!    report from [`export_postscript_with_report`] to see exactly what was
//!    omitted, so unsupported content is documented honestly.
//! 5. Every numeric field is checked for finiteness before formatting, and
//!    input sizes (pages, per-page and document-wide content, title bytes) are
//!    bounded by documented constants. Titles must be single-line values. Hostile
//!    input therefore cannot produce `NaN` tokens, DSC comment injection, or
//!    unbounded allocation; every rejection is a typed [`PsExportError`].

use std::collections::BTreeSet;
use thiserror::Error;

/// Maximum number of pages a single export accepts.
pub const MAX_PAGES: usize = 100_000;
/// Maximum number of content items on one page.
pub const MAX_CONTENT_PER_PAGE: usize = 1_000_000;
/// Maximum number of UTF-8 bytes in a DSC title.
pub const MAX_TITLE_BYTES: usize = 4096;
/// Maximum number of content items across the whole document.
pub const MAX_TOTAL_CONTENT: usize = 2_000_000;

/// A typed failure while validating a PostScript export.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PsExportError {
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
    /// A text run declares a non-positive font size.
    #[error("invalid font size on page {page}: size must be finite and positive")]
    InvalidFontSize {
        /// The offending page index.
        page: usize,
    },
    /// A page has more than [`MAX_CONTENT_PER_PAGE`] items.
    #[error("page {page} has more than {limit} content items")]
    TooMuchContent {
        /// The offending page index.
        page: usize,
        /// The enforced content limit.
        limit: usize,
    },
    /// The DSC title is not a safe single-line value.
    #[error("invalid document title: {reason}")]
    InvalidTitle {
        /// Why the title was rejected.
        reason: &'static str,
    },
    /// The document-wide content budget was exceeded.
    #[error("document has more than {limit} content items in total")]
    DocumentTooLarge {
        /// The enforced document-wide limit.
        limit: usize,
    },
}

/// A standard PostScript document font used for text runs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PsFont {
    /// The sans-serif `Helvetica` face.
    Helvetica,
    /// The bold `Helvetica-Bold` face.
    HelveticaBold,
    /// The fixed-pitch `Courier` face.
    Courier,
    /// The serif `Times-Roman` face.
    TimesRoman,
    /// The bold `Times-Bold` face.
    TimesBold,
}

impl PsFont {
    /// Returns the PostScript font name used by `findfont`.
    #[must_use]
    pub const fn ps_name(self) -> &'static str {
        match self {
            Self::Helvetica => "Helvetica",
            Self::HelveticaBold => "Helvetica-Bold",
            Self::Courier => "Courier",
            Self::TimesRoman => "Times-Roman",
            Self::TimesBold => "Times-Bold",
        }
    }
}

/// One positioned text run drawn with a standard font.
#[derive(Debug, Clone, PartialEq)]
pub struct PsText {
    /// UTF-8 text to show.
    pub text: String,
    /// Left edge of the run in page coordinates (points).
    pub x: f32,
    /// Baseline of the run in page coordinates (points).
    pub y: f32,
    /// Font size in points.
    pub font_size: f32,
    /// Standard font used for this run.
    pub font: PsFont,
}

impl PsText {
    /// Creates a text run at `(x, y)` with [`PsFont::Helvetica`].
    #[must_use]
    pub fn new(text: impl Into<String>, x: f32, y: f32, font_size: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            font_size,
            font: PsFont::Helvetica,
        }
    }

    /// Replaces the standard font used for this run.
    #[must_use]
    pub fn with_font(mut self, font: PsFont) -> Self {
        self.font = font;
        self
    }
}

/// An axis-aligned rectangle drawn as a closed path.
#[derive(Debug, Clone, PartialEq)]
pub struct PsRect {
    /// Left edge in page coordinates.
    pub x: f32,
    /// Bottom edge in page coordinates.
    pub y: f32,
    /// Rectangle width.
    pub width: f32,
    /// Rectangle height.
    pub height: f32,
    /// When `true` the path is filled; otherwise it is stroked.
    pub fill: bool,
}

impl PsRect {
    /// Creates an outlined rectangle at `(x, y)`.
    #[must_use]
    pub fn new(x: f32, y: f32, width: f32, height: f32) -> Self {
        Self {
            x,
            y,
            width,
            height,
            fill: false,
        }
    }

    /// Marks the rectangle as filled instead of outlined.
    #[must_use]
    pub fn filled(mut self) -> Self {
        self.fill = true;
        self
    }
}

/// A straight line drawn with a given stroke width.
#[derive(Debug, Clone, PartialEq)]
pub struct PsLine {
    /// Start-point x coordinate.
    pub x1: f32,
    /// Start-point y coordinate.
    pub y1: f32,
    /// End-point x coordinate.
    pub x2: f32,
    /// End-point y coordinate.
    pub y2: f32,
    /// Stroke width.
    pub width: f32,
}

impl PsLine {
    /// Creates a line from `(x1, y1)` to `(x2, y2)`.
    #[must_use]
    pub fn new(x1: f32, y1: f32, x2: f32, y2: f32, width: f32) -> Self {
        Self {
            x1,
            y1,
            x2,
            y2,
            width,
        }
    }
}

/// A marker for image content the caller would like drawn.
///
/// Raster images are intentionally outside this writer's scope. Supplying one
/// is not an error: it is skipped and recorded in [`PsExportReport`] so the
/// omission is explicit rather than silent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsImageRef {
    /// A caller-supplied label for the omitted image.
    pub name: String,
}

impl PsImageRef {
    /// Creates a reference labelled `name`.
    #[must_use]
    pub fn new(name: impl Into<String>) -> Self {
        Self { name: name.into() }
    }
}

/// One drawable (or explicitly unsupported) content item on a page.
#[derive(Debug, Clone, PartialEq)]
pub enum PsContent {
    /// A positioned, escaped text run.
    Text(PsText),
    /// A rectangle path.
    Rect(PsRect),
    /// A stroked line.
    Line(PsLine),
    /// Image content, which is skipped and reported.
    Image(PsImageRef),
}

/// One page of content for PostScript export.
#[derive(Debug, Clone, PartialEq)]
pub struct PsPage {
    /// Zero-based page index.
    pub page: usize,
    /// Page width in points.
    pub width: f32,
    /// Page height in points.
    pub height: f32,
    /// Content items in drawing order.
    pub content: Vec<PsContent>,
}

impl PsPage {
    /// Creates an empty page with the given index and geometry.
    #[must_use]
    pub fn new(page: usize, width: f32, height: f32) -> Self {
        Self {
            page,
            width,
            height,
            content: Vec::new(),
        }
    }

    /// Attaches content items to this page.
    #[must_use]
    pub fn with_content(mut self, content: Vec<PsContent>) -> Self {
        self.content = content;
        self
    }
}

/// A complete document ready for PostScript export.
#[derive(Debug, Clone, PartialEq)]
pub struct PsDocument {
    /// Document title, written (escaped) into the DSC `%%Title` comment.
    pub title: String,
    /// Pages of the document.
    pub pages: Vec<PsPage>,
}

impl PsDocument {
    /// Creates a document from a title and its pages.
    #[must_use]
    pub fn new(title: impl Into<String>, pages: Vec<PsPage>) -> Self {
        Self {
            title: title.into(),
            pages,
        }
    }

    /// Returns the number of supplied pages.
    #[must_use]
    pub fn page_count(&self) -> usize {
        self.pages.len()
    }
}

/// One honestly reported omission from a PostScript export.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PsUnsupportedItem {
    /// Zero-based index of the page holding the omitted content.
    pub page: usize,
    /// Human-readable explanation of what was omitted and why.
    pub detail: String,
}

impl PsUnsupportedItem {
    /// Creates an unsupported-content note.
    #[must_use]
    pub fn new(page: usize, detail: impl Into<String>) -> Self {
        Self {
            page,
            detail: detail.into(),
        }
    }
}

/// A report describing what a PostScript export omitted.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PsExportReport {
    /// Number of pages written.
    pub pages: usize,
    /// Content that could not be represented and was skipped.
    pub unsupported: Vec<PsUnsupportedItem>,
}

impl PsExportReport {
    /// Returns `true` when nothing was omitted.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.unsupported.is_empty()
    }
}

struct Validated<'a> {
    pages: Vec<&'a PsPage>,
}

fn fmt_num(value: f32) -> String {
    if value.is_finite() {
        format!("{value}")
    } else {
        "0".to_owned()
    }
}

fn push_octal(out: &mut String, value: u32) {
    out.push('\\');
    out.push(char::from(b'0' + ((value >> 6) & 0x7) as u8));
    out.push(char::from(b'0' + ((value >> 3) & 0x7) as u8));
    out.push(char::from(b'0' + (value & 0x7) as u8));
}

/// Escapes a string for a PostScript literal, recording unrepresentable glyphs.
fn escape_ps_string(text: &str, page: usize, report: &mut PsExportReport) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        let code = ch as u32;
        match ch {
            '\\' => out.push_str("\\\\"),
            '(' => out.push_str("\\("),
            ')' => out.push_str("\\)"),
            '\n' => out.push_str("\\012"),
            '\r' => out.push_str("\\015"),
            '\t' => out.push_str("\\011"),
            _ if code < 0x20 || code == 0x7f => push_octal(&mut out, code),
            // Printable ASCII passes through verbatim; the remaining Latin-1
            // range is emitted as single-byte octal escapes for standard fonts.
            _ if code <= 0x7e => out.push(ch),
            _ if code <= 0xff => push_octal(&mut out, code),
            _ => report.unsupported.push(PsUnsupportedItem::new(
                page,
                format!("glyph U+{code:04X} is not representable with standard fonts; omitted"),
            )),
        }
    }
    out
}

/// Validates the DSC title as a bounded, single-line value.
fn validate_title(title: &str) -> Result<(), PsExportError> {
    if title.is_empty() {
        return Err(PsExportError::InvalidTitle {
            reason: "must not be empty",
        });
    }
    if title.len() > MAX_TITLE_BYTES {
        return Err(PsExportError::InvalidTitle {
            reason: "the title exceeds the byte limit",
        });
    }
    if title.contains('\r') || title.contains('\n') {
        return Err(PsExportError::InvalidTitle {
            reason: "the title must be a single DSC line",
        });
    }
    Ok(())
}

/// Enforces the document-wide content budget.
fn check_total_content(total: usize) -> Result<(), PsExportError> {
    if total > MAX_TOTAL_CONTENT {
        return Err(PsExportError::DocumentTooLarge {
            limit: MAX_TOTAL_CONTENT,
        });
    }
    Ok(())
}

fn validate(doc: &PsDocument) -> Result<Validated<'_>, PsExportError> {
    validate_title(&doc.title)?;
    if doc.pages.is_empty() {
        return Err(PsExportError::EmptyDocument);
    }
    if doc.pages.len() > MAX_PAGES {
        return Err(PsExportError::TooManyPages { limit: MAX_PAGES });
    }

    let mut total_content = 0usize;
    let mut pages: Vec<&PsPage> = doc.pages.iter().collect();
    pages.sort_by_key(|page| page.page);

    let mut seen = BTreeSet::new();
    for page in &pages {
        if !seen.insert(page.page) {
            return Err(PsExportError::DuplicatePage { page: page.page });
        }
        if !(page.width.is_finite()
            && page.width > 0.0
            && page.height.is_finite()
            && page.height > 0.0)
        {
            return Err(PsExportError::InvalidPageGeometry { page: page.page });
        }
        if page.content.len() > MAX_CONTENT_PER_PAGE {
            return Err(PsExportError::TooMuchContent {
                page: page.page,
                limit: MAX_CONTENT_PER_PAGE,
            });
        }
        total_content = total_content.saturating_add(page.content.len());
        for item in &page.content {
            match item {
                PsContent::Text(text) => {
                    for (value, field) in [
                        (text.x, "text x"),
                        (text.y, "text y"),
                        (text.font_size, "text font size"),
                    ] {
                        if !value.is_finite() {
                            return Err(PsExportError::NonFinite {
                                page: page.page,
                                field,
                            });
                        }
                    }
                    if text.font_size <= 0.0 {
                        return Err(PsExportError::InvalidFontSize { page: page.page });
                    }
                }
                PsContent::Rect(rect) => {
                    for (value, field) in [
                        (rect.x, "rect x"),
                        (rect.y, "rect y"),
                        (rect.width, "rect width"),
                        (rect.height, "rect height"),
                    ] {
                        if !value.is_finite() {
                            return Err(PsExportError::NonFinite {
                                page: page.page,
                                field,
                            });
                        }
                    }
                }
                PsContent::Line(line) => {
                    for (value, field) in [
                        (line.x1, "line x1"),
                        (line.y1, "line y1"),
                        (line.x2, "line x2"),
                        (line.y2, "line y2"),
                        (line.width, "line width"),
                    ] {
                        if !value.is_finite() {
                            return Err(PsExportError::NonFinite {
                                page: page.page,
                                field,
                            });
                        }
                    }
                }
                PsContent::Image(_) => {}
            }
        }
    }
    check_total_content(total_content)?;

    Ok(Validated { pages })
}

fn collect_fonts(pages: &[&PsPage]) -> Vec<&'static str> {
    let mut fonts = BTreeSet::new();
    for page in pages {
        for item in &page.content {
            if let PsContent::Text(text) = item {
                fonts.insert(text.font.ps_name());
            }
        }
    }
    fonts.into_iter().collect()
}

fn render_content(lines: &mut Vec<String>, page: &PsPage, report: &mut PsExportReport) {
    for item in &page.content {
        match item {
            PsContent::Text(text) => {
                lines.push(format!(
                    "/{} findfont {} scalefont setfont",
                    text.font.ps_name(),
                    fmt_num(text.font_size)
                ));
                lines.push(format!("{} {} moveto", fmt_num(text.x), fmt_num(text.y)));
                lines.push(format!(
                    "({}) show",
                    escape_ps_string(&text.text, page.page, report)
                ));
            }
            PsContent::Rect(rect) => {
                let x = fmt_num(rect.x);
                let y = fmt_num(rect.y);
                let width = fmt_num(rect.width);
                let height = fmt_num(rect.height);
                let neg_width = fmt_num(-rect.width);
                let neg_height = fmt_num(-rect.height);
                lines.push("newpath".to_owned());
                lines.push(format!("{x} {y} moveto"));
                lines.push(format!("{width} 0 rlineto"));
                lines.push(format!("0 {height} rlineto"));
                lines.push(format!("{neg_width} 0 rlineto"));
                lines.push(format!("0 {neg_height} rlineto"));
                lines.push("closepath".to_owned());
                lines.push(if rect.fill { "fill" } else { "stroke" }.to_owned());
            }
            PsContent::Line(line) => {
                lines.push("newpath".to_owned());
                lines.push(format!("{} {} moveto", fmt_num(line.x1), fmt_num(line.y1)));
                lines.push(format!("{} {} lineto", fmt_num(line.x2), fmt_num(line.y2)));
                lines.push(format!("{} setlinewidth", fmt_num(line.width)));
                lines.push("stroke".to_owned());
            }
            PsContent::Image(image) => {
                report.unsupported.push(PsUnsupportedItem::new(
                    page.page,
                    format!(
                        "image '{}' is not supported by the PostScript writer; omitted",
                        image.name
                    ),
                ));
            }
        }
    }
}

/// Exports a document as deterministic PostScript program bytes.
///
/// # Errors
///
/// Returns a typed [`PsExportError`] when the document is empty or oversized, a
/// page index is duplicated, geometry is invalid, a numeric field is not finite,
/// or a font size is not positive. Unsupported content is not an error; it is
/// reported by [`export_postscript_with_report`].
pub fn export_postscript(doc: &PsDocument) -> Result<Vec<u8>, PsExportError> {
    let (bytes, _) = export_postscript_with_report(doc)?;
    Ok(bytes)
}

/// Exports a document and returns the bytes together with an omission report.
///
/// Every entry in [`PsExportReport::unsupported`] names the page and explains
/// what the writer could not represent, so callers can surface that information
/// instead of silently producing an incomplete program.
///
/// # Errors
///
/// Propagates every [`PsExportError`] from validation.
pub fn export_postscript_with_report(
    doc: &PsDocument,
) -> Result<(Vec<u8>, PsExportReport), PsExportError> {
    let validated = validate(doc)?;
    let mut report = PsExportReport {
        pages: validated.pages.len(),
        unsupported: Vec::new(),
    };

    let max_width = validated
        .pages
        .iter()
        .map(|page| page.width.ceil() as i64)
        .max()
        .unwrap_or(0);
    let max_height = validated
        .pages
        .iter()
        .map(|page| page.height.ceil() as i64)
        .max()
        .unwrap_or(0);

    let mut lines = vec![
        "%!PS-Adobe-3.0".to_owned(),
        "%%Creator: PDFXI pdfxi-convert (T-0110)".to_owned(),
        format!("%%Title: {}", escape_ps_string(&doc.title, 0, &mut report)),
        format!("%%BoundingBox: 0 0 {max_width} {max_height}"),
        format!("%%Pages: {}", validated.pages.len()),
        "%%LanguageLevel: 2".to_owned(),
        "%%DocumentData: Clean7Bit".to_owned(),
        "%%Orientation: Portrait".to_owned(),
    ];
    let fonts = collect_fonts(&validated.pages);
    lines.push(format!(
        "%%DocumentFonts: {}",
        if fonts.is_empty() {
            "none".to_owned()
        } else {
            fonts.join(" ")
        }
    ));
    lines.extend([
        "%%EndComments".to_owned(),
        "%%BeginProlog".to_owned(),
        "%%EndProlog".to_owned(),
        "%%BeginSetup".to_owned(),
        "%%EndSetup".to_owned(),
    ]);

    for page in &validated.pages {
        lines.push(format!("%%Page: {0} {0}", page.page + 1));
        lines.push("%%BeginPageSetup".to_owned());
        lines.push("%%EndPageSetup".to_owned());
        lines.push("gsave".to_owned());
        render_content(&mut lines, page, &mut report);
        lines.push("grestore".to_owned());
        lines.push("showpage".to_owned());
    }

    lines.push("%%Trailer".to_owned());
    lines.push("%%EOF".to_owned());

    Ok((lines.join("\n").into_bytes(), report))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_page(index: usize) -> PsPage {
        PsPage::new(index, 200.0, 300.0).with_content(vec![PsContent::Text(PsText::new(
            "Hello", 10.0, 20.0, 12.0,
        ))])
    }

    #[test]
    fn header_has_dsc_version_boundingbox_pages_and_fonts() {
        let doc = PsDocument::new("My Doc", vec![sample_page(0), sample_page(1)]);
        let bytes = export_postscript(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(text.starts_with("%!PS-Adobe-3.0\n"));
        assert!(text.contains("%%Creator:"));
        assert!(text.contains("%%Title: My Doc"));
        assert!(text.contains("%%BoundingBox: 0 0 200 300"));
        assert!(text.contains("%%Pages: 2"));
        assert!(text.contains("%%DocumentFonts: Helvetica"));
        assert!(text.contains("%%EndComments"));
        assert!(text.contains("%%BeginProlog"));
        assert!(text.contains("%%EndSetup"));
        assert!(text.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn page_sections_and_showpage_count_match_page_count() {
        let doc = PsDocument::new("T", vec![sample_page(2), sample_page(0), sample_page(1)]);
        let bytes = export_postscript(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        assert_eq!(text.matches("%%Page:").count(), 3);
        assert_eq!(text.matches("showpage").count(), 3);
        assert!(text.contains("%%Page: 1 1"));
        assert!(text.contains("%%Page: 2 2"));
        assert!(text.contains("%%Page: 3 3"));
        let first = text.find("%%Page: 1 1").expect("p1");
        let second = text.find("%%Page: 2 2").expect("p2");
        let third = text.find("%%Page: 3 3").expect("p3");
        assert!(first < second && second < third);
    }

    #[test]
    fn text_uses_standard_font_and_is_positioned() {
        let doc = PsDocument::new(
            "T",
            vec![
                PsPage::new(0, 100.0, 100.0).with_content(vec![PsContent::Text(
                    PsText::new("Word", 12.5, 30.0, 14.0).with_font(PsFont::TimesRoman),
                )]),
            ],
        );
        let bytes = export_postscript(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(text.contains("/Times-Roman findfont 14 scalefont setfont"));
        assert!(text.contains("12.5 30 moveto"));
        assert!(text.contains("(Word) show"));
        assert!(text.contains("%%DocumentFonts: Times-Roman"));
    }

    #[test]
    fn text_string_escaping_handles_backslash_parens_and_controls() {
        let mut report = PsExportReport::default();
        let escaped = escape_ps_string("a(b)c\\d\n\0\u{7f}", 0, &mut report);
        assert_eq!(escaped, "a\\(b\\)c\\\\d\\012\\000\\177");
        assert!(report.is_clean());
        let doc = PsDocument::new(
            "T",
            vec![
                PsPage::new(0, 100.0, 100.0)
                    .with_content(vec![PsContent::Text(PsText::new("a(b)", 1.0, 2.0, 3.0))]),
            ],
        );
        let text = String::from_utf8(export_postscript(&doc).expect("export")).expect("ascii");
        assert!(text.contains("(a\\(b\\)) show"));
    }

    #[test]
    fn rect_and_line_emit_path_operators() {
        let doc = PsDocument::new(
            "T",
            vec![PsPage::new(0, 100.0, 100.0).with_content(vec![
                PsContent::Rect(PsRect::new(1.0, 2.0, 10.0, 20.0).filled()),
                PsContent::Line(PsLine::new(0.0, 0.0, 5.0, 5.0, 2.0)),
            ])],
        );
        let text = String::from_utf8(export_postscript(&doc).expect("export")).expect("ascii");
        assert!(text.contains("1 2 moveto"));
        assert!(text.contains("10 0 rlineto"));
        assert!(text.contains("0 20 rlineto"));
        assert!(text.contains("-10 0 rlineto"));
        assert!(text.contains("closepath\nfill"));
        assert!(text.contains("0 0 moveto\n5 5 lineto\n2 setlinewidth\nstroke"));
    }

    #[test]
    fn unsupported_image_and_non_latin1_glyphs_are_reported() {
        let doc = PsDocument::new(
            "T",
            vec![PsPage::new(0, 100.0, 100.0).with_content(vec![
                PsContent::Image(PsImageRef::new("scan")),
                PsContent::Text(PsText::new("A\u{20ac}B", 1.0, 2.0, 3.0)),
            ])],
        );
        let (bytes, report) = export_postscript_with_report(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(!report.is_clean());
        assert_eq!(report.pages, 1);
        assert!(
            report
                .unsupported
                .iter()
                .any(|item| item.detail.contains("image 'scan'"))
        );
        assert!(
            report
                .unsupported
                .iter()
                .any(|item| item.detail.contains("U+20AC"))
        );
        // The image is not drawn, and the unrepresentable glyph is omitted from
        // the shown string while its representable neighbours remain.
        assert!(!text.contains("(scan)"));
        assert!(text.contains("(AB) show"));
    }

    #[test]
    fn postscript_bytes_are_deterministic() {
        let doc = PsDocument::new("T", vec![sample_page(1), sample_page(0)]);
        let a = export_postscript(&doc).expect("export");
        let b = export_postscript(&doc).expect("export");
        assert_eq!(a, b);
    }

    #[test]
    fn boundingbox_uses_ceil_of_max_page_extent() {
        let doc = PsDocument::new(
            "T",
            vec![PsPage::new(0, 100.2, 200.9), PsPage::new(1, 300.1, 50.5)],
        );
        let text = String::from_utf8(export_postscript(&doc).expect("export")).expect("ascii");
        assert!(text.contains("%%BoundingBox: 0 0 301 201"));
    }

    #[test]
    fn non_finite_coordinates_are_typed_errors() {
        let doc = PsDocument::new(
            "T",
            vec![
                PsPage::new(0, 100.0, 100.0).with_content(vec![PsContent::Line(PsLine::new(
                    f32::NAN,
                    0.0,
                    1.0,
                    1.0,
                    1.0,
                ))]),
            ],
        );
        assert!(matches!(
            export_postscript(&doc),
            Err(PsExportError::NonFinite {
                field: "line x1",
                ..
            })
        ));
    }

    #[test]
    fn invalid_geometry_font_size_and_duplicates_are_errors() {
        let bad_geo = PsDocument::new("T", vec![PsPage::new(0, 0.0, 10.0)]);
        assert!(matches!(
            export_postscript(&bad_geo),
            Err(PsExportError::InvalidPageGeometry { .. })
        ));

        let bad_font = PsDocument::new(
            "T",
            vec![
                PsPage::new(0, 10.0, 10.0)
                    .with_content(vec![PsContent::Text(PsText::new("x", 0.0, 0.0, 0.0))]),
            ],
        );
        assert!(matches!(
            export_postscript(&bad_font),
            Err(PsExportError::InvalidFontSize { .. })
        ));

        let dup = PsDocument::new("T", vec![sample_page(0), sample_page(0)]);
        assert!(matches!(
            export_postscript(&dup),
            Err(PsExportError::DuplicatePage { .. })
        ));
    }

    #[test]
    fn empty_document_is_rejected() {
        let doc = PsDocument::new("T", Vec::new());
        assert!(matches!(
            export_postscript(&doc),
            Err(PsExportError::EmptyDocument)
        ));
    }

    #[test]
    fn dsc_headers_include_language_level_data_and_orientation() {
        let doc = PsDocument::new("T", vec![sample_page(0)]);
        let bytes = export_postscript(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        assert!(text.starts_with("%!PS-Adobe-3.0\n"));
        assert!(text.contains("%%LanguageLevel: 2\n"));
        assert!(text.contains("%%DocumentData: Clean7Bit\n"));
        assert!(text.contains("%%Orientation: Portrait\n"));
        let language = text.find("%%LanguageLevel: 2").expect("language");
        let comments = text.find("%%EndComments").expect("comments");
        assert!(language < comments, "DSC headers precede EndComments");
        assert!(text.trim_end().ends_with("%%EOF"));
    }

    #[test]
    fn content_items_are_emitted_in_caller_order() {
        let doc = PsDocument::new(
            "T",
            vec![PsPage::new(0, 100.0, 100.0).with_content(vec![
                PsContent::Text(PsText::new("First", 1.0, 2.0, 3.0)),
                PsContent::Rect(PsRect::new(10.0, 20.0, 30.0, 40.0)),
                PsContent::Line(PsLine::new(3.0, 4.0, 5.0, 6.0, 1.0)),
                PsContent::Text(PsText::new("Second", 7.0, 8.0, 9.0)),
            ])],
        );
        let bytes = export_postscript(&doc).expect("export");
        let text = String::from_utf8(bytes).expect("ascii");
        let first = text.find("(First) show").expect("first text");
        let rect = text.find("10 20 moveto").expect("rect");
        let line = text.find("5 6 lineto").expect("line");
        let second = text.find("(Second) show").expect("second text");
        assert!(first < rect && rect < line && line < second);
        assert_eq!(text.matches("showpage").count(), 1);
    }

    #[test]
    fn hostile_and_oversized_titles_are_typed_errors() {
        let long = "x".repeat(MAX_TITLE_BYTES + 1);
        for title in ["", "first\nsecond", "first\rsecond", long.as_str()] {
            let doc = PsDocument::new(title, vec![sample_page(0)]);
            assert!(
                matches!(
                    export_postscript(&doc),
                    Err(PsExportError::InvalidTitle { .. })
                ),
                "expected title rejection for {title:?}"
            );
        }
    }

    #[test]
    fn aggregate_content_budget_is_enforced() {
        assert!(check_total_content(MAX_TOTAL_CONTENT).is_ok());
        assert!(matches!(
            check_total_content(MAX_TOTAL_CONTENT + 1),
            Err(PsExportError::DocumentTooLarge { .. })
        ));
    }

    #[test]
    fn postscript_output_is_deterministic_with_full_dsc_header() {
        let doc = PsDocument::new("T", vec![sample_page(1), sample_page(0)]);
        let first = export_postscript(&doc).expect("export");
        let second = export_postscript(&doc).expect("re-export");
        assert_eq!(first, second);
        let text = String::from_utf8(first).expect("ascii");
        for header in [
            "%!PS-Adobe-3.0",
            "%%Title: T",
            "%%BoundingBox: 0 0 200 300",
            "%%Pages: 2",
            "%%LanguageLevel: 2",
            "%%DocumentData: Clean7Bit",
            "%%Orientation: Portrait",
            "%%DocumentFonts: Helvetica",
            "%%Trailer",
            "%%EOF",
        ] {
            assert!(text.contains(header), "missing DSC token {header}");
        }
    }
}
