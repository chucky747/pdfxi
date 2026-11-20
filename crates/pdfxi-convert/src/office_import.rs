//! Bounded, dependency-free ingestion of caller-supplied Office-derived HTML/XML bundles.
//!
//! This module is the *read* counterpart of [`crate::office_export`]. Office
//! text extractors are optional external tools; this crate neither launches
//! them nor opens `.docx`/`.xlsx`/`.pptx` archives itself (no ZIP, no COM, no
//! network, no new dependency). Instead the caller supplies the **plain
//! HTML/XML fragment** an extractor produced, and this module validates a small,
//! deterministic subset and maps it onto the existing [`OfficeDocument`] model
//! that the export adapters already consume.
//!
//! # Supported input envelope
//!
//! - [`OfficeBundle`] carries a declared [`OfficeBundleKind`] (`Word`, `Excel`,
//!   or `PowerPoint`), a title, and a UTF-8 `markup` string. The kind is used for
//!   reporting only; the same markup subset is accepted for all three.
//! - Recognised tags: `p`/`div`/`w:p`, `h1`..`h6`, `ul`/`ol`/`li`,
//!   `table`/`w:tbl`/`tr`/`w:tr`/`td`/`th`/`w:tc`, `br`/`w:br`, and inline
//!   `span`/`w:t`/`b`/`strong`/`i`/`em`/`u`/`small`/`sub`/`sup`.
//! - Page breaks are the canonical `<!--pdfxi:page-->` marker or the common
//!   Word forms `<w:br w:type="page"/>` and `<br type="page"/>`.
//! - Entities `&amp; &lt; &gt; &quot; &apos;` and numeric `&#N;`/`&#xH;` are
//!   decoded; inline styling is intentionally not captured.
//!
//! # Honest scope
//!
//! Active content (`script`, `iframe`, `object`, `embed`, `form`, `input`, and
//! siblings), event-handler attributes, and remote `http(s)` references are
//! **typed errors**: nothing is fetched or executed. Media tags (`img`, `svg`,
//! `canvas`, `video`, `audio`) and unknown tags are recorded in
//! [`OfficeImportReport::findings`] with deterministic reasons and are never
//! guessed into content. [`import_office_bundle_strict`] promotes any finding to
//! a typed error. All sizes are bounded before parsing.

use std::collections::HashSet;

use thiserror::Error;

use crate::office_export::{
    CellValue, OfficeBlock, OfficeCell, OfficeDocument, OfficePage, ParagraphStyle, TableBlock,
};

/// Maximum accepted markup size in bytes, checked before parsing.
pub const OFFICE_BUNDLE_MAX_BYTES: usize = 4 * 1024 * 1024;
/// Maximum number of pages a bundle may declare through page-break markers.
pub const OFFICE_BUNDLE_MAX_PAGES: usize = 10_000;
/// Maximum number of blocks produced per page.
pub const OFFICE_BUNDLE_MAX_BLOCKS_PER_PAGE: usize = 100_000;
/// Maximum characters in a single imported text block or table cell.
pub const OFFICE_BUNDLE_MAX_TEXT_LEN: usize = 65_536;
/// Maximum number of distinct findings kept in a report.
pub const OFFICE_BUNDLE_MAX_FINDINGS: usize = 512;
/// Canonical, inert page-break marker.
pub const PAGE_BREAK_MARKER: &str = "<!--pdfxi:page-->";
/// Page width used for synthesized pages, in points (A4).
pub const OFFICE_BUNDLE_PAGE_WIDTH: f32 = 595.0;
/// Page height used for synthesized pages, in points (A4).
pub const OFFICE_BUNDLE_PAGE_HEIGHT: f32 = 842.0;

/// The Office family a bundle came from, used for deterministic reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum OfficeBundleKind {
    /// Word / WordprocessingML-derived HTML.
    Word,
    /// Excel / SpreadsheetML-derived HTML.
    Excel,
    /// PowerPoint / PresentationML-derived HTML.
    PowerPoint,
}

impl OfficeBundleKind {
    /// Returns a stable label for reports.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Word => "Word",
            Self::Excel => "Excel",
            Self::PowerPoint => "PowerPoint",
        }
    }

    /// Maps a file extension or format name to a bundle kind.
    #[must_use]
    pub fn from_name(name: &str) -> Option<Self> {
        match name.to_ascii_lowercase().as_str() {
            "docx" | "doc" | "word" | "wml" => Some(Self::Word),
            "xlsx" | "xls" | "excel" | "sml" => Some(Self::Excel),
            "pptx" | "ppt" | "powerpoint" | "pml" => Some(Self::PowerPoint),
            _ => None,
        }
    }
}

/// A caller-supplied, already-extracted Office HTML/XML bundle.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeBundle {
    /// Declared source family.
    pub kind: OfficeBundleKind,
    /// Document title carried into the resulting model.
    pub title: String,
    /// Plain HTML/XML markup produced by the caller's extractor.
    pub markup: String,
}

impl OfficeBundle {
    /// Creates a bundle from its kind, title, and markup.
    #[must_use]
    pub fn new(
        kind: OfficeBundleKind,
        title: impl Into<String>,
        markup: impl Into<String>,
    ) -> Self {
        Self {
            kind,
            title: title.into(),
            markup: markup.into(),
        }
    }
}

/// A typed failure while validating or converting an Office bundle.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OfficeImportError {
    /// The markup contains no non-whitespace content.
    #[error("empty Office bundle")]
    EmptyBundle,
    /// The markup exceeds [`OFFICE_BUNDLE_MAX_BYTES`].
    #[error("Office bundle exceeds {limit} bytes")]
    TooLarge {
        /// The enforced byte limit.
        limit: usize,
    },
    /// The bundle declares too many pages.
    #[error("Office bundle exceeds {limit} pages")]
    TooManyPages {
        /// The enforced page limit.
        limit: usize,
    },
    /// A page produced too many blocks.
    #[error("page {page} exceeds {limit} blocks")]
    TooManyBlocks {
        /// The offending page number (one-based).
        page: usize,
        /// The enforced block limit.
        limit: usize,
    },
    /// A text field exceeded [`OFFICE_BUNDLE_MAX_TEXT_LEN`].
    #[error("page {page} text exceeds {limit} characters")]
    TextTooLong {
        /// The offending page number (one-based).
        page: usize,
        /// The enforced text limit.
        limit: usize,
    },
    /// A construct is deliberately unsupported for security or fidelity reasons.
    #[error("unsupported Office bundle construct '{kind}': {detail}")]
    UnsupportedConstruct {
        /// Stable machine-readable kind.
        kind: &'static str,
        /// Deterministic explanation.
        detail: String,
    },
    /// A tag was not closed correctly.
    #[error("malformed Office bundle markup: {detail}")]
    MalformedMarkup {
        /// Stable explanation.
        detail: &'static str,
    },
}

/// One deterministic finding of content that was not converted.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct OfficeImportFinding {
    /// One-based page number.
    pub page: usize,
    /// Stable machine-readable kind (`media`, `unknown-tag`, ...).
    pub kind: &'static str,
    /// The tag or token that triggered the finding.
    pub token: String,
    /// Stable explanation.
    pub reason: &'static str,
}

/// A deterministic report describing the conversion.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeImportReport {
    /// Source family from the bundle.
    pub kind: OfficeBundleKind,
    /// Number of pages produced.
    pub page_count: usize,
    /// Number of blocks produced across all pages.
    pub block_count: usize,
    /// Sorted findings for content that was skipped.
    pub findings: Vec<OfficeImportFinding>,
}

impl OfficeImportReport {
    /// Returns `true` when nothing was skipped.
    #[must_use]
    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }
}

/// The converted document plus its report.
#[derive(Debug, Clone, PartialEq)]
pub struct OfficeImport {
    /// Document ready for the existing Office export adapters.
    pub document: OfficeDocument,
    /// Deterministic conversion report.
    pub report: OfficeImportReport,
}

/// Converts a bundle to an [`OfficeDocument`], recording skipped content.
///
/// # Errors
///
/// Returns a typed [`OfficeImportError`] for empty, oversized, malformed, or
/// deliberately unsupported (active/remote) input. Non-fatal unsupported content
/// is reported through [`OfficeImportReport::findings`].
pub fn import_office_bundle(bundle: &OfficeBundle) -> Result<OfficeDocument, OfficeImportError> {
    Ok(import_office_bundle_with_report(bundle)?.document)
}

/// Converts a bundle, failing on the first non-fatal finding as well.
///
/// # Errors
///
/// Propagates every [`OfficeImportError`] from [`import_office_bundle`] and
/// additionally returns [`OfficeImportError::UnsupportedConstruct`] when the
/// report contains any finding.
pub fn import_office_bundle_strict(
    bundle: &OfficeBundle,
) -> Result<OfficeImport, OfficeImportError> {
    let result = import_office_bundle_with_report(bundle)?;
    if let Some(first) = result.report.findings.first() {
        return Err(OfficeImportError::UnsupportedConstruct {
            kind: first.kind,
            detail: format!("page {}: {}", first.page, first.reason),
        });
    }
    Ok(result)
}

/// Converts a bundle and returns both the document and its deterministic report.
///
/// # Errors
///
/// See [`import_office_bundle`].
pub fn import_office_bundle_with_report(
    bundle: &OfficeBundle,
) -> Result<OfficeImport, OfficeImportError> {
    if bundle.markup.trim().is_empty() {
        return Err(OfficeImportError::EmptyBundle);
    }
    if bundle.markup.len() > OFFICE_BUNDLE_MAX_BYTES {
        return Err(OfficeImportError::TooLarge {
            limit: OFFICE_BUNDLE_MAX_BYTES,
        });
    }

    let normalized = normalize_page_breaks(&bundle.markup);
    let chunks: Vec<&str> = normalized.split(PAGE_BREAK_MARKER).collect();
    if chunks.len() > OFFICE_BUNDLE_MAX_PAGES {
        return Err(OfficeImportError::TooManyPages {
            limit: OFFICE_BUNDLE_MAX_PAGES,
        });
    }

    let mut pages = Vec::with_capacity(chunks.len());
    let mut all_findings = Vec::new();
    let mut block_count = 0usize;
    for (index, chunk) in chunks.iter().enumerate() {
        let mut parser = PageParser::new(index + 1);
        parser.parse(chunk)?;
        let count = parser.blocks.len();
        block_count = block_count.saturating_add(count);
        all_findings.extend(parser.findings);
        pages.push(
            OfficePage::new(index, OFFICE_BUNDLE_PAGE_WIDTH, OFFICE_BUNDLE_PAGE_HEIGHT)
                .with_blocks(parser.blocks),
        );
    }
    all_findings.sort();
    all_findings.dedup();
    let page_count = pages.len();

    Ok(OfficeImport {
        document: OfficeDocument::new(bundle.title.clone(), pages),
        report: OfficeImportReport {
            kind: bundle.kind,
            page_count,
            block_count,
            findings: all_findings,
        },
    })
}

fn normalize_page_breaks(markup: &str) -> String {
    markup
        .replace("<w:br w:type=\"page\"/>", PAGE_BREAK_MARKER)
        .replace("<w:br w:type=\"page\" />", PAGE_BREAK_MARKER)
        .replace("<br type=\"page\"/>", PAGE_BREAK_MARKER)
        .replace("<br type=\"page\" />", PAGE_BREAK_MARKER)
        .replace("<br w:type=\"page\"/>", PAGE_BREAK_MARKER)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Paragraph,
    Heading(u8),
    List { level: u8, ordered: bool },
}

struct PageParser {
    page: usize,
    blocks: Vec<OfficeBlock>,
    findings: Vec<OfficeImportFinding>,
    seen: HashSet<(String, String)>,
    text: String,
    kind: BlockKind,
    list_level: u8,
    ordered_list: bool,
    in_table: bool,
    in_cell: bool,
    table_rows: Vec<Vec<OfficeCell>>,
    row: Vec<OfficeCell>,
}

impl PageParser {
    fn new(page: usize) -> Self {
        Self {
            page,
            blocks: Vec::new(),
            findings: Vec::new(),
            seen: HashSet::new(),
            text: String::new(),
            kind: BlockKind::Paragraph,
            list_level: 0,
            ordered_list: false,
            in_table: false,
            in_cell: false,
            table_rows: Vec::new(),
            row: Vec::new(),
        }
    }

    fn note(&mut self, kind: &'static str, token: impl Into<String>) {
        let token = token.into();
        if self.findings.len() >= OFFICE_BUNDLE_MAX_FINDINGS
            || !self.seen.insert((kind.to_owned(), token.clone()))
        {
            return;
        }
        let reason = match kind {
            "media" => "media content is not decoded or embedded by this importer",
            "unknown-tag" => "tag is outside the documented HTML/XML subset",
            _ => "construct is outside the documented HTML/XML subset",
        };
        self.findings.push(OfficeImportFinding {
            page: self.page,
            kind,
            token,
            reason,
        });
    }

    fn push_block(&mut self, block: OfficeBlock) -> Result<(), OfficeImportError> {
        if self.blocks.len() >= OFFICE_BUNDLE_MAX_BLOCKS_PER_PAGE {
            return Err(OfficeImportError::TooManyBlocks {
                page: self.page,
                limit: OFFICE_BUNDLE_MAX_BLOCKS_PER_PAGE,
            });
        }
        self.blocks.push(block);
        Ok(())
    }

    fn check_len(&self, text: &str) -> Result<(), OfficeImportError> {
        if text.chars().count() > OFFICE_BUNDLE_MAX_TEXT_LEN {
            Err(OfficeImportError::TextTooLong {
                page: self.page,
                limit: OFFICE_BUNDLE_MAX_TEXT_LEN,
            })
        } else {
            Ok(())
        }
    }

    fn append_text(&mut self, chunk: &str) {
        let normalized = normalize_ws(chunk);
        if normalized.is_empty() {
            return;
        }
        if !self.text.is_empty() && !self.text.ends_with('\n') && !self.text.ends_with(' ') {
            self.text.push(' ');
        }
        self.text.push_str(&normalized);
    }

    fn flush_text(&mut self) -> Result<(), OfficeImportError> {
        let text = self.text.trim().replace('\u{1}', "\n");
        self.text.clear();
        if text.is_empty() {
            return Ok(());
        }
        self.check_len(&text)?;
        let block = match self.kind {
            BlockKind::Paragraph => OfficeBlock::paragraph(text),
            BlockKind::Heading(level) => {
                OfficeBlock::styled(text, ParagraphStyle::Heading(level.clamp(1, 9)))
            }
            BlockKind::List { level, ordered } => {
                OfficeBlock::list_item(text, level.min(8), ordered)
            }
        };
        self.push_block(block)
    }

    fn flush_cell(&mut self) -> Result<(), OfficeImportError> {
        let text = self.text.trim().replace('\u{1}', "\n");
        self.text.clear();
        self.check_len(&text)?;
        self.row.push(OfficeCell {
            value: CellValue::Text(text),
        });
        Ok(())
    }

    fn flush_pending(&mut self) -> Result<(), OfficeImportError> {
        if self.in_cell {
            self.flush_cell()
        } else {
            self.flush_text()
        }
    }

    fn finish_table(&mut self) -> Result<(), OfficeImportError> {
        if !self.row.is_empty() {
            self.table_rows.push(std::mem::take(&mut self.row));
        }
        self.in_table = false;
        self.in_cell = false;
        if self.table_rows.is_empty() {
            return Ok(());
        }
        let rows = std::mem::take(&mut self.table_rows);
        self.push_block(OfficeBlock::table(TableBlock::new(rows)))
    }

    fn parse(&mut self, markup: &str) -> Result<(), OfficeImportError> {
        let bytes = markup.as_bytes();
        let mut index = 0usize;
        while index < bytes.len() {
            if bytes[index] == b'<' {
                let end =
                    markup[index + 1..]
                        .find('>')
                        .ok_or(OfficeImportError::MalformedMarkup {
                            detail: "unclosed '<' tag",
                        })?
                        + index
                        + 1;
                let raw = &markup[index + 1..end];
                self.apply_tag(raw)?;
                index = end + 1;
            } else {
                let end = markup[index..]
                    .find('<')
                    .map_or(bytes.len(), |offset| index + offset);
                let text = decode_entities(&markup[index..end]);
                self.append_text(&text);
                index = end;
            }
        }
        self.flush_pending()?;
        if self.in_table {
            self.finish_table()?;
        }
        Ok(())
    }

    fn apply_tag(&mut self, raw: &str) -> Result<(), OfficeImportError> {
        let raw = raw.trim();
        let closing = raw.starts_with('/');
        let self_closing = raw.ends_with('/');
        let name_part = raw
            .trim_start_matches('/')
            .split(|c: char| c.is_whitespace() || c == '/')
            .next()
            .unwrap_or("");
        let local = name_part.rsplit(':').next().unwrap_or(name_part);
        let name = local.to_ascii_lowercase();

        if has_event_handler(raw) {
            return Err(OfficeImportError::UnsupportedConstruct {
                kind: "event-handler",
                detail: "HTML event-handler attributes are not executed or imported".to_owned(),
            });
        }
        if raw.contains("http://") || raw.contains("https://") {
            return Err(OfficeImportError::UnsupportedConstruct {
                kind: "remote-reference",
                detail: "remote http(s) references are never fetched".to_owned(),
            });
        }

        if is_active_tag(&name) {
            return Err(OfficeImportError::UnsupportedConstruct {
                kind: "active-content",
                detail: format!("<{name}> is active content and is rejected"),
            });
        }
        if is_media_tag(&name) {
            self.note("media", name.clone());
            return Ok(());
        }

        match name.as_str() {
            "table" | "tbl" => {
                if closing {
                    self.finish_table()?;
                } else {
                    self.flush_pending()?;
                    self.in_table = true;
                    self.table_rows.clear();
                }
            }
            "tr" => {
                if closing {
                    self.flush_cell()?;
                    self.in_cell = false;
                    if !self.row.is_empty() {
                        self.table_rows.push(std::mem::take(&mut self.row));
                    }
                } else {
                    self.flush_pending()?;
                    self.in_cell = false;
                }
            }
            "td" | "th" | "tc" => {
                if closing {
                    self.flush_cell()?;
                    self.in_cell = false;
                } else {
                    self.flush_pending()?;
                    self.in_cell = true;
                }
            }
            "ul" | "ol" => {
                self.flush_pending()?;
                self.ordered_list = name == "ol";
                self.list_level = 0;
            }
            "li" => {
                self.flush_pending()?;
                self.kind = BlockKind::List {
                    level: self.list_level,
                    ordered: self.ordered_list,
                };
                if closing {
                    self.flush_text()?;
                }
            }
            "br" => {
                if !self_closing || !raw.contains("type") {
                    self.text.push('\u{1}');
                }
            }
            "p" | "div" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.flush_pending()?;
                if !closing {
                    self.kind = match name.as_str() {
                        "h1" => BlockKind::Heading(1),
                        "h2" => BlockKind::Heading(2),
                        "h3" => BlockKind::Heading(3),
                        "h4" => BlockKind::Heading(4),
                        "h5" => BlockKind::Heading(5),
                        "h6" => BlockKind::Heading(6),
                        _ => BlockKind::Paragraph,
                    };
                } else {
                    self.kind = BlockKind::Paragraph;
                }
                self.in_cell = false;
            }
            "span" | "t" | "b" | "strong" | "i" | "em" | "u" | "small" | "sub" | "sup" | "font" => {
            }
            _ => self.note("unknown-tag", name),
        }
        Ok(())
    }
}

fn is_active_tag(name: &str) -> bool {
    matches!(
        name,
        "script"
            | "iframe"
            | "object"
            | "embed"
            | "form"
            | "input"
            | "textarea"
            | "select"
            | "button"
    )
}

fn is_media_tag(name: &str) -> bool {
    matches!(name, "img" | "svg" | "canvas" | "video" | "audio")
}

fn has_event_handler(raw: &str) -> bool {
    let lower = raw.to_ascii_lowercase();
    let bytes = lower.as_bytes();
    let mut index = 0usize;
    while index + 2 < bytes.len() {
        let at_attribute_boundary =
            index == 0 || bytes[index - 1].is_ascii_whitespace() || bytes[index - 1] == b'/';
        if at_attribute_boundary
            && bytes[index] == b'o'
            && bytes[index + 1] == b'n'
            && bytes[index + 2].is_ascii_alphabetic()
        {
            let mut cursor = index + 2;
            while cursor < bytes.len() && bytes[cursor].is_ascii_alphabetic() {
                cursor += 1;
            }
            while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                cursor += 1;
            }
            if cursor < bytes.len() && bytes[cursor] == b'=' {
                return true;
            }
            index = cursor;
        } else {
            index += 1;
        }
    }
    false
}

fn normalize_ws(text: &str) -> String {
    text.split_whitespace().collect::<Vec<&str>>().join(" ")
}

fn decode_entities(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    let mut rest = text;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index..];
        let Some(end) = rest.find(';') else {
            out.push_str(rest);
            return out;
        };
        let entity = &rest[1..end];
        let decoded = match entity {
            "amp" => Some('&'),
            "lt" => Some('<'),
            "gt" => Some('>'),
            "quot" => Some('"'),
            "apos" => Some('\''),
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                u32::from_str_radix(&entity[2..], 16)
                    .ok()
                    .and_then(char::from_u32)
            }
            _ if entity.starts_with('#') => {
                entity[1..].parse::<u32>().ok().and_then(char::from_u32)
            }
            _ => None,
        };
        if let Some(ch) = decoded {
            out.push(ch);
        } else {
            out.push('&');
            out.push_str(entity);
            out.push(';');
        }
        rest = &rest[end + 1..];
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::office_export::{DocxExporter, OfficeExporter};

    fn bundle(markup: &str) -> OfficeBundle {
        OfficeBundle::new(OfficeBundleKind::Word, "Imported", markup)
    }

    #[test]
    fn office_bundle_validation_accepts_word_html_and_builds_blocks() {
        let markup = "<h1>Report</h1><p>Hello &amp; welcome</p><ul><li>one</li></ul>\
                      <table><tr><td>a</td><td>b</td></tr></table>";
        let result = import_office_bundle_with_report(&bundle(markup)).expect("import");
        assert_eq!(result.report.page_count, 1);
        assert_eq!(result.report.kind, OfficeBundleKind::Word);
        assert!(result.report.is_clean());
        let blocks = &result.document.pages[0].blocks;
        assert_eq!(blocks.len(), 4);
        assert_match(&blocks[0], "heading");
        assert_match(&blocks[1], "paragraph:Hello & welcome");
        assert_match(&blocks[2], "list:one");
        assert_match(&blocks[3], "table:a|b");
        assert_eq!(result.report.block_count, 4);
    }

    fn assert_match(block: &OfficeBlock, expected: &str) {
        let actual = match block {
            OfficeBlock::Paragraph { text, style } => match style {
                ParagraphStyle::Heading(level) => format!("heading:{text}:{level}"),
                _ => format!("paragraph:{text}"),
            },
            OfficeBlock::ListItem { text, .. } => format!("list:{text}"),
            OfficeBlock::Table(table) => format!(
                "table:{}",
                table.rows[0]
                    .iter()
                    .map(|cell| match &cell.value {
                        CellValue::Text(text) => text.as_str(),
                        _ => "?",
                    })
                    .collect::<Vec<_>>()
                    .join("|")
            ),
            _ => "other".to_owned(),
        };
        assert!(
            actual.starts_with(expected),
            "expected {expected}, got {actual}"
        );
    }

    #[test]
    fn office_bundle_rejects_active_content_with_typed_error() {
        let result = import_office_bundle(&bundle("<script>alert(1)</script>"));
        assert!(matches!(
            result,
            Err(OfficeImportError::UnsupportedConstruct {
                kind: "active-content",
                ..
            })
        ));
        let handler = import_office_bundle(&bundle("<p onclick=\"boom()\">x</p>"));
        assert!(matches!(
            handler,
            Err(OfficeImportError::UnsupportedConstruct {
                kind: "event-handler",
                ..
            })
        ));
        let remote = import_office_bundle(&bundle("<img src=\"https://example.com/a.png\"/>"));
        assert!(matches!(
            remote,
            Err(OfficeImportError::UnsupportedConstruct {
                kind: "remote-reference",
                ..
            })
        ));
    }

    #[test]
    fn office_bundle_records_unsupported_media_finding() {
        let result = import_office_bundle_with_report(&bundle("<p>text</p><img src=\"a.png\"/>"))
            .expect("import");
        assert!(!result.report.is_clean());
        assert!(
            result
                .report
                .findings
                .iter()
                .any(|finding| finding.kind == "media" && finding.token == "img")
        );
        assert!(matches!(
            import_office_bundle_strict(&bundle("<p>text</p><svg></svg>")),
            Err(OfficeImportError::UnsupportedConstruct { kind: "media", .. })
        ));
    }

    #[test]
    fn office_bundle_page_breaks_produce_ordered_pages() {
        let markup = "<p>A</p><!--pdfxi:page--><p>B</p><w:br w:type=\"page\"/><p>C</p>";
        let result = import_office_bundle_with_report(&bundle(markup)).expect("import");
        assert_eq!(result.report.page_count, 3);
        let texts: Vec<String> = result
            .document
            .pages
            .iter()
            .map(|page| match &page.blocks[0] {
                OfficeBlock::Paragraph { text, .. } => text.clone(),
                _ => String::new(),
            })
            .collect();
        assert_eq!(texts, vec!["A", "B", "C"]);
    }

    #[test]
    fn office_bundle_output_is_deterministic() {
        let markup = "<h2>Title</h2><p>Deterministic &amp; stable</p>";
        let a = import_office_bundle_with_report(&bundle(markup)).expect("import");
        let b = import_office_bundle_with_report(&bundle(markup)).expect("import");
        assert_eq!(a, b);
        assert_eq!(a.document, b.document);
    }

    #[test]
    fn office_bundle_oversized_input_is_rejected() {
        let large = "x".repeat(OFFICE_BUNDLE_MAX_BYTES + 1);
        assert!(matches!(
            import_office_bundle(&bundle(&large)),
            Err(OfficeImportError::TooLarge { limit }) if limit == OFFICE_BUNDLE_MAX_BYTES
        ));
        assert!(matches!(
            import_office_bundle(&bundle("   ")),
            Err(OfficeImportError::EmptyBundle)
        ));
    }

    #[test]
    fn imported_office_document_exports_through_existing_adapters() {
        let document = import_office_bundle(&bundle("<p>Bridge works</p>")).expect("import");
        let exported = DocxExporter.export(&document).expect("export");
        assert_eq!(exported.extension(), "docx");
        assert!(exported.report.is_clean());
    }

    #[test]
    fn bundle_kind_mapping_and_entity_decoding_are_stable() {
        assert_eq!(
            OfficeBundleKind::from_name("docx"),
            Some(OfficeBundleKind::Word)
        );
        assert_eq!(
            OfficeBundleKind::from_name("XLSX"),
            Some(OfficeBundleKind::Excel)
        );
        assert_eq!(
            OfficeBundleKind::from_name("ppt"),
            Some(OfficeBundleKind::PowerPoint)
        );
        assert_eq!(OfficeBundleKind::from_name("pdf"), None);
        assert_eq!(decode_entities("&#65;&#x42;&lt;&gt;"), "AB<>");
    }
}
