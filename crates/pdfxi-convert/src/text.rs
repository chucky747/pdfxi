//! Deterministic plain-text export of pre-extracted page text.
//!
//! This module turns already-extracted page text into a single plain-text
//! string. It deliberately knows nothing about PDF parsing or rendering: the
//! caller supplies `(page, text)` pairs (for example derived from
//! [`pdfxi_text::TextPage::text`](https://docs.rs/pdfxi-text)), and
//! [`export_text`] applies a [`TextExportPlan`] to produce reproducible output.
//!
//! # Page numbering
//!
//! The page number carried by each pair and used by [`TextPageSelection`] is a
//! **zero-based page index**, matching `pdfxi_text::TextPage::page` and
//! `pdfxi_render::PageIndex`. Human-facing page headers display the one-based
//! value (`page + 1`).
//!
//! # Output rules (all deterministic)
//!
//! 1. Pages are selected and ordered by the plan's [`TextPageSelection`].
//!    [`TextPageSelection::All`] sorts by page number;
//!    [`TextPageSelection::Pages`] preserves the supplied order (including
//!    duplicates); [`TextPageSelection::Range`] ascends numerically.
//! 2. If the iterator yields the same page number more than once, the last
//!    text wins.
//! 3. A page with no supplied text is skipped.
//! 4. Selected pages are joined with the configured
//!    [`TextExportPlan::page_separator`]; the separator is inserted **between**
//!    chunks only, never after the last one.
//! 5. When [`TextExportPlan::include_page_headers`] is enabled, each emitted
//!    chunk is prefixed with `--- Page N ---\n`, where `N` is the one-based
//!    page number.
//! 6. Page text is emitted verbatim (no trimming). The result therefore has
//!    **no trailing newline** unless a page's own text ends with one; callers
//!    that require one can append it themselves.
//! 7. An empty document (or a selection matching no pages) yields an empty
//!    string.

use crate::ConversionError;
use std::collections::BTreeMap;

/// The default separator inserted between exported pages (`"\n"`).
pub const DEFAULT_PAGE_SEPARATOR: &str = "\n";

/// The prefix used for per-page headers. The `{page}` placeholder expands to
/// the one-based page number.
pub const PAGE_HEADER_PATTERN: &str = "--- Page {page} ---\n";

/// The character encoding applied when plain text is turned into bytes.
///
/// [`TextExportPlan::encoding`] controls which of these the [`crate::TextConverter`]
/// adapter uses in `Converter::export`. [`TextEncoding::encode`] is shared by
/// that path and by tests.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TextEncoding {
    /// UTF-8 without a byte-order mark (the default).
    #[default]
    Utf8,
    /// UTF-16 little-endian with a leading `FF FE` byte-order mark.
    Utf16Le,
    /// UTF-16 big-endian with a leading `FE FF` byte-order mark.
    Utf16Be,
}

impl TextEncoding {
    /// Every supported encoding, in a stable order.
    pub const ALL: &'static [Self] = &[Self::Utf8, Self::Utf16Le, Self::Utf16Be];

    /// Return every supported encoding in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Encode `text` into bytes using this encoding.
    ///
    /// UTF-8 is emitted without a BOM. UTF-16 variants always begin with their
    /// byte-order mark so the byte stream is self-describing. Non-BMP characters
    /// are emitted as surrogate pairs via [`str::encode_utf16`].
    #[must_use]
    pub fn encode(self, text: &str) -> Vec<u8> {
        match self {
            Self::Utf8 => text.as_bytes().to_vec(),
            Self::Utf16Le => encode_utf16(text, true),
            Self::Utf16Be => encode_utf16(text, false),
        }
    }

    /// Return a short human-readable label.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Utf8 => "utf-8",
            Self::Utf16Le => "utf-16le",
            Self::Utf16Be => "utf-16be",
        }
    }
}

impl std::fmt::Display for TextEncoding {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.label())
    }
}

/// Encode `text` as UTF-16 with an explicit byte-order mark.
fn encode_utf16(text: &str, little_endian: bool) -> Vec<u8> {
    let mut out = Vec::with_capacity(2 + text.len() * 2);
    if little_endian {
        out.extend_from_slice(&[0xFF, 0xFE]);
    } else {
        out.extend_from_slice(&[0xFE, 0xFF]);
    }
    for unit in text.encode_utf16() {
        if little_endian {
            out.extend_from_slice(&unit.to_le_bytes());
        } else {
            out.extend_from_slice(&unit.to_be_bytes());
        }
    }
    out
}

/// Selects which pages a [`TextExportPlan`] exports.
///
/// All variants use **zero-based** page indices, consistent with the iterator
/// passed to [`export_text`]. Human-facing headers display `page + 1`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum TextPageSelection {
    /// Export every page present in the input, sorted by ascending page number.
    #[default]
    All,
    /// Export the inclusive zero-based range `first..=last`, in ascending order.
    ///
    /// Pages in the range that have no text are skipped. An inverted range
    /// selects no pages.
    Range {
        /// First zero-based page index to export.
        first: usize,
        /// Last zero-based page index to export (must be at least `first`).
        last: usize,
    },
    /// Export an explicit zero-based page list in the caller's order.
    ///
    /// Duplicates are preserved; an empty list selects no pages.
    Pages(Vec<usize>),
}

impl TextPageSelection {
    /// Validate structural constraints that do not depend on the input pages.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] for an inverted range or an
    /// empty explicit page list. [`TextPageSelection::All`] is always valid.
    pub fn validate(&self) -> Result<(), ConversionError> {
        match self {
            Self::All => Ok(()),
            Self::Range { first, last } if last < first => Err(ConversionError::InvalidInput {
                message: format!("invalid text-export page range {first}..={last}"),
            }),
            Self::Range { .. } => Ok(()),
            Self::Pages(pages) if pages.is_empty() => Err(ConversionError::InvalidInput {
                message: "text-export page selection is empty".to_owned(),
            }),
            Self::Pages(_) => Ok(()),
        }
    }
}

/// Everything needed to export pre-extracted text as deterministic plain text.
///
/// Build a plan with [`TextExportPlan::new`] and the `with_*` methods, then pass
/// it to [`export_text`] (or to [`crate::TextConverter`]). Defaults are
/// [`TextPageSelection::All`], the separator `"\n"`, no page headers, and
/// [`TextEncoding::Utf8`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextExportPlan {
    pages: TextPageSelection,
    page_separator: String,
    include_page_headers: bool,
    encoding: TextEncoding,
}

impl Default for TextExportPlan {
    fn default() -> Self {
        Self::new()
    }
}

impl TextExportPlan {
    /// Create a plan with conservative defaults.
    #[must_use]
    pub fn new() -> Self {
        Self {
            pages: TextPageSelection::default(),
            page_separator: DEFAULT_PAGE_SEPARATOR.to_owned(),
            include_page_headers: false,
            encoding: TextEncoding::default(),
        }
    }

    /// Replace the page selection.
    #[must_use]
    pub fn with_pages(mut self, pages: TextPageSelection) -> Self {
        self.pages = pages;
        self
    }

    /// Replace the separator inserted between pages.
    ///
    /// The separator is emitted verbatim between chunks and is never appended
    /// after the last chunk.
    #[must_use]
    pub fn with_page_separator(mut self, separator: impl Into<String>) -> Self {
        self.page_separator = separator.into();
        self
    }

    /// Enable or disable per-page headers (`--- Page N ---\n`).
    #[must_use]
    pub fn with_page_headers(mut self, include: bool) -> Self {
        self.include_page_headers = include;
        self
    }

    /// Replace the encoding used when the text is turned into bytes.
    #[must_use]
    pub fn with_encoding(mut self, encoding: TextEncoding) -> Self {
        self.encoding = encoding;
        self
    }

    /// Return the configured page selection.
    #[must_use]
    pub fn pages(&self) -> &TextPageSelection {
        &self.pages
    }

    /// Return the configured page separator.
    #[must_use]
    pub fn page_separator(&self) -> &str {
        &self.page_separator
    }

    /// Return whether per-page headers are emitted.
    #[must_use]
    pub const fn include_page_headers(&self) -> bool {
        self.include_page_headers
    }

    /// Return the configured encoding.
    #[must_use]
    pub const fn encoding(&self) -> TextEncoding {
        self.encoding
    }

    /// Validate the plan's structural constraints.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] for an invalid page selection.
    pub fn validate(&self) -> Result<(), ConversionError> {
        self.pages.validate()
    }
}

/// Resolve the selected page numbers, in output order, against `by_page`.
fn selected_pages(plan: &TextExportPlan, by_page: &BTreeMap<usize, String>) -> Vec<usize> {
    match plan.pages() {
        TextPageSelection::All => by_page.keys().copied().collect(),
        TextPageSelection::Range { first, last } => {
            if last < first {
                Vec::new()
            } else {
                (*first..=*last)
                    .filter(|page| by_page.contains_key(page))
                    .collect()
            }
        }
        TextPageSelection::Pages(pages) => pages
            .iter()
            .copied()
            .filter(|page| by_page.contains_key(page))
            .collect(),
    }
}

/// Render the header line for a zero-based page index.
fn page_header(page: usize) -> String {
    PAGE_HEADER_PATTERN.replace("{page}", &page.saturating_add(1).to_string())
}

/// Export pre-extracted `(page, text)` pairs as deterministic plain text.
///
/// See the module documentation for the complete set of ordering, separator,
/// header, duplicate, and trailing-newline rules. The function is total: it
/// never fails and never panics, returning an empty string when no page has
/// text or when the selection matches nothing.
///
/// # Examples
///
/// ```text
/// use pdfxi_convert::{export_text, TextExportPlan, TextPageSelection};
///
/// let pages = [(1usize, "second".to_owned()), (0, "first".to_owned())];
/// let plan = TextExportPlan::new().with_page_separator(" | ");
/// assert_eq!(export_text(pages.into_iter(), &plan), "first | second");
/// ```
#[must_use]
pub fn export_text(pages: impl Iterator<Item = (usize, String)>, plan: &TextExportPlan) -> String {
    // A BTreeMap gives deterministic ascending key order for `All`, and the
    // last insertion wins for duplicate page numbers (documented behaviour).
    let by_page: BTreeMap<usize, String> = pages.collect();
    let selected = selected_pages(plan, &by_page);

    let mut chunks: Vec<String> = Vec::with_capacity(selected.len());
    for page in selected {
        // `selected_pages` only returns pages present in `by_page`.
        let text = match by_page.get(&page) {
            Some(text) => text,
            None => continue,
        };
        let mut chunk = String::new();
        if plan.include_page_headers() {
            chunk.push_str(&page_header(page));
        }
        chunk.push_str(text);
        chunks.push(chunk);
    }
    chunks.join(plan.page_separator())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn export_text_uses_separator_and_adds_no_trailing_newline() {
        let pages = vec![(0usize, "Alpha".to_owned()), (1, "Beta".to_owned())];
        let plan = TextExportPlan::new();
        assert_eq!(export_text(pages.into_iter(), &plan), "Alpha\nBeta");
        assert!(
            !export_text(vec![(0usize, "Alpha".to_owned())].into_iter(), &plan).ends_with('\n')
        );

        let custom = TextExportPlan::new().with_page_separator(" | ");
        assert_eq!(
            export_text(
                vec![(0usize, "Alpha".to_owned()), (1, "Beta".to_owned())].into_iter(),
                &custom
            ),
            "Alpha | Beta"
        );
    }

    #[test]
    fn export_text_all_sorts_by_page_number_and_last_duplicate_wins() {
        let pages = vec![
            (2usize, "third".to_owned()),
            (0, "first".to_owned()),
            (2, "third-again".to_owned()),
            (1, "second".to_owned()),
        ];
        let plan = TextExportPlan::new();
        assert_eq!(
            export_text(pages.into_iter(), &plan),
            "first\nsecond\nthird-again"
        );
    }

    #[test]
    fn export_text_explicit_page_order_and_range_are_honoured() {
        let pages = vec![
            (0usize, "zero".to_owned()),
            (1, "one".to_owned()),
            (2, "two".to_owned()),
        ];
        let explicit = TextExportPlan::new().with_pages(TextPageSelection::Pages(vec![2, 0]));
        assert_eq!(
            export_text(pages.clone().into_iter(), &explicit),
            "two\nzero"
        );

        let range =
            TextExportPlan::new().with_pages(TextPageSelection::Range { first: 1, last: 2 });
        assert_eq!(export_text(pages.into_iter(), &range), "one\ntwo");
    }

    #[test]
    fn export_text_page_headers_use_one_based_numbers() {
        let pages = vec![(0usize, "A".to_owned()), (1, "B".to_owned())];
        let plan = TextExportPlan::new().with_page_headers(true);
        assert_eq!(
            export_text(pages.into_iter(), &plan),
            "--- Page 1 ---\nA\n--- Page 2 ---\nB"
        );
    }

    #[test]
    fn export_text_empty_document_and_missing_pages_yield_empty_string() {
        let plan = TextExportPlan::new();
        assert_eq!(export_text(std::iter::empty(), &plan), "");
        let range =
            TextExportPlan::new().with_pages(TextPageSelection::Range { first: 5, last: 9 });
        assert_eq!(
            export_text(vec![(0usize, "x".to_owned())].into_iter(), &range),
            ""
        );
        let inverted =
            TextExportPlan::new().with_pages(TextPageSelection::Range { first: 2, last: 1 });
        assert_eq!(
            export_text(vec![(0usize, "x".to_owned())].into_iter(), &inverted),
            ""
        );
    }

    #[test]
    fn text_export_plan_validate_rejects_invalid_selections() {
        assert!(TextExportPlan::new().validate().is_ok());
        assert!(matches!(
            TextExportPlan::new()
                .with_pages(TextPageSelection::Range { first: 3, last: 2 })
                .validate(),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert!(matches!(
            TextExportPlan::new()
                .with_pages(TextPageSelection::Pages(Vec::new()))
                .validate(),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn text_export_plan_builder_getters_round_trip() {
        let plan = TextExportPlan::new()
            .with_pages(TextPageSelection::Pages(vec![2, 0]))
            .with_page_separator("--")
            .with_page_headers(true)
            .with_encoding(TextEncoding::Utf16Be);
        assert_eq!(plan.pages(), &TextPageSelection::Pages(vec![2, 0]));
        assert_eq!(plan.page_separator(), "--");
        assert!(plan.include_page_headers());
        assert_eq!(plan.encoding(), TextEncoding::Utf16Be);
        assert_eq!(TextExportPlan::default(), TextExportPlan::new());
    }

    #[test]
    fn text_encoding_emits_utf8_and_bom_prefixed_utf16() {
        let text = "Aé𐐀";
        assert_eq!(TextEncoding::Utf8.encode(text), text.as_bytes().to_vec());
        let le = TextEncoding::Utf16Le.encode(text);
        assert_eq!(&le[..2], &[0xFF, 0xFE]);
        let be = TextEncoding::Utf16Be.encode(text);
        assert_eq!(&be[..2], &[0xFE, 0xFF]);
        assert_eq!(le.len(), be.len());
        assert_eq!(le.len(), 2 + text.encode_utf16().count() * 2);
        assert_eq!(TextEncoding::Utf16Le.label(), "utf-16le");
        assert_eq!(TextEncoding::Utf8.to_string(), "utf-8");
        assert_eq!(TextEncoding::all().len(), 3);
    }
}
