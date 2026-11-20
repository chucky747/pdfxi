//! Deterministic comparison of two extracted page-text documents (REVIEW-008).
//!
//! This module takes *already extracted* text, one entry per page, and produces
//! a stable list of added / removed / changed hunks with page and line
//! references plus a summary. It is renderer- and storage-agnostic: callers
//! supply the text they obtained from any extractor, and no PDF bytes are read
//! here. All output is bounded by [`CompareOptions`] so hostile or very large
//! inputs cannot drive unbounded memory or output; violations are typed
//! [`CompareError`]s.
//!
//! Pages are aligned by their numeric page index. Within a page, an LCS diff
//! groups adjacent removals and insertions into
//! [`HunkKind::Changed`] pairs, with leftover lines reported as Added or
//! Removed. Iteration is over sorted page numbers and ordered line vectors, so
//! repeated calls return byte-identical results and hunk order is stable.

use std::collections::BTreeMap;
use std::fmt;

/// Errors produced while validating or comparing text documents.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CompareError {
    /// The same page index appeared more than once in one document.
    DuplicatePage(usize),
    /// A document carried more pages than the configured limit.
    TooManyPages {
        /// Configured page limit.
        limit: usize,
    },
    /// A page carried more lines than the configured limit.
    TooManyLines {
        /// Page that exceeded the limit.
        page: usize,
        /// Configured line limit.
        limit: usize,
    },
    /// A line was longer than the configured byte limit.
    LineTooLong {
        /// Page holding the long line.
        page: usize,
        /// One-based line number.
        line: usize,
        /// Configured byte limit.
        limit: usize,
    },
    /// The LCS matrix would exceed the configured cell budget.
    TooLarge {
        /// Requested number of matrix cells.
        cells: usize,
        /// Configured cell budget.
        limit: usize,
    },
    /// Comparison would emit more hunks than the configured limit.
    OutputLimitExceeded(usize),
}

impl fmt::Display for CompareError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicatePage(page) => write!(f, "duplicate page index {page}"),
            Self::TooManyPages { limit } => write!(f, "document has more than {limit} pages"),
            Self::TooManyLines { page, limit } => {
                write!(f, "page {page} has more than {limit} lines")
            }
            Self::LineTooLong { page, line, limit } => {
                write!(f, "page {page} line {line} exceeds {limit} bytes")
            }
            Self::TooLarge { cells, limit } => {
                write!(f, "diff needs {cells} cells, above the {limit} cell budget")
            }
            Self::OutputLimitExceeded(limit) => {
                write!(f, "comparison exceeded {limit} hunks")
            }
        }
    }
}

impl std::error::Error for CompareError {}

/// Bounds applied to comparison input and output.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompareOptions {
    /// Maximum number of pages in either document.
    pub max_pages: usize,
    /// Maximum number of lines on one page.
    pub max_lines_per_page: usize,
    /// Maximum number of bytes in one line.
    pub max_line_bytes: usize,
    /// Maximum number of LCS matrix cells allocated for one page diff.
    pub max_diff_cells: usize,
    /// Maximum number of hunks emitted for the whole comparison.
    pub max_hunks: usize,
}

impl Default for CompareOptions {
    fn default() -> Self {
        Self {
            max_pages: 4096,
            max_lines_per_page: 1_000,
            max_line_bytes: 4_096,
            max_diff_cells: 1_000_000,
            max_hunks: 10_000,
        }
    }
}

impl CompareOptions {
    /// Overrides the hunk limit.
    pub fn with_max_hunks(mut self, max_hunks: usize) -> Self {
        self.max_hunks = max_hunks;
        self
    }

    /// Overrides the per-page line limit and the derived diff cell budget.
    pub fn with_max_lines_per_page(mut self, max_lines_per_page: usize) -> Self {
        self.max_lines_per_page = max_lines_per_page;
        self.max_diff_cells = self
            .max_diff_cells
            .min(max_lines_per_page.saturating_mul(max_lines_per_page));
        self
    }
}

/// Extracted text for one page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextPage {
    /// Zero-based page index this text was extracted from.
    pub page: usize,
    /// Lines of text in reading order, without trailing newline characters.
    pub lines: Vec<String>,
}

/// A document whose text has already been extracted, keyed by page.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct TextDocument {
    pages: Vec<TextPage>,
}

impl TextDocument {
    /// Builds a document from explicit page and line vectors.
    ///
    /// Page indices are validated for uniqueness here; size limits are checked
    /// by [`compare_documents`] against its [`CompareOptions`].
    pub fn new(pages: Vec<TextPage>) -> Result<Self, CompareError> {
        let mut seen = BTreeMap::new();
        for page in &pages {
            if seen.insert(page.page, ()).is_some() {
                return Err(CompareError::DuplicatePage(page.page));
            }
        }
        Ok(Self { pages })
    }

    /// Builds a document from `(page, text)` pairs, splitting each text on
    /// newlines and dropping a single trailing empty line.
    pub fn from_lines(entries: &[(usize, &str)]) -> Result<Self, CompareError> {
        let mut pages = Vec::with_capacity(entries.len());
        for (page, text) in entries {
            let mut lines: Vec<String> = text.split('\n').map(str::to_string).collect();
            if lines.last().is_some_and(String::is_empty) {
                lines.pop();
            }
            pages.push(TextPage { page: *page, lines });
        }
        Self::new(pages)
    }

    /// Pages in ascending page-index order.
    pub fn pages(&self) -> &[TextPage] {
        &self.pages
    }

    /// Total number of lines across all pages.
    pub fn line_count(&self) -> usize {
        self.pages.iter().map(|page| page.lines.len()).sum()
    }

    fn validate(&self, options: &CompareOptions) -> Result<(), CompareError> {
        if self.pages.len() > options.max_pages {
            return Err(CompareError::TooManyPages {
                limit: options.max_pages,
            });
        }
        for page in &self.pages {
            if page.lines.len() > options.max_lines_per_page {
                return Err(CompareError::TooManyLines {
                    page: page.page,
                    limit: options.max_lines_per_page,
                });
            }
            for (index, line) in page.lines.iter().enumerate() {
                if line.len() > options.max_line_bytes {
                    return Err(CompareError::LineTooLong {
                        page: page.page,
                        line: index + 1,
                        limit: options.max_line_bytes,
                    });
                }
            }
        }
        Ok(())
    }

    fn index(&self) -> BTreeMap<usize, &[String]> {
        self.pages
            .iter()
            .map(|page| (page.page, page.lines.as_slice()))
            .collect()
    }
}

/// Kind of change described by a [`Hunk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HunkKind {
    /// A line is present only in the new document.
    Added,
    /// A line is present only in the old document.
    Removed,
    /// A line in the old document was replaced by one in the new document.
    Changed,
}

impl HunkKind {
    /// Stable lowercase name used in summaries and diagnostics.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Added => "added",
            Self::Removed => "removed",
            Self::Changed => "changed",
        }
    }
}

/// One deterministic change hunk with page and line references.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hunk {
    /// What changed.
    pub kind: HunkKind,
    /// Page index the change occurred on.
    pub page: usize,
    /// One-based old line number, when the hunk has an old side.
    pub old_line: Option<usize>,
    /// One-based new line number, when the hunk has a new side.
    pub new_line: Option<usize>,
    /// Old line text, when the hunk has an old side.
    pub old_text: Option<String>,
    /// New line text, when the hunk has a new side.
    pub new_text: Option<String>,
}

/// Deterministic counts describing one comparison.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CompareSummary {
    /// Number of added hunks.
    pub added: usize,
    /// Number of removed hunks.
    pub removed: usize,
    /// Number of changed hunks.
    pub changed: usize,
    /// Number of distinct pages examined (union of both documents).
    pub pages_examined: usize,
    /// Total number of hunks emitted.
    pub hunks: usize,
    /// Total lines in the old document.
    pub old_lines: usize,
    /// Total lines in the new document.
    pub new_lines: usize,
}

/// Result of a deterministic document comparison.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareResult {
    /// Change hunks in deterministic page-then-line order.
    pub hunks: Vec<Hunk>,
    /// Counts summarizing the comparison.
    pub summary: CompareSummary,
}

/// Compares two extracted text documents (REVIEW-008).
///
/// Both documents are validated against `options` before any diff work. Pages
/// are matched by index: a page present only in `new` yields Added hunks, a
/// page present only in `old` yields Removed hunks, and shared pages are
/// diffed with an LCS whose adjacent removals/insertions become Changed hunks.
pub fn compare_documents(
    old: &TextDocument,
    new: &TextDocument,
    options: &CompareOptions,
) -> Result<CompareResult, CompareError> {
    old.validate(options)?;
    new.validate(options)?;

    let old_index = old.index();
    let new_index = new.index();
    let mut pages: Vec<usize> = old_index.keys().chain(new_index.keys()).copied().collect();
    pages.sort();
    pages.dedup();

    let pages_examined = pages.len();
    let mut hunks = Vec::new();
    for page in &pages {
        let old_lines = old_index.get(page).copied().unwrap_or(&[]);
        let new_lines = new_index.get(page).copied().unwrap_or(&[]);
        let page_hunks = diff_page(*page, old_lines, new_lines, options)?;
        for hunk in page_hunks {
            if hunks.len() >= options.max_hunks {
                return Err(CompareError::OutputLimitExceeded(options.max_hunks));
            }
            hunks.push(hunk);
        }
    }

    let mut added = 0;
    let mut removed = 0;
    let mut changed = 0;
    for hunk in &hunks {
        match hunk.kind {
            HunkKind::Added => added += 1,
            HunkKind::Removed => removed += 1,
            HunkKind::Changed => changed += 1,
        }
    }
    Ok(CompareResult {
        summary: CompareSummary {
            added,
            removed,
            changed,
            pages_examined,
            hunks: hunks.len(),
            old_lines: old.line_count(),
            new_lines: new.line_count(),
        },
        hunks,
    })
}

#[derive(Clone, Debug)]
enum Op {
    Equal,
    Added(String, usize),
    Removed(String, usize),
}

fn diff_page(
    page: usize,
    old_lines: &[String],
    new_lines: &[String],
    options: &CompareOptions,
) -> Result<Vec<Hunk>, CompareError> {
    let cells = old_lines.len().saturating_add(1) * new_lines.len().saturating_add(1);
    if cells > options.max_diff_cells {
        return Err(CompareError::TooLarge {
            cells,
            limit: options.max_diff_cells,
        });
    }
    let ops = lcs_ops(old_lines, new_lines);
    let mut hunks = Vec::new();
    let mut pending_removed: Vec<(usize, String)> = Vec::new();
    let mut pending_added: Vec<(usize, String)> = Vec::new();

    for op in ops {
        match op {
            Op::Equal => {
                flush_group(page, &mut pending_removed, &mut pending_added, &mut hunks);
            }
            Op::Removed(text, line) => pending_removed.push((line, text)),
            Op::Added(text, line) => pending_added.push((line, text)),
        }
    }
    flush_group(page, &mut pending_removed, &mut pending_added, &mut hunks);
    Ok(hunks)
}

fn flush_group(
    page: usize,
    removed: &mut Vec<(usize, String)>,
    added: &mut Vec<(usize, String)>,
    out: &mut Vec<Hunk>,
) {
    let pairs = removed.len().min(added.len());
    for index in 0..pairs {
        let (old_line, old_text) = &removed[index];
        let (new_line, new_text) = &added[index];
        out.push(Hunk {
            kind: HunkKind::Changed,
            page,
            old_line: Some(*old_line),
            new_line: Some(*new_line),
            old_text: Some(old_text.clone()),
            new_text: Some(new_text.clone()),
        });
    }
    for (line, text) in removed.iter().skip(pairs) {
        out.push(Hunk {
            kind: HunkKind::Removed,
            page,
            old_line: Some(*line),
            new_line: None,
            old_text: Some(text.clone()),
            new_text: None,
        });
    }
    for (line, text) in added.iter().skip(pairs) {
        out.push(Hunk {
            kind: HunkKind::Added,
            page,
            old_line: None,
            new_line: Some(*line),
            old_text: None,
            new_text: Some(text.clone()),
        });
    }
    removed.clear();
    added.clear();
}

fn lcs_ops(old_lines: &[String], new_lines: &[String]) -> Vec<Op> {
    let n = old_lines.len();
    let m = new_lines.len();
    let width = m + 1;
    let mut dp = vec![0u32; (n + 1) * width];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            dp[i * width + j] = if old_lines[i] == new_lines[j] {
                dp[(i + 1) * width + (j + 1)] + 1
            } else {
                dp[(i + 1) * width + j].max(dp[i * width + (j + 1)])
            };
        }
    }
    let mut ops = Vec::new();
    let (mut i, mut j) = (0usize, 0usize);
    while i < n && j < m {
        if old_lines[i] == new_lines[j] {
            ops.push(Op::Equal);
            i += 1;
            j += 1;
        } else if dp[(i + 1) * width + j] >= dp[i * width + (j + 1)] {
            ops.push(Op::Removed(old_lines[i].clone(), i + 1));
            i += 1;
        } else {
            ops.push(Op::Added(new_lines[j].clone(), j + 1));
            j += 1;
        }
    }
    while i < n {
        ops.push(Op::Removed(old_lines[i].clone(), i + 1));
        i += 1;
    }
    while j < m {
        ops.push(Op::Added(new_lines[j].clone(), j + 1));
        j += 1;
    }
    ops
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(entries: &[(usize, &str)]) -> TextDocument {
        TextDocument::from_lines(entries).expect("valid document")
    }

    #[test]
    fn identical_documents_produce_no_hunks() {
        let doc = document(&[(0, "alpha\nbeta\ngamma")]);
        let result = compare_documents(&doc, &doc, &CompareOptions::default()).expect("compare");
        assert!(result.hunks.is_empty());
        assert_eq!(result.summary.hunks, 0);
        assert_eq!(result.summary.old_lines, 3);
        assert_eq!(result.summary.new_lines, 3);
        assert_eq!(result.summary.pages_examined, 1);
    }

    #[test]
    fn added_and_removed_lines_carry_page_and_line_refs() {
        let old = document(&[(2, "keep\nold")]);
        let new = document(&[(2, "keep\nnew")]);
        let result = compare_documents(&old, &new, &CompareOptions::default()).expect("compare");
        assert_eq!(result.hunks.len(), 1);
        assert_eq!(result.hunks[0].kind, HunkKind::Changed);
        assert_eq!(result.hunks[0].page, 2);
        assert_eq!(result.hunks[0].old_line, Some(2));
        assert_eq!(result.hunks[0].new_line, Some(2));
        assert_eq!(result.hunks[0].old_text.as_deref(), Some("old"));
        assert_eq!(result.hunks[0].new_text.as_deref(), Some("new"));
        assert_eq!(result.summary.changed, 1);
    }

    #[test]
    fn pure_insertion_and_deletion_are_reported_as_added_and_removed() {
        let old = document(&[(0, "a\nc")]);
        let new = document(&[(0, "a\nb\nc\nd")]);
        let result = compare_documents(&old, &new, &CompareOptions::default()).expect("compare");
        let kinds: Vec<HunkKind> = result.hunks.iter().map(|h| h.kind).collect();
        assert_eq!(kinds, vec![HunkKind::Added, HunkKind::Added]);
        assert_eq!(result.hunks[0].new_text.as_deref(), Some("b"));
        assert_eq!(result.hunks[1].new_text.as_deref(), Some("d"));
        assert_eq!(result.summary.added, 2);

        let reversed = compare_documents(&new, &old, &CompareOptions::default()).expect("compare");
        assert_eq!(reversed.summary.removed, 2);
        assert_eq!(reversed.hunks[0].old_text.as_deref(), Some("b"));
    }

    #[test]
    fn page_only_in_one_document_yields_all_added_or_removed() {
        let old = document(&[(0, "only old\nsecond")]);
        let new = document(&[(1, "only new")]);
        let result = compare_documents(&old, &new, &CompareOptions::default()).expect("compare");
        assert_eq!(result.summary.pages_examined, 2);
        assert_eq!(result.hunks.len(), 3);
        assert_eq!(result.hunks[0].kind, HunkKind::Removed);
        assert_eq!(result.hunks[0].page, 0);
        assert_eq!(result.hunks[1].kind, HunkKind::Removed);
        assert_eq!(result.hunks[2].kind, HunkKind::Added);
        assert_eq!(result.hunks[2].page, 1);
        assert_eq!(result.hunks[2].new_line, Some(1));
    }

    #[test]
    fn hunks_are_in_deterministic_page_then_line_order() {
        let old = document(&[(3, "x\ny"), (1, "a\nb")]);
        let new = document(&[(1, "a\nB"), (3, "X\ny")]);
        let result = compare_documents(&old, &new, &CompareOptions::default()).expect("compare");
        let pages: Vec<usize> = result.hunks.iter().map(|h| h.page).collect();
        assert_eq!(pages, vec![1, 3]);
        assert_eq!(
            result,
            compare_documents(&old, &new, &CompareOptions::default()).expect("again")
        );
    }

    #[test]
    fn hunk_limit_is_a_typed_error() {
        let old = document(&[(0, "a\nb\nc")]);
        let new = document(&[(0, "x\ny\nz")]);
        let options = CompareOptions::default().with_max_hunks(2);
        assert_eq!(
            compare_documents(&old, &new, &options),
            Err(CompareError::OutputLimitExceeded(2))
        );
    }

    #[test]
    fn input_limits_are_typed_errors() {
        let many: Vec<(usize, String)> = (0..3).map(|p| (p, "a\n".to_string())).collect();
        let pages: Vec<TextPage> = many
            .into_iter()
            .map(|(page, text)| TextPage {
                page,
                lines: text.split('\n').map(str::to_string).collect(),
            })
            .collect();
        let doc = TextDocument::new(pages).expect("valid");
        let options = CompareOptions {
            max_pages: 2,
            ..CompareOptions::default()
        };
        assert_eq!(
            compare_documents(&doc, &doc, &options),
            Err(CompareError::TooManyPages { limit: 2 })
        );

        let long = document(&[(0, "0123456789")]);
        let options = CompareOptions {
            max_line_bytes: 4,
            ..CompareOptions::default()
        };
        assert_eq!(
            compare_documents(&long, &long, &options),
            Err(CompareError::LineTooLong {
                page: 0,
                line: 1,
                limit: 4
            })
        );
    }

    #[test]
    fn diff_cell_budget_and_duplicate_pages_are_typed() {
        let old = document(&[(0, "a\nb\nc")]);
        let new = document(&[(0, "x\ny\nz")]);
        let options = CompareOptions {
            max_diff_cells: 4,
            ..CompareOptions::default()
        };
        assert_eq!(
            compare_documents(&old, &new, &options),
            Err(CompareError::TooLarge {
                cells: 16,
                limit: 4
            })
        );
        let duplicate = TextDocument::new(vec![
            TextPage {
                page: 1,
                lines: vec!["a".to_string()],
            },
            TextPage {
                page: 1,
                lines: vec!["b".to_string()],
            },
        ]);
        assert_eq!(duplicate, Err(CompareError::DuplicatePage(1)));
    }

    #[test]
    fn hunk_kind_names_and_errors_are_descriptive() {
        assert_eq!(HunkKind::Added.as_str(), "added");
        assert_eq!(HunkKind::Removed.as_str(), "removed");
        assert_eq!(HunkKind::Changed.as_str(), "changed");
        assert!(!CompareError::DuplicatePage(7).to_string().is_empty());
        assert!(!CompareError::OutputLimitExceeded(3).to_string().is_empty());
    }

    #[test]
    fn from_lines_drops_only_a_single_trailing_empty_line() {
        let doc = document(&[(0, "a\n\n")]);
        assert_eq!(doc.pages()[0].lines, vec!["a".to_string(), String::new()]);
        assert_eq!(doc.line_count(), 2);
    }
}
