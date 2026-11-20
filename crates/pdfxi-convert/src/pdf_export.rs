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
//! # Fidelity limits (round A)
//!
//! - Text comes from `Document::page_text`, so paragraph boundaries are the
//!   line breaks that extractor emits (the `'` and `"` operators). The core
//!   extractor does not expose glyph coordinates; a page whose content uses
//!   only `Tj`/`TJ` becomes one paragraph. Position-based line reconstruction is
//!   a separate extraction feature and is not claimed here.
//! - DOCX export writes text only. Images, vector art, annotations, and page
//!   backgrounds are not embedded and are not silently approximated.
//! - XLSX and PPTX writers exist in [`crate::office_export`] but are not wired
//!   to a real document in this round.

use crate::office_export::{
    DocxExportError, DocxExporter, OfficeBlock, OfficeDocument, OfficeExport, OfficePage,
};
use crate::text::{TextExportPlan, TextPageSelection};
use pdfxi_core::Document;
use std::collections::HashSet;

/// Default title written into the DOCX core properties.
pub const DEFAULT_DOCX_TITLE: &str = "PDFXI export";

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

/// A typed failure while exporting a parsed document.
#[derive(Debug, Clone, thiserror::Error)]
pub enum PdfExportError {
    /// Reading pages or text from the source document failed.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// The DOCX adapter rejected the mapped model or archive.
    #[error(transparent)]
    Docx(#[from] DocxExportError),
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
    #[error("the DOCX title must not be empty")]
    EmptyTitle,
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

    let mut pages = Vec::with_capacity(page_count);
    for index in 0..page_count {
        let page = document.page(index)?;
        let width = export_geometry(page.width(), index)?;
        let height = export_geometry(page.height(), index)?;
        let text = document.page_text(index)?;
        let blocks = text_lines(&text)
            .into_iter()
            .map(|line| OfficeBlock::paragraph(line.to_owned()))
            .collect();
        pages.push(OfficePage::new(index, width, height).with_blocks(blocks));
    }
    Ok(OfficeDocument::new(title, pages))
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
}
