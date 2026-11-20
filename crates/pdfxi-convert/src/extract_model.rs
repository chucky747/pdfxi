//! Caller-supplied page/run extraction model shared by the export mappers.
//!
//! The Office, RTF, and plain-text export layers all need the same bridge from
//! "text recovered from a page" to a document model. This module defines that
//! bridge once: [`ExtractedRun`] carries text plus page geometry,
//! [`ExtractedPage`] groups runs, tables, and typed unsupported constructs, and
//! [`ExtractedDocument`] validates the bundle and derives deterministic reading
//! order, plain-text bytes, and RTF bytes.
//!
//! No PDF parsing happens here: the caller supplies already-extracted data, as
//! it does for `crate::TextExportPlan` and `crate::RtfOptions`. Ordering,
//! numbering, escaping, and encoding are library concerns and are deterministic;
//! the missing host-only step is CLI/UI wiring that supplies the extracted runs.

use crate::export_options::ExportOptionError;
use crate::rtf::{RtfOptions, RtfPage, export_rtf};
use crate::text::{TextExportPlan, export_text};

/// Maximum number of pages in one extracted document.
pub const EXTRACT_MAX_PAGES: usize = 10_000;
/// Maximum number of text runs on one page.
pub const EXTRACT_MAX_RUNS_PER_PAGE: usize = 100_000;
/// Maximum number of characters in one run, table cell, or unsupported detail.
pub const EXTRACT_MAX_TEXT_LEN: usize = 1_000_000;

/// A typed failure while validating an extracted document.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ExtractModelError {
    /// The document has no pages.
    #[error("extracted document has no pages")]
    EmptyDocument,
    /// The page count exceeds the documented limit.
    #[error("extracted document has more than {limit} pages")]
    TooManyPages {
        /// The enforced page limit.
        limit: usize,
    },
    /// The same zero-based page index appears twice.
    #[error("duplicate extracted page index {page}")]
    DuplicatePage {
        /// The duplicated page index.
        page: usize,
    },
    /// A page has non-positive or non-finite geometry.
    #[error("invalid extracted geometry on page {page}")]
    InvalidPageGeometry {
        /// The offending page index.
        page: usize,
    },
    /// A page carries more runs than the documented limit.
    #[error("page {page} has more than {limit} text runs")]
    TooManyRuns {
        /// The offending page index.
        page: usize,
        /// The enforced run limit.
        limit: usize,
    },
    /// A coordinate or font size is not finite.
    #[error("non-finite {field} on extracted page {page}")]
    NonFinite {
        /// The offending page index.
        page: usize,
        /// The field that failed validation.
        field: &'static str,
    },
    /// A text field is longer than the documented limit.
    #[error("extracted {field} on page {page} exceeds {limit} characters")]
    TextTooLong {
        /// The offending page index.
        page: usize,
        /// The field that failed validation.
        field: &'static str,
        /// The enforced character limit.
        limit: usize,
    },
}

/// A typed failure at an end-to-end text emitter boundary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum TextModelExportError {
    /// The extracted model failed validation.
    #[error(transparent)]
    Model(#[from] ExtractModelError),
    /// The caller's export options were invalid.
    #[error(transparent)]
    Options(#[from] ExportOptionError),
}

/// One run of text recovered from a page, with PDF-point geometry.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedRun {
    /// Recovered UTF-8 text.
    pub text: String,
    /// Left edge in PDF points.
    pub x: f32,
    /// Baseline in PDF points (larger values are higher on the page).
    pub y: f32,
    /// Recovered font size in PDF points.
    pub font_size: f32,
    /// Tie-breaker for runs with identical geometry; lower values come first.
    pub sequence: usize,
}

impl ExtractedRun {
    /// Create a run with `sequence` set to zero.
    #[must_use]
    pub fn new(text: impl Into<String>, x: f32, y: f32, font_size: f32) -> Self {
        Self {
            text: text.into(),
            x,
            y,
            font_size,
            sequence: 0,
        }
    }

    /// Attach a deterministic tie-breaker for identical geometry.
    #[must_use]
    pub const fn with_sequence(mut self, sequence: usize) -> Self {
        self.sequence = sequence;
        self
    }
}

/// A positioned text box recovered from a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedTextBox {
    /// Recovered UTF-8 text.
    pub text: String,
    /// Left edge in PDF points.
    pub x: f32,
    /// Bottom edge in PDF points.
    pub y: f32,
    /// Width in PDF points.
    pub width: f32,
    /// Height in PDF points.
    pub height: f32,
    /// Recovered font size in PDF points.
    pub font_size: f32,
}

impl ExtractedTextBox {
    /// Create a text box anchored at `(x, y)` with zero metrics.
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

    /// Attach width, height, and font metrics.
    #[must_use]
    pub const fn with_metrics(mut self, width: f32, height: f32, font_size: f32) -> Self {
        self.width = width;
        self.height = height;
        self.font_size = font_size;
        self
    }
}

/// A rectangular table recovered from a page.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedTable {
    /// Optional caller-supplied name used for sheet naming.
    pub name: Option<String>,
    /// Cell texts in reading order; a simple table is rectangular.
    pub rows: Vec<Vec<String>>,
}

impl ExtractedTable {
    /// Create a table from rows of cell text.
    #[must_use]
    pub fn new(rows: Vec<Vec<String>>) -> Self {
        Self { name: None, rows }
    }

    /// Attach a name used by spreadsheet sheet naming.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Return `true` when every row has the same number of cells.
    #[must_use]
    pub fn is_rectangular(&self) -> bool {
        self.rows
            .first()
            .is_none_or(|first| self.rows.iter().all(|row| row.len() == first.len()))
    }
}

/// A construct that the downstream document formats cannot represent faithfully.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtractedUnsupported {
    /// Vector art, a path, or a drawing without an editable equivalent.
    VectorArt {
        /// Short caller-supplied description.
        detail: String,
    },
    /// A merged, nested, or otherwise complex table.
    ComplexTable {
        /// Short caller-supplied description.
        detail: String,
    },
    /// Embedded audio, video, or other media.
    EmbeddedMedia {
        /// Short caller-supplied description.
        detail: String,
    },
}

impl ExtractedUnsupported {
    /// Return a stable machine-readable kind name.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::VectorArt { .. } => "vector-art",
            Self::ComplexTable { .. } => "complex-table",
            Self::EmbeddedMedia { .. } => "embedded-media",
        }
    }

    /// Return the caller-supplied detail text.
    #[must_use]
    pub fn detail(&self) -> &str {
        match self {
            Self::VectorArt { detail }
            | Self::ComplexTable { detail }
            | Self::EmbeddedMedia { detail } => detail,
        }
    }
}

/// One extracted page.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedPage {
    /// Zero-based page index.
    pub index: usize,
    /// Page width in PDF points.
    pub width: f32,
    /// Page height in PDF points.
    pub height: f32,
    /// Text runs recovered from the page.
    pub runs: Vec<ExtractedRun>,
    /// Simple rectangular tables recovered from the page.
    pub tables: Vec<ExtractedTable>,
    /// Positioned text boxes recovered from the page.
    pub text_boxes: Vec<ExtractedTextBox>,
    /// Typed constructs that cannot be represented faithfully.
    pub unsupported: Vec<ExtractedUnsupported>,
}

impl ExtractedPage {
    /// Create an empty page with the given index and geometry.
    #[must_use]
    pub fn new(index: usize, width: f32, height: f32) -> Self {
        Self {
            index,
            width,
            height,
            runs: Vec::new(),
            tables: Vec::new(),
            text_boxes: Vec::new(),
            unsupported: Vec::new(),
        }
    }

    /// Attach text runs.
    #[must_use]
    pub fn with_runs(mut self, runs: Vec<ExtractedRun>) -> Self {
        self.runs = runs;
        self
    }

    /// Attach one rectangular table.
    #[must_use]
    pub fn with_table(mut self, table: ExtractedTable) -> Self {
        self.tables.push(table);
        self
    }

    /// Attach one positioned text box.
    #[must_use]
    pub fn with_text_box(mut self, text_box: ExtractedTextBox) -> Self {
        self.text_boxes.push(text_box);
        self
    }

    /// Attach one typed unsupported construct.
    #[must_use]
    pub fn with_unsupported(mut self, item: ExtractedUnsupported) -> Self {
        self.unsupported.push(item);
        self
    }
}

/// A complete, caller-supplied extraction bundle.
#[derive(Debug, Clone, PartialEq)]
pub struct ExtractedDocument {
    /// Document title used by the downstream writers.
    pub title: String,
    /// Extracted pages in any order; output order is derived, not supplied.
    pub pages: Vec<ExtractedPage>,
}

impl ExtractedDocument {
    /// Create a bundle from a title and pages.
    #[must_use]
    pub fn new(title: impl Into<String>, pages: Vec<ExtractedPage>) -> Self {
        Self {
            title: title.into(),
            pages,
        }
    }

    /// Validate the bundle without emitting anything.
    ///
    /// # Errors
    ///
    /// Returns a typed [`ExtractModelError`] for an empty bundle, too many
    /// pages, a duplicate page index, invalid geometry, oversized text, or a
    /// non-finite coordinate/font size.
    pub fn validate(&self) -> Result<(), ExtractModelError> {
        if self.pages.is_empty() {
            return Err(ExtractModelError::EmptyDocument);
        }
        if self.pages.len() > EXTRACT_MAX_PAGES {
            return Err(ExtractModelError::TooManyPages {
                limit: EXTRACT_MAX_PAGES,
            });
        }
        if self.title.chars().count() > EXTRACT_MAX_TEXT_LEN {
            return Err(ExtractModelError::TextTooLong {
                page: 0,
                field: "title",
                limit: EXTRACT_MAX_TEXT_LEN,
            });
        }
        let mut seen = std::collections::HashSet::with_capacity(self.pages.len());
        for page in &self.pages {
            if !seen.insert(page.index) {
                return Err(ExtractModelError::DuplicatePage { page: page.index });
            }
            if !(page.width.is_finite()
                && page.width > 0.0
                && page.height.is_finite()
                && page.height > 0.0)
            {
                return Err(ExtractModelError::InvalidPageGeometry { page: page.index });
            }
            if page.runs.len() > EXTRACT_MAX_RUNS_PER_PAGE {
                return Err(ExtractModelError::TooManyRuns {
                    page: page.index,
                    limit: EXTRACT_MAX_RUNS_PER_PAGE,
                });
            }
            for run in &page.runs {
                for (value, field) in [
                    (run.x, "run x"),
                    (run.y, "run y"),
                    (run.font_size, "run font size"),
                ] {
                    if !value.is_finite() {
                        return Err(ExtractModelError::NonFinite {
                            page: page.index,
                            field,
                        });
                    }
                }
                if run.text.chars().count() > EXTRACT_MAX_TEXT_LEN {
                    return Err(ExtractModelError::TextTooLong {
                        page: page.index,
                        field: "run text",
                        limit: EXTRACT_MAX_TEXT_LEN,
                    });
                }
            }
            for text_box in &page.text_boxes {
                for (value, field) in [
                    (text_box.x, "text box x"),
                    (text_box.y, "text box y"),
                    (text_box.width, "text box width"),
                    (text_box.height, "text box height"),
                    (text_box.font_size, "text box font size"),
                ] {
                    if !value.is_finite() {
                        return Err(ExtractModelError::NonFinite {
                            page: page.index,
                            field,
                        });
                    }
                }
                if text_box.text.chars().count() > EXTRACT_MAX_TEXT_LEN {
                    return Err(ExtractModelError::TextTooLong {
                        page: page.index,
                        field: "text box text",
                        limit: EXTRACT_MAX_TEXT_LEN,
                    });
                }
            }
            for table in &page.tables {
                for row in &table.rows {
                    for cell in row {
                        if cell.chars().count() > EXTRACT_MAX_TEXT_LEN {
                            return Err(ExtractModelError::TextTooLong {
                                page: page.index,
                                field: "table cell",
                                limit: EXTRACT_MAX_TEXT_LEN,
                            });
                        }
                    }
                }
            }
            for item in &page.unsupported {
                if item.detail().chars().count() > EXTRACT_MAX_TEXT_LEN {
                    return Err(ExtractModelError::TextTooLong {
                        page: page.index,
                        field: "unsupported detail",
                        limit: EXTRACT_MAX_TEXT_LEN,
                    });
                }
            }
        }
        Ok(())
    }

    /// Return pages in ascending zero-based index order.
    #[must_use]
    pub fn sorted_pages(&self) -> Vec<&ExtractedPage> {
        let mut pages: Vec<&ExtractedPage> = self.pages.iter().collect();
        pages.sort_by_key(|page| page.index);
        pages
    }

    /// Return one page's runs in deterministic reading order.
    ///
    /// PDF origins are bottom-left, so larger `y` values come first. Ties are
    /// broken by ascending `x`, then by the caller's `sequence` tie-breaker, so
    /// shuffled input cannot change the order.
    #[must_use]
    pub fn ordered_runs(page: &ExtractedPage) -> Vec<&ExtractedRun> {
        let mut runs: Vec<&ExtractedRun> = page.runs.iter().collect();
        runs.sort_by(|left, right| {
            line_key(right.y)
                .cmp(&line_key(left.y))
                .then_with(|| float_key(left.x).cmp(&float_key(right.x)))
                .then_with(|| left.sequence.cmp(&right.sequence))
        });
        runs
    }

    /// Render one page's runs into text lines in deterministic reading order.
    ///
    /// Runs whose `y` values round to the same 0.01-point key form one line;
    /// within a line they are joined with a single space. Lines are joined with
    /// `\n` and no trailing newline is added.
    #[must_use]
    pub fn page_text(page: &ExtractedPage) -> String {
        let ordered = Self::ordered_runs(page);
        let mut lines: Vec<String> = Vec::new();
        let mut current_key = i64::MIN;
        let mut current = String::new();
        for run in ordered {
            let key = line_key(run.y);
            if !run.text.is_empty() && current_key != key {
                if !current.is_empty() {
                    lines.push(std::mem::take(&mut current));
                }
                current_key = key;
            }
            if !run.text.is_empty() {
                if !current.is_empty() {
                    current.push(' ');
                }
                current.push_str(&run.text);
            }
        }
        if !current.is_empty() {
            lines.push(current);
        }
        lines.join("\n")
    }

    /// Convert to `(zero-based page, text)` pairs for `crate::export_text`.
    #[must_use]
    pub fn to_text_pages(&self) -> Vec<(usize, String)> {
        self.sorted_pages()
            .into_iter()
            .map(|page| (page.index, Self::page_text(page)))
            .collect()
    }

    /// Convert to [`RtfPage`] values for `crate::export_rtf`.
    #[must_use]
    pub fn to_rtf_pages(&self) -> Vec<RtfPage> {
        self.sorted_pages()
            .into_iter()
            .map(|page| RtfPage::new(page.index, Self::page_text(page)))
            .collect()
    }

    /// Emit deterministic plain-text bytes using `plan` (UTF-8/UTF-16).
    ///
    /// # Errors
    ///
    /// Returns [`ExtractModelError`] when the model is invalid.
    pub fn emit_plain_text(&self, plan: &TextExportPlan) -> Result<Vec<u8>, ExtractModelError> {
        self.validate()?;
        let selection = plan.pages().clone();
        let text = export_text(
            self.to_text_pages().into_iter(),
            &plan.clone().with_pages(selection),
        );
        Ok(plan.encoding().encode(&text))
    }

    /// Emit deterministic RTF bytes using `options`.
    ///
    /// # Errors
    ///
    /// Returns a typed [`TextModelExportError`] for an invalid model or options.
    pub fn emit_rtf(&self, options: &RtfOptions) -> Result<Vec<u8>, TextModelExportError> {
        self.validate()?;
        Ok(export_rtf(self.to_rtf_pages().into_iter(), options)?)
    }
}

/// Quantize a PDF `y` coordinate to 0.01 points for stable line grouping.
fn line_key(y: f32) -> i64 {
    (f64::from(y) * 100.0).round() as i64
}

/// Total-order key for a finite `f32` (all non-finite values are rejected first).
fn float_key(value: f32) -> u32 {
    value.to_bits()
}

/// Emit extracted bytes as plain text using the caller's plan.
///
/// # Errors
///
/// Returns a typed [`ExtractModelError`] when the model is invalid.
pub fn emit_text_bytes(
    document: &ExtractedDocument,
    plan: &TextExportPlan,
) -> Result<Vec<u8>, ExtractModelError> {
    document.emit_plain_text(plan)
}

/// Emit extracted bytes as RTF using the caller's options.
///
/// # Errors
///
/// Returns a typed [`TextModelExportError`] for an invalid model or options.
pub fn emit_rtf_bytes(
    document: &ExtractedDocument,
    options: &RtfOptions,
) -> Result<Vec<u8>, TextModelExportError> {
    document.emit_rtf(options)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::text::TextEncoding;

    fn doc() -> ExtractedDocument {
        ExtractedDocument::new(
            "Report",
            vec![
                ExtractedPage::new(1, 200.0, 300.0)
                    .with_runs(vec![ExtractedRun::new("Second", 10.0, 280.0, 11.0)]),
                ExtractedPage::new(0, 200.0, 300.0).with_runs(vec![
                    ExtractedRun::new("world", 30.0, 280.0, 11.0).with_sequence(1),
                    ExtractedRun::new("Hello", 10.0, 280.0, 11.0).with_sequence(0),
                    ExtractedRun::new("tail", 10.0, 260.0, 11.0),
                ]),
            ],
        )
    }

    #[test]
    fn reading_order_sorts_top_to_bottom_then_left_to_right() {
        let page = ExtractedPage::new(0, 200.0, 300.0).with_runs(vec![
            ExtractedRun::new("low", 10.0, 100.0, 10.0),
            ExtractedRun::new("right", 40.0, 200.0, 10.0),
            ExtractedRun::new("left", 10.0, 200.0, 10.0),
        ]);
        assert_eq!(ExtractedDocument::page_text(&page), "left right\nlow");
    }

    #[test]
    fn reading_order_is_independent_of_input_order() {
        let first = doc();
        let mut second = doc();
        second.pages.reverse();
        for page in &mut second.pages {
            page.runs.reverse();
        }
        assert_eq!(first.to_text_pages(), second.to_text_pages());
        assert_eq!(first.to_text_pages()[0].1, "Hello world\ntail");
    }

    #[test]
    fn validation_rejects_empty_duplicate_and_non_finite_models() {
        assert_eq!(
            ExtractedDocument::new("T", Vec::new()).validate(),
            Err(ExtractModelError::EmptyDocument)
        );
        let page = ExtractedPage::new(0, 10.0, 10.0);
        assert_eq!(
            ExtractedDocument::new("T", vec![page.clone(), page]).validate(),
            Err(ExtractModelError::DuplicatePage { page: 0 })
        );
        let bad = ExtractedDocument::new("T", vec![ExtractedPage::new(0, f32::NAN, 10.0)]);
        assert_eq!(
            bad.validate(),
            Err(ExtractModelError::InvalidPageGeometry { page: 0 })
        );
        let non_finite = ExtractedDocument::new(
            "T",
            vec![
                ExtractedPage::new(0, 10.0, 10.0).with_runs(vec![ExtractedRun::new(
                    "x",
                    0.0,
                    f32::INFINITY,
                    10.0,
                )]),
            ],
        );
        assert!(matches!(
            non_finite.validate(),
            Err(ExtractModelError::NonFinite { .. })
        ));
    }

    #[test]
    fn table_rectangularity_and_unsupported_kinds_are_stable() {
        assert!(ExtractedTable::new(vec![vec!["a".to_owned()]]).is_rectangular());
        assert!(
            !ExtractedTable::new(vec![
                vec!["a".to_owned()],
                vec!["b".to_owned(), "c".to_owned()]
            ])
            .is_rectangular()
        );
        assert_eq!(
            ExtractedUnsupported::VectorArt {
                detail: "curve".to_owned()
            }
            .kind(),
            "vector-art"
        );
        assert_eq!(
            ExtractedUnsupported::EmbeddedMedia {
                detail: "audio".to_owned()
            }
            .detail(),
            "audio"
        );
    }

    #[test]
    fn plain_text_emitter_is_deterministic_and_honours_encoding() {
        let document = doc();
        let plan = crate::text::TextExportPlan::new().with_page_headers(true);
        let first = emit_text_bytes(&document, &plan).expect("plain text");
        assert_eq!(
            first,
            emit_text_bytes(&document, &plan).expect("plain text again")
        );
        let text = String::from_utf8(first).expect("UTF-8 by default");
        assert!(text.starts_with("--- Page 1 ---\nHello world\ntail"));
        assert!(text.contains("--- Page 2 ---\nSecond"));

        let utf16 = document
            .emit_plain_text(&plan.clone().with_encoding(TextEncoding::Utf16Le))
            .expect("UTF-16");
        assert_eq!(&utf16[..2], &[0xFF, 0xFE]);
    }

    #[test]
    fn rtf_emitter_is_deterministic_and_escapes_text() {
        let document = ExtractedDocument::new(
            "T",
            vec![
                ExtractedPage::new(0, 200.0, 300.0).with_runs(vec![ExtractedRun::new(
                    "a\\b {c} café",
                    10.0,
                    280.0,
                    11.0,
                )]),
            ],
        );
        let first = emit_rtf_bytes(&document, &RtfOptions::new()).expect("RTF");
        assert_eq!(
            first,
            emit_rtf_bytes(&document, &RtfOptions::new()).expect("RTF again")
        );
        let text = String::from_utf8(first).expect("ASCII RTF");
        assert!(text.contains("a\\\\b \\{c\\} caf\\u233?"));
        assert!(text.ends_with('}'));
    }

    #[test]
    fn emitters_return_typed_errors_for_bad_models_and_options() {
        let empty = ExtractedDocument::new("T", Vec::new());
        assert!(matches!(
            emit_text_bytes(&empty, &crate::text::TextExportPlan::new()),
            Err(ExtractModelError::EmptyDocument)
        ));
        assert!(matches!(
            emit_rtf_bytes(&empty, &RtfOptions::new()),
            Err(TextModelExportError::Model(
                ExtractModelError::EmptyDocument
            ))
        ));
        let bad_options = RtfOptions::new()
            .with_pages(crate::export_options::ExportPageSelection::Range { first: 5, last: 2 });
        assert!(matches!(
            emit_rtf_bytes(&doc(), &bad_options),
            Err(TextModelExportError::Options(_))
        ));
    }

    #[test]
    fn boundary_sizes_are_enforced() {
        let many_pages: Vec<ExtractedPage> = (0..EXTRACT_MAX_PAGES + 1)
            .map(|index| ExtractedPage::new(index, 10.0, 10.0))
            .collect();
        assert_eq!(
            ExtractedDocument::new("T", many_pages).validate(),
            Err(ExtractModelError::TooManyPages {
                limit: EXTRACT_MAX_PAGES
            })
        );
        let many_runs: Vec<ExtractedRun> = (0..EXTRACT_MAX_RUNS_PER_PAGE + 1)
            .map(|index| ExtractedRun::new(index.to_string(), 0.0, 0.0, 10.0))
            .collect();
        assert_eq!(
            ExtractedDocument::new(
                "T",
                vec![ExtractedPage::new(0, 10.0, 10.0).with_runs(many_runs)]
            )
            .validate(),
            Err(ExtractModelError::TooManyRuns {
                page: 0,
                limit: EXTRACT_MAX_RUNS_PER_PAGE
            })
        );
    }
}
