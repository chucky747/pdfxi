//! Side-by-side compare view model with a deterministic aligned row plan.
//!
//! [`CompareViewModel`] takes two caller-supplied panes of extracted page text
//! (a flat sequence of numbered lines) and produces a deterministic
//! [`AlignedRow`] plan with [`RowKind::Match`], [`RowKind::Insert`], and
//! [`RowKind::Delete`] markers via a longest-common-subsequence alignment. A
//! [`ScrollSync`] maps line indices in either pane to shared rows and back, so a
//! host can keep two scroll views in step. This is a pure model: no diff widget,
//! renderer, or filesystem access is involved. Rendering the rows and driving
//! scroll events remain explicit host boundaries.

// ---------------------------------------------------------------------------
// Bounds and errors
// ---------------------------------------------------------------------------

/// Maximum number of lines accepted in one compare pane.
pub const MAX_COMPARE_LINES: usize = 2_000;
/// Maximum number of dynamic-programming cells in an alignment.
pub const MAX_ALIGN_CELLS: usize = 4_000_000;

/// Typed errors produced while building or querying a [`CompareViewModel`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompareViewError {
    /// A pane carried more lines than the budget allows.
    #[error("compare pane '{pane}' has more than {limit} lines")]
    TooManyLines {
        /// Which pane was rejected (`left` or `right`).
        pane: &'static str,
        /// Maximum number of lines allowed.
        limit: usize,
    },
    /// A pane listed the same page number more than once.
    #[error("compare pane '{pane}' has duplicate page {page}")]
    DuplicatePage {
        /// Which pane was rejected (`left` or `right`).
        pane: &'static str,
        /// Page number that appeared twice.
        page: usize,
    },
    /// The alignment matrix would be too large.
    #[error("alignment too large (limit {limit} cells)")]
    TooManyCells {
        /// Maximum number of DP cells allowed.
        limit: usize,
    },
    /// A line index was outside the pane's line sequence.
    #[error("{pane} line index {index} out of range (length {len})")]
    LineOutOfRange {
        /// Which pane was queried (`left` or `right`).
        pane: &'static str,
        /// Line index that was requested.
        index: usize,
        /// Number of lines in the pane.
        len: usize,
    },
    /// A row index was outside the aligned plan.
    #[error("row index {index} out of range (rows {len})")]
    RowOutOfRange {
        /// Row index that was requested.
        index: usize,
        /// Number of rows in the plan.
        len: usize,
    },
    /// A page number was zero (pages are numbered from one).
    #[error("compare pane '{pane}' has invalid page number {page}")]
    InvalidPageNumber {
        /// Which pane was rejected (`left` or `right`).
        pane: &'static str,
        /// The invalid page number.
        page: usize,
    },
    /// Page numbers in a pane were not strictly increasing.
    #[error("compare pane '{pane}' has non-monotonic page numbers")]
    NonMonotonicPages {
        /// Which pane was rejected (`left` or `right`).
        pane: &'static str,
    },
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// One numbered page of caller-supplied extracted lines.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparePage {
    /// Caller-defined page number.
    pub number: usize,
    /// Extracted lines in reading order.
    pub lines: Vec<String>,
}

impl ComparePage {
    /// Create a page from a number and owned lines.
    pub fn new(number: usize, lines: Vec<String>) -> Self {
        Self { number, lines }
    }

    /// Create a page by splitting `text` on line feeds.
    pub fn from_text(number: usize, text: &str) -> Self {
        Self::new(number, text.lines().map(str::to_string).collect())
    }
}

/// One side of a comparison: an opaque label plus numbered pages.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ComparePaneInput {
    /// Opaque document label (shown by the host, never interpreted).
    pub name: String,
    /// Pages supplied by the caller.
    pub pages: Vec<ComparePage>,
}

impl ComparePaneInput {
    /// Validate and construct a pane. Duplicate page numbers are rejected so
    /// the two documents can be paired unambiguously.
    pub fn new(
        name: impl Into<String>,
        pages: Vec<ComparePage>,
        pane: &'static str,
    ) -> Result<Self, CompareViewError> {
        let mut seen = Vec::new();
        for page in &pages {
            if seen.contains(&page.number) {
                return Err(CompareViewError::DuplicatePage {
                    pane,
                    page: page.number,
                });
            }
            seen.push(page.number);
        }
        Ok(Self {
            name: name.into(),
            pages,
        })
    }
}

/// A single extracted line with its source page and line indices.
#[derive(Clone, Debug, PartialEq, Eq)]
struct SourceLine {
    page: usize,
    line: usize,
    flat: usize,
    text: String,
}

fn flatten(pane: &ComparePaneInput) -> Vec<SourceLine> {
    let mut pages: Vec<&ComparePage> = pane.pages.iter().collect();
    pages.sort_by_key(|page| page.number);
    let mut out = Vec::new();
    for page in pages {
        for (line, text) in page.lines.iter().enumerate() {
            out.push(SourceLine {
                page: page.number,
                line,
                flat: out.len(),
                text: text.clone(),
            });
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Outputs
// ---------------------------------------------------------------------------

/// How an aligned row relates the two panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RowKind {
    /// The same line is present in both panes.
    Match,
    /// The line is present only in the right pane.
    Insert,
    /// The line is present only in the left pane.
    Delete,
}

/// One line cell in an aligned row.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowCell {
    /// Source page number.
    pub page: usize,
    /// Zero-based line index within the page.
    pub line: usize,
    /// Zero-based line index across the whole pane (used for scroll sync).
    pub flat: usize,
    /// Line text.
    pub text: String,
}

impl RowCell {
    fn from_source(source: &SourceLine) -> Self {
        Self {
            page: source.page,
            line: source.line,
            flat: source.flat,
            text: source.text.clone(),
        }
    }
}

/// One deterministic row in the aligned compare plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AlignedRow {
    /// Zero-based row index.
    pub index: usize,
    /// Relationship between the panes for this row.
    pub kind: RowKind,
    /// Left-pane cell, or `None` for an insert.
    pub left: Option<RowCell>,
    /// Right-pane cell, or `None` for a delete.
    pub right: Option<RowCell>,
}

/// Counts of each row kind in a compare plan.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompareViewSummary {
    /// Number of matched rows.
    pub matches: usize,
    /// Number of inserted (right-only) rows.
    pub inserts: usize,
    /// Number of deleted (left-only) rows.
    pub deletes: usize,
}

impl CompareViewSummary {
    /// Total number of rows.
    pub fn total(&self) -> usize {
        self.matches + self.inserts + self.deletes
    }
}

// ---------------------------------------------------------------------------
// Page pairing
// ---------------------------------------------------------------------------

/// How one page in a pairing relates the two panes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PagePairKind {
    /// The same page number exists in both panes.
    Aligned,
    /// The page exists only in the right pane.
    Inserted,
    /// The page exists only in the left pane.
    Deleted,
}

/// One deterministic page-level pairing entry.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagePair {
    /// Relationship between the panes for this page.
    pub kind: PagePairKind,
    /// Left-pane page number, or `None` for an inserted page.
    pub left: Option<usize>,
    /// Right-pane page number, or `None` for a deleted page.
    pub right: Option<usize>,
}

/// Counts of each page-pair kind.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PagePairSummary {
    /// Number of pages present in both panes.
    pub aligned: usize,
    /// Number of right-only pages.
    pub inserted: usize,
    /// Number of left-only pages.
    pub deleted: usize,
}

impl PagePairSummary {
    /// Total number of page pairs.
    pub fn total(&self) -> usize {
        self.aligned + self.inserted + self.deleted
    }
}

/// A deterministic, page-level pairing of two paged inputs.
///
/// Page numbers are validated (non-zero, strictly increasing) and then paired
/// with a two-pointer merge. Because both inputs are strictly increasing, the
/// merge is the unique longest-common-subsequence pairing; ties cannot arise,
/// so iteration order and classification are stable and reproducible.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PagePairing {
    pairs: Vec<PagePair>,
    summary: PagePairSummary,
}

impl PagePairing {
    /// Validate and pair two ascending sequences of page numbers.
    pub fn build(left: &[usize], right: &[usize]) -> Result<Self, CompareViewError> {
        validate_page_numbers(left, "left")?;
        validate_page_numbers(right, "right")?;
        let mut pairs = Vec::with_capacity(left.len() + right.len());
        let mut summary = PagePairSummary::default();
        let (mut i, mut j) = (0usize, 0usize);
        while i < left.len() && j < right.len() {
            let (kind, left_page, right_page, advance_left, advance_right) = if left[i] == right[j]
            {
                (
                    PagePairKind::Aligned,
                    Some(left[i]),
                    Some(right[j]),
                    true,
                    true,
                )
            } else if left[i] < right[j] {
                (PagePairKind::Deleted, Some(left[i]), None, true, false)
            } else {
                (PagePairKind::Inserted, None, Some(right[j]), false, true)
            };
            push_page_pair(&mut pairs, &mut summary, kind, left_page, right_page);
            if advance_left {
                i += 1;
            }
            if advance_right {
                j += 1;
            }
        }
        while i < left.len() {
            push_page_pair(
                &mut pairs,
                &mut summary,
                PagePairKind::Deleted,
                Some(left[i]),
                None,
            );
            i += 1;
        }
        while j < right.len() {
            push_page_pair(
                &mut pairs,
                &mut summary,
                PagePairKind::Inserted,
                None,
                Some(right[j]),
            );
            j += 1;
        }
        Ok(Self { pairs, summary })
    }

    /// Page pairs in ascending page order.
    pub fn pairs(&self) -> &[PagePair] {
        &self.pairs
    }

    /// Page-pair counts.
    pub fn summary(&self) -> PagePairSummary {
        self.summary
    }

    /// Whether every page was aligned and no page was inserted or deleted.
    pub fn is_aligned(&self) -> bool {
        self.summary.inserted == 0 && self.summary.deleted == 0
    }

    /// A deterministic, human-readable page summary.
    pub fn render_summary(&self) -> String {
        format!(
            "aligned={} inserted={} deleted={} pages={}",
            self.summary.aligned,
            self.summary.inserted,
            self.summary.deleted,
            self.summary.total()
        )
    }
}

/// Validate one pane's page-number sequence.
fn validate_page_numbers(pages: &[usize], pane: &'static str) -> Result<(), CompareViewError> {
    let mut previous: Option<usize> = None;
    for &page in pages {
        if page == 0 {
            return Err(CompareViewError::InvalidPageNumber { pane, page });
        }
        if let Some(earlier) = previous {
            if page == earlier {
                return Err(CompareViewError::DuplicatePage { pane, page });
            }
            if page < earlier {
                return Err(CompareViewError::NonMonotonicPages { pane });
            }
        }
        previous = Some(page);
    }
    Ok(())
}

/// Append one page pair and bump its summary counter.
fn push_page_pair(
    pairs: &mut Vec<PagePair>,
    summary: &mut PagePairSummary,
    kind: PagePairKind,
    left: Option<usize>,
    right: Option<usize>,
) {
    pairs.push(PagePair { kind, left, right });
    match kind {
        PagePairKind::Aligned => summary.aligned += 1,
        PagePairKind::Inserted => summary.inserted += 1,
        PagePairKind::Deleted => summary.deleted += 1,
    }
}

/// A deterministic mapping between line indices in the two panes.
///
/// A host calls [`Self::sync_left_to_right`] (or the reverse) when one pane
/// scrolls, then positions the other pane at the returned line. Rows without a
/// cell in the target pane map to `None`, which the host can treat as a gap.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ScrollSync {
    left_len: usize,
    right_len: usize,
    left_line_to_row: Vec<usize>,
    right_line_to_row: Vec<usize>,
    row_to_left_line: Vec<Option<usize>>,
    row_to_right_line: Vec<Option<usize>>,
}

impl ScrollSync {
    /// Number of lines in the left pane.
    pub fn left_len(&self) -> usize {
        self.left_len
    }

    /// Number of lines in the right pane.
    pub fn right_len(&self) -> usize {
        self.right_len
    }

    /// Number of aligned rows.
    pub fn row_count(&self) -> usize {
        self.row_to_left_line.len()
    }

    /// Aligned row for a left-pane line index.
    pub fn row_for_left_line(&self, line: usize) -> Result<usize, CompareViewError> {
        self.left_line_to_row
            .get(line)
            .copied()
            .ok_or(CompareViewError::LineOutOfRange {
                pane: "left",
                index: line,
                len: self.left_len,
            })
    }

    /// Aligned row for a right-pane line index.
    pub fn row_for_right_line(&self, line: usize) -> Result<usize, CompareViewError> {
        self.right_line_to_row
            .get(line)
            .copied()
            .ok_or(CompareViewError::LineOutOfRange {
                pane: "right",
                index: line,
                len: self.right_len,
            })
    }

    /// Left-pane line at an aligned row, if that row has a left cell.
    pub fn left_line_at_row(&self, row: usize) -> Result<Option<usize>, CompareViewError> {
        self.row_to_left_line
            .get(row)
            .copied()
            .ok_or(CompareViewError::RowOutOfRange {
                index: row,
                len: self.row_count(),
            })
    }

    /// Right-pane line at an aligned row, if that row has a right cell.
    pub fn right_line_at_row(&self, row: usize) -> Result<Option<usize>, CompareViewError> {
        self.row_to_right_line
            .get(row)
            .copied()
            .ok_or(CompareViewError::RowOutOfRange {
                index: row,
                len: self.row_count(),
            })
    }

    /// Map a left-pane line to the aligned right-pane line, if one exists.
    pub fn sync_left_to_right(&self, line: usize) -> Result<Option<usize>, CompareViewError> {
        let row = self.row_for_left_line(line)?;
        self.right_line_at_row(row)
    }

    /// Map a right-pane line to the aligned left-pane line, if one exists.
    pub fn sync_right_to_left(&self, line: usize) -> Result<Option<usize>, CompareViewError> {
        let row = self.row_for_right_line(line)?;
        self.left_line_at_row(row)
    }
}

/// A deterministic side-by-side compare plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareViewModel {
    left_name: String,
    right_name: String,
    rows: Vec<AlignedRow>,
    summary: CompareViewSummary,
}

impl CompareViewModel {
    /// Align two panes into a deterministic row plan.
    ///
    /// Pages in each pane are ordered by page number before flattening, so the
    /// result does not depend on the order pages were supplied. Lines are
    /// compared by exact text. Ties in the subsequence alignment are resolved
    /// with a fixed rule (prefer a deletion on equality of scores), which makes
    /// the output byte-for-byte reproducible.
    pub fn new(
        left: &ComparePaneInput,
        right: &ComparePaneInput,
    ) -> Result<Self, CompareViewError> {
        let left_lines = flatten(left);
        let right_lines = flatten(right);
        if left_lines.len() > MAX_COMPARE_LINES {
            return Err(CompareViewError::TooManyLines {
                pane: "left",
                limit: MAX_COMPARE_LINES,
            });
        }
        if right_lines.len() > MAX_COMPARE_LINES {
            return Err(CompareViewError::TooManyLines {
                pane: "right",
                limit: MAX_COMPARE_LINES,
            });
        }
        let cells = (left_lines.len() + 1).saturating_mul(right_lines.len() + 1);
        if cells > MAX_ALIGN_CELLS {
            return Err(CompareViewError::TooManyCells {
                limit: MAX_ALIGN_CELLS,
            });
        }

        let n = left_lines.len();
        let m = right_lines.len();
        let mut dp = vec![vec![0usize; m + 1]; n + 1];
        for i in (0..n).rev() {
            for j in (0..m).rev() {
                dp[i][j] = if left_lines[i].text == right_lines[j].text {
                    dp[i + 1][j + 1] + 1
                } else {
                    dp[i + 1][j].max(dp[i][j + 1])
                };
            }
        }

        let mut rows: Vec<AlignedRow> = Vec::with_capacity(n + m);
        let mut summary = CompareViewSummary::default();
        let (mut i, mut j) = (0usize, 0usize);
        while i < n && j < m {
            if left_lines[i].text == right_lines[j].text {
                push_row(
                    &mut rows,
                    &mut summary,
                    RowKind::Match,
                    Some(&left_lines[i]),
                    Some(&right_lines[j]),
                );
                i += 1;
                j += 1;
            } else if dp[i + 1][j] >= dp[i][j + 1] {
                push_row(
                    &mut rows,
                    &mut summary,
                    RowKind::Delete,
                    Some(&left_lines[i]),
                    None,
                );
                i += 1;
            } else {
                push_row(
                    &mut rows,
                    &mut summary,
                    RowKind::Insert,
                    None,
                    Some(&right_lines[j]),
                );
                j += 1;
            }
        }
        while i < n {
            push_row(
                &mut rows,
                &mut summary,
                RowKind::Delete,
                Some(&left_lines[i]),
                None,
            );
            i += 1;
        }
        while j < m {
            push_row(
                &mut rows,
                &mut summary,
                RowKind::Insert,
                None,
                Some(&right_lines[j]),
            );
            j += 1;
        }

        Ok(Self {
            left_name: left.name.clone(),
            right_name: right.name.clone(),
            rows,
            summary,
        })
    }

    /// Opaque label of the left pane.
    pub fn left_name(&self) -> &str {
        &self.left_name
    }

    /// Opaque label of the right pane.
    pub fn right_name(&self) -> &str {
        &self.right_name
    }

    /// Aligned rows in order.
    pub fn rows(&self) -> &[AlignedRow] {
        &self.rows
    }

    /// Row-kind counts.
    pub fn summary(&self) -> CompareViewSummary {
        self.summary
    }

    /// Whether the two panes had identical extracted lines.
    pub fn is_equal(&self) -> bool {
        self.summary.inserts == 0 && self.summary.deletes == 0
    }

    /// A deterministic, human-readable summary line suitable for a status bar.
    pub fn render_summary(&self) -> String {
        format!(
            "matches={} inserts={} deletes={} rows={}",
            self.summary.matches,
            self.summary.inserts,
            self.summary.deletes,
            self.summary.total()
        )
    }

    /// Derive the page-level pairing from the row plan.
    ///
    /// Rows are emitted in ascending page order, so collecting the first
    /// occurrence of each left/right page yields validated, strictly increasing
    /// sequences. Pages with no extracted lines are not represented.
    pub fn page_pairing(&self) -> Result<PagePairing, CompareViewError> {
        let mut left_pages = Vec::new();
        let mut right_pages = Vec::new();
        for row in &self.rows {
            if let Some(cell) = &row.left
                && left_pages.last() != Some(&cell.page)
            {
                left_pages.push(cell.page);
            }
            if let Some(cell) = &row.right
                && right_pages.last() != Some(&cell.page)
            {
                right_pages.push(cell.page);
            }
        }
        PagePairing::build(&left_pages, &right_pages)
    }

    /// Build the scroll-sync mapping for this plan.
    pub fn scroll_sync(&self) -> ScrollSync {
        let mut left_line_to_row = vec![0usize; self.summary.matches + self.summary.deletes];
        let mut right_line_to_row = vec![0usize; self.summary.matches + self.summary.inserts];
        let mut row_to_left_line = Vec::with_capacity(self.rows.len());
        let mut row_to_right_line = Vec::with_capacity(self.rows.len());
        for row in &self.rows {
            let left_line = row.left.as_ref().map(|cell| cell.flat);
            let right_line = row.right.as_ref().map(|cell| cell.flat);
            if let Some(index) = left_line
                && let Some(slot) = left_line_to_row.get_mut(index)
            {
                *slot = row.index;
            }
            if let Some(index) = right_line
                && let Some(slot) = right_line_to_row.get_mut(index)
            {
                *slot = row.index;
            }
            row_to_left_line.push(left_line);
            row_to_right_line.push(right_line);
        }
        ScrollSync {
            left_len: left_line_to_row.len(),
            right_len: right_line_to_row.len(),
            left_line_to_row,
            right_line_to_row,
            row_to_left_line,
            row_to_right_line,
        }
    }
}

/// Append one aligned row and bump its summary counter.
fn push_row(
    rows: &mut Vec<AlignedRow>,
    summary: &mut CompareViewSummary,
    kind: RowKind,
    left: Option<&SourceLine>,
    right: Option<&SourceLine>,
) {
    let index = rows.len();
    rows.push(AlignedRow {
        index,
        kind,
        left: left.map(RowCell::from_source),
        right: right.map(RowCell::from_source),
    });
    match kind {
        RowKind::Match => summary.matches += 1,
        RowKind::Insert => summary.inserts += 1,
        RowKind::Delete => summary.deletes += 1,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pane(name: &str, pages: Vec<(usize, Vec<&str>)>, side: &'static str) -> ComparePaneInput {
        let pages = pages
            .into_iter()
            .map(|(number, lines)| {
                ComparePage::new(number, lines.into_iter().map(str::to_string).collect())
            })
            .collect();
        ComparePaneInput::new(name, pages, side).expect("valid test pane")
    }

    #[test]
    fn identical_panes_produce_only_matches() {
        let left = pane("a", vec![(1, vec!["one", "two"])], "left");
        let right = pane("b", vec![(1, vec!["one", "two"])], "right");
        let model = CompareViewModel::new(&left, &right).expect("model");
        assert!(model.is_equal());
        assert_eq!(
            model.rows().iter().map(|row| row.kind).collect::<Vec<_>>(),
            vec![RowKind::Match, RowKind::Match]
        );
        assert_eq!(
            model.render_summary(),
            "matches=2 inserts=0 deletes=0 rows=2"
        );
    }

    #[test]
    fn alignment_detects_inserted_and_deleted_lines() {
        let left = pane("old", vec![(1, vec!["a", "b", "c"])], "left");
        let right = pane("new", vec![(1, vec!["a", "X", "c", "Y"])], "right");
        let model = CompareViewModel::new(&left, &right).expect("model");
        let kinds: Vec<RowKind> = model.rows().iter().map(|row| row.kind).collect();
        assert_eq!(
            kinds,
            vec![
                RowKind::Match,  // a
                RowKind::Delete, // b
                RowKind::Insert, // X
                RowKind::Match,  // c
                RowKind::Insert, // Y
            ]
        );
        assert_eq!(
            model.summary(),
            CompareViewSummary {
                matches: 2,
                inserts: 2,
                deletes: 1
            }
        );
        assert_eq!(
            model.rows()[1].left.as_ref().map(|c| c.text.as_str()),
            Some("b")
        );
        assert_eq!(model.rows()[1].right, None);
        assert_eq!(
            model.rows()[2].right.as_ref().map(|c| c.text.as_str()),
            Some("X")
        );
        assert_eq!(model.rows()[2].left, None);
    }

    #[test]
    fn row_cells_carry_page_and_line_metadata() {
        let left = pane("l", vec![(2, vec!["p2a", "p2b"]), (5, vec!["p5a"])], "left");
        let right = pane(
            "r",
            vec![(5, vec!["p5a"]), (2, vec!["p2a", "p2b"])],
            "right",
        );
        // Pages are sorted by number before flattening, so both panes align.
        let model = CompareViewModel::new(&left, &right).expect("model");
        assert!(model.is_equal());
        let cells: Vec<(usize, usize, &str)> = model
            .rows()
            .iter()
            .map(|row| {
                let cell = row.left.as_ref().expect("left cell");
                (cell.page, cell.line, cell.text.as_str())
            })
            .collect();
        assert_eq!(cells, vec![(2, 0, "p2a"), (2, 1, "p2b"), (5, 0, "p5a")]);
    }

    #[test]
    fn scroll_sync_maps_lines_between_panes() {
        let left = pane("old", vec![(1, vec!["a", "b", "c"])], "left");
        let right = pane("new", vec![(1, vec!["a", "X", "c", "Y"])], "right");
        let model = CompareViewModel::new(&left, &right).expect("model");
        let sync = model.scroll_sync();
        assert_eq!(sync.left_len(), 3);
        assert_eq!(sync.right_len(), 4);
        assert_eq!(sync.row_count(), 5);
        // left line 0 ("a") aligns with right line 0.
        assert_eq!(sync.row_for_left_line(0), Ok(0));
        assert_eq!(sync.sync_left_to_right(0), Ok(Some(0)));
        // left line 1 ("b") is a delete: no right cell.
        assert_eq!(sync.sync_left_to_right(1), Ok(None));
        // right line 1 ("X") is an insert: no left cell.
        assert_eq!(sync.sync_right_to_left(1), Ok(None));
        // left line 2 ("c") aligns with right line 2.
        assert_eq!(sync.sync_left_to_right(2), Ok(Some(2)));
        assert_eq!(sync.sync_right_to_left(3), Ok(None));
        // Row-level lookups round-trip.
        assert_eq!(sync.right_line_at_row(0), Ok(Some(0)));
        assert_eq!(sync.left_line_at_row(2), Ok(None));
    }

    #[test]
    fn scroll_sync_uses_global_line_indices_across_pages() {
        let left = pane("l", vec![(1, vec!["a", "b"]), (2, vec!["c"])], "left");
        let right = pane("r", vec![(1, vec!["a", "b"]), (2, vec!["c"])], "right");
        let model = CompareViewModel::new(&left, &right).expect("model");
        let sync = model.scroll_sync();
        assert_eq!(sync.left_len(), 3);
        // Global line 2 is page 2's "c"; its row is 2.
        assert_eq!(sync.row_for_left_line(2), Ok(2));
        assert_eq!(sync.sync_left_to_right(2), Ok(Some(2)));
        assert_eq!(
            model.rows()[2]
                .left
                .as_ref()
                .map(|cell| (cell.page, cell.line, cell.flat)),
            Some((2, 0, 2))
        );
    }

    #[test]
    fn scroll_sync_rejects_out_of_range_lines_and_rows() {
        let left = pane("l", vec![(1, vec!["a"])], "left");
        let right = pane("r", vec![(1, vec!["a"])], "right");
        let model = CompareViewModel::new(&left, &right).expect("model");
        let sync = model.scroll_sync();
        assert_eq!(
            sync.row_for_left_line(5),
            Err(CompareViewError::LineOutOfRange {
                pane: "left",
                index: 5,
                len: 1
            })
        );
        assert_eq!(
            sync.sync_right_to_left(9),
            Err(CompareViewError::LineOutOfRange {
                pane: "right",
                index: 9,
                len: 1
            })
        );
        assert_eq!(
            sync.left_line_at_row(4),
            Err(CompareViewError::RowOutOfRange { index: 4, len: 1 })
        );
    }

    #[test]
    fn duplicate_pages_are_a_typed_mismatched_input_error() {
        let bad = ComparePaneInput::new(
            "bad",
            vec![
                ComparePage::from_text(1, "a"),
                ComparePage::from_text(1, "b"),
            ],
            "left",
        );
        assert_eq!(
            bad,
            Err(CompareViewError::DuplicatePage {
                pane: "left",
                page: 1
            })
        );
    }

    #[test]
    fn over_budget_lines_are_rejected() {
        let lines: Vec<String> = (0..MAX_COMPARE_LINES + 1).map(|i| i.to_string()).collect();
        let left =
            ComparePaneInput::new("huge", vec![ComparePage::new(1, lines)], "left").expect("pane");
        let right = pane("r", vec![(1, vec!["a"])], "right");
        assert_eq!(
            CompareViewModel::new(&left, &right),
            Err(CompareViewError::TooManyLines {
                pane: "left",
                limit: MAX_COMPARE_LINES
            })
        );
    }

    #[test]
    fn alignment_is_deterministic_regardless_of_page_input_order() {
        let left_a = pane("l", vec![(2, vec!["c"]), (1, vec!["a", "b"])], "left");
        let left_b = pane("l", vec![(1, vec!["a", "b"]), (2, vec!["c"])], "left");
        let right = pane("r", vec![(1, vec!["a", "X"]), (2, vec!["c"])], "right");
        assert_eq!(
            CompareViewModel::new(&left_a, &right).expect("model a"),
            CompareViewModel::new(&left_b, &right).expect("model b")
        );
    }

    #[test]
    fn repeated_build_is_byte_for_byte_stable() {
        let left = pane("l", vec![(1, vec!["a", "b", "a", "c"])], "left");
        let right = pane("r", vec![(1, vec!["a", "c", "b", "a"])], "right");
        let first = CompareViewModel::new(&left, &right).expect("model");
        assert_eq!(first, CompareViewModel::new(&left, &right).expect("model"));
        assert_eq!(first.summary().total(), first.rows().len());
    }

    #[test]
    fn empty_panes_are_valid_and_produce_only_inserts_or_deletes() {
        let empty = pane("empty", vec![], "left");
        let full = pane("full", vec![(1, vec!["a", "b"])], "right");
        let model = CompareViewModel::new(&empty, &full).expect("model");
        assert_eq!(
            model.summary(),
            CompareViewSummary {
                matches: 0,
                inserts: 2,
                deletes: 0
            }
        );
        assert!(!model.is_equal());
        let reverse = CompareViewModel::new(&full, &empty).expect("model");
        assert_eq!(
            reverse.summary(),
            CompareViewSummary {
                matches: 0,
                inserts: 0,
                deletes: 2
            }
        );
    }

    fn kinds(pairing: &PagePairing) -> Vec<(PagePairKind, Option<usize>, Option<usize>)> {
        pairing
            .pairs()
            .iter()
            .map(|pair| (pair.kind, pair.left, pair.right))
            .collect()
    }

    #[test]
    fn page_pairing_aligns_inserts_and_deletes_pages() {
        let pairing = PagePairing::build(&[1, 2, 4], &[1, 3, 4]).expect("pairing");
        assert_eq!(
            kinds(&pairing),
            vec![
                (PagePairKind::Aligned, Some(1), Some(1)),
                (PagePairKind::Deleted, Some(2), None),
                (PagePairKind::Inserted, None, Some(3)),
                (PagePairKind::Aligned, Some(4), Some(4)),
            ]
        );
        assert_eq!(
            pairing.summary(),
            PagePairSummary {
                aligned: 2,
                inserted: 1,
                deleted: 1
            }
        );
        assert!(!pairing.is_aligned());
        assert_eq!(
            pairing.render_summary(),
            "aligned=2 inserted=1 deleted=1 pages=4"
        );
    }

    #[test]
    fn page_pairing_is_stable_for_identical_and_empty_inputs() {
        let same = PagePairing::build(&[2, 5], &[2, 5]).expect("pairing");
        assert!(same.is_aligned());
        assert_eq!(
            same.render_summary(),
            "aligned=2 inserted=0 deleted=0 pages=2"
        );
        let empty_left = PagePairing::build(&[], &[7]).expect("pairing");
        assert_eq!(
            kinds(&empty_left),
            vec![(PagePairKind::Inserted, None, Some(7))]
        );
        let empty_both = PagePairing::build(&[], &[]).expect("pairing");
        assert_eq!(empty_both.pairs().len(), 0);
        assert_eq!(empty_both.summary().total(), 0);
        assert_eq!(
            PagePairing::build(&[1, 2, 4], &[1, 3, 4]).expect("repeat"),
            PagePairing::build(&[1, 2, 4], &[1, 3, 4]).expect("repeat")
        );
    }

    #[test]
    fn page_pairing_rejects_zero_duplicate_and_non_monotonic_pages() {
        assert_eq!(
            PagePairing::build(&[0], &[]),
            Err(CompareViewError::InvalidPageNumber {
                pane: "left",
                page: 0
            })
        );
        assert_eq!(
            PagePairing::build(&[], &[3, 0]),
            Err(CompareViewError::InvalidPageNumber {
                pane: "right",
                page: 0
            })
        );
        assert_eq!(
            PagePairing::build(&[2, 2], &[]),
            Err(CompareViewError::DuplicatePage {
                pane: "left",
                page: 2
            })
        );
        assert_eq!(
            PagePairing::build(&[], &[4, 2]),
            Err(CompareViewError::NonMonotonicPages { pane: "right" })
        );
    }

    #[test]
    fn view_model_page_pairing_matches_pane_page_numbers() {
        let left = pane(
            "l",
            vec![(4, vec!["x"]), (1, vec!["a"]), (2, vec!["b"])],
            "left",
        );
        let right = pane(
            "r",
            vec![(3, vec!["y"]), (4, vec!["x"]), (1, vec!["a"])],
            "right",
        );
        let model = CompareViewModel::new(&left, &right).expect("model");
        let pairing = model.page_pairing().expect("pairing");
        assert_eq!(
            kinds(&pairing),
            vec![
                (PagePairKind::Aligned, Some(1), Some(1)),
                (PagePairKind::Deleted, Some(2), None),
                (PagePairKind::Inserted, None, Some(3)),
                (PagePairKind::Aligned, Some(4), Some(4)),
            ]
        );
        assert_eq!(pairing.summary().total(), 4);
    }
}
