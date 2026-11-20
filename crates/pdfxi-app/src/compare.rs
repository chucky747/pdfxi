//! Version compare over two page-text snapshots.
//!
//! [`compare_snapshots`] compares an older and a newer [`TextSnapshot`] and
//! produces deterministic added/removed/changed hunks, each referencing its
//! page, plus a [`CompareSummary`]. Hunks are ordered by page number, so the
//! output is independent of the order in which pages were supplied. This is a
//! pure model: no diff widget, renderer, or filesystem access is involved; the
//! UI can consume the hunks to draw whatever view it needs.

use std::collections::BTreeMap;

use crate::page_text::PageText;

/// Maximum number of pages accepted in a [`TextSnapshot`].
pub const MAX_SNAPSHOT_PAGES: usize = 10_000;

/// Typed errors produced while building a [`TextSnapshot`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CompareError {
    /// Two pages shared the same page number.
    #[error("duplicate page number in snapshot: {0}")]
    DuplicatePage(usize),
    /// Too many pages were supplied.
    #[error("too many pages in snapshot (limit {limit})")]
    TooManyPages {
        /// Maximum number of pages allowed.
        limit: usize,
    },
}

/// An immutable, validated snapshot of a document's page text.
///
/// Pages are sorted by page number and must have unique numbers. The snapshot
/// owns cloned page text, so later changes to the caller's data cannot alter a
/// comparison.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TextSnapshot {
    pages: Vec<PageText>,
}

impl TextSnapshot {
    /// Build a snapshot from pages, rejecting duplicate page numbers and
    /// enforcing [`MAX_SNAPSHOT_PAGES`].
    pub fn new(pages: Vec<PageText>) -> Result<Self, CompareError> {
        if pages.len() > MAX_SNAPSHOT_PAGES {
            return Err(CompareError::TooManyPages {
                limit: MAX_SNAPSHOT_PAGES,
            });
        }
        let mut sorted = pages;
        sorted.sort_by_key(PageText::page);
        let mut previous: Option<usize> = None;
        for page in &sorted {
            let number = page.page();
            if previous == Some(number) {
                return Err(CompareError::DuplicatePage(number));
            }
            previous = Some(number);
        }
        Ok(Self { pages: sorted })
    }

    /// Build a snapshot from `(page_number, text)` pairs.
    pub fn from_plain(
        pages: impl IntoIterator<Item = (usize, String)>,
    ) -> Result<Self, CompareError> {
        Self::new(
            pages
                .into_iter()
                .map(|(page, text)| PageText::new(page, text))
                .collect(),
        )
    }

    /// Return the pages, ordered by ascending page number.
    pub fn pages(&self) -> &[PageText] {
        &self.pages
    }

    /// Return the number of pages in this snapshot.
    pub fn len(&self) -> usize {
        self.pages.len()
    }

    /// Return `true` when this snapshot has no pages.
    pub fn is_empty(&self) -> bool {
        self.pages.is_empty()
    }

    /// Return the text for `page`, if the snapshot contains it.
    pub fn page_text(&self, page: usize) -> Option<&str> {
        self.pages
            .iter()
            .find(|entry| entry.page() == page)
            .map(PageText::text)
    }
}

/// The kind of change recorded by a [`CompareHunk`].
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum HunkKind {
    /// The page exists only in the newer snapshot.
    Added,
    /// The page exists only in the older snapshot.
    Removed,
    /// The page exists in both snapshots but its text differs.
    Changed,
}

/// One deterministic change hunk, always tied to a page number.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CompareHunk {
    page: usize,
    kind: HunkKind,
    old_text: Option<String>,
    new_text: Option<String>,
}

impl CompareHunk {
    /// Create a hunk.
    pub fn new(
        page: usize,
        kind: HunkKind,
        old_text: Option<String>,
        new_text: Option<String>,
    ) -> Self {
        Self {
            page,
            kind,
            old_text,
            new_text,
        }
    }

    /// Return the zero-based page number this hunk refers to.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the change kind.
    pub fn kind(&self) -> HunkKind {
        self.kind
    }

    /// Return the older text, if the page existed in the older snapshot.
    pub fn old_text(&self) -> Option<&str> {
        self.old_text.as_deref()
    }

    /// Return the newer text, if the page existed in the newer snapshot.
    pub fn new_text(&self) -> Option<&str> {
        self.new_text.as_deref()
    }
}

/// Counts of each change category produced by a comparison.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct CompareSummary {
    /// Number of added pages.
    pub added: usize,
    /// Number of removed pages.
    pub removed: usize,
    /// Number of pages whose text changed.
    pub changed: usize,
    /// Number of pages whose text is identical in both snapshots.
    pub unchanged: usize,
}

impl CompareSummary {
    /// Return the total number of hunks (`added + removed + changed`).
    pub fn total_hunks(&self) -> usize {
        self.added + self.removed + self.changed
    }

    /// Return the total number of pages examined.
    pub fn total_pages(&self) -> usize {
        self.total_hunks() + self.unchanged
    }
}

/// The result of comparing two snapshots.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct CompareReport {
    hunks: Vec<CompareHunk>,
    summary: CompareSummary,
}

impl CompareReport {
    /// Create a report from hunks and a summary.
    pub fn new(hunks: Vec<CompareHunk>, summary: CompareSummary) -> Self {
        Self { hunks, summary }
    }

    /// Return the hunks, ordered by ascending page number.
    pub fn hunks(&self) -> &[CompareHunk] {
        &self.hunks
    }

    /// Return the summary counts.
    pub fn summary(&self) -> CompareSummary {
        self.summary
    }

    /// Return `true` when the snapshots had identical page text.
    pub fn is_equal(&self) -> bool {
        self.summary.total_hunks() == 0
    }

    /// Return a deterministic, human-readable summary line.
    ///
    /// The layout is stable so it can be shown in a status bar or used in a
    /// golden test: `added=A removed=R changed=C unchanged=U`.
    pub fn render_summary(&self) -> String {
        format!(
            "added={} removed={} changed={} unchanged={}",
            self.summary.added, self.summary.removed, self.summary.changed, self.summary.unchanged
        )
    }
}

/// Compare `older` against `newer`, producing deterministic hunks and a summary.
///
/// For every page number in the union of both snapshots:
///
/// - present only in `newer` → an [`HunkKind::Added`] hunk;
/// - present only in `older` → a [`HunkKind::Removed`] hunk;
/// - present in both with differing text → a [`HunkKind::Changed`] hunk carrying
///   both texts;
/// - present in both with identical text → counted as unchanged and omitted
///   from the hunk list.
///
/// Comparison is exact: whitespace and case differences count as changes.
/// Hunks are ordered by ascending page number regardless of input order.
pub fn compare_snapshots(older: &TextSnapshot, newer: &TextSnapshot) -> CompareReport {
    let old_pages: BTreeMap<usize, &str> = older
        .pages()
        .iter()
        .map(|page| (page.page(), page.text()))
        .collect();
    let new_pages: BTreeMap<usize, &str> = newer
        .pages()
        .iter()
        .map(|page| (page.page(), page.text()))
        .collect();

    let mut hunks = Vec::new();
    let mut summary = CompareSummary::default();

    for (page, old_text) in &old_pages {
        match new_pages.get(page) {
            None => {
                summary.removed += 1;
                hunks.push(CompareHunk::new(
                    *page,
                    HunkKind::Removed,
                    Some((*old_text).to_string()),
                    None,
                ));
            }
            Some(new_text) if old_text != new_text => {
                summary.changed += 1;
                hunks.push(CompareHunk::new(
                    *page,
                    HunkKind::Changed,
                    Some((*old_text).to_string()),
                    Some((*new_text).to_string()),
                ));
            }
            Some(_) => summary.unchanged += 1,
        }
    }

    for (page, new_text) in &new_pages {
        if !old_pages.contains_key(page) {
            summary.added += 1;
            hunks.push(CompareHunk::new(
                *page,
                HunkKind::Added,
                None,
                Some((*new_text).to_string()),
            ));
        }
    }

    hunks.sort_by_key(CompareHunk::page);
    CompareReport::new(hunks, summary)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn snapshot(pages: Vec<(usize, &str)>) -> TextSnapshot {
        TextSnapshot::from_plain(
            pages
                .into_iter()
                .map(|(page, text)| (page, text.to_string())),
        )
        .expect("valid test snapshot")
    }

    #[test]
    fn compare_detects_all_change_kinds_with_page_references() {
        let older = snapshot(vec![(0, "same"), (1, "old"), (2, "gone")]);
        let newer = snapshot(vec![(0, "same"), (1, "new"), (3, "fresh")]);
        let report = compare_snapshots(&older, &newer);

        assert!(!report.is_equal());
        assert_eq!(
            report.summary(),
            CompareSummary {
                added: 1,
                removed: 1,
                changed: 1,
                unchanged: 1,
            }
        );
        assert_eq!(
            report
                .hunks()
                .iter()
                .map(|h| (h.page(), h.kind()))
                .collect::<Vec<_>>(),
            vec![
                (1, HunkKind::Changed),
                (2, HunkKind::Removed),
                (3, HunkKind::Added)
            ]
        );
        let changed = &report.hunks()[0];
        assert_eq!(changed.old_text(), Some("old"));
        assert_eq!(changed.new_text(), Some("new"));
        assert_eq!(report.hunks()[1].new_text(), None);
        assert_eq!(report.hunks()[2].old_text(), None);
    }

    #[test]
    fn compare_ignores_input_page_order() {
        let older_a = snapshot(vec![(2, "c"), (0, "a"), (1, "b")]);
        let older_b = snapshot(vec![(1, "b"), (2, "c"), (0, "a")]);
        let newer_a = snapshot(vec![(3, "d"), (1, "B"), (0, "a")]);
        let newer_b = snapshot(vec![(0, "a"), (3, "d"), (1, "B")]);
        assert_eq!(
            compare_snapshots(&older_a, &newer_a),
            compare_snapshots(&older_b, &newer_b)
        );
    }

    #[test]
    fn equal_snapshots_produce_empty_report() {
        let a = snapshot(vec![(0, "x"), (1, "y")]);
        let b = snapshot(vec![(1, "y"), (0, "x")]);
        let report = compare_snapshots(&a, &b);
        assert!(report.is_equal());
        assert!(report.hunks().is_empty());
        assert_eq!(report.summary().total_pages(), 2);
        assert_eq!(
            report.render_summary(),
            "added=0 removed=0 changed=0 unchanged=2"
        );
    }

    #[test]
    fn empty_snapshots_are_handled() {
        let empty = TextSnapshot::new(Vec::new()).expect("empty snapshot");
        let non_empty = snapshot(vec![(0, "x")]);

        assert!(compare_snapshots(&empty, &empty).is_equal());
        let added = compare_snapshots(&empty, &non_empty);
        assert_eq!(added.summary().added, 1);
        assert_eq!(added.hunks()[0].kind(), HunkKind::Added);
        let removed = compare_snapshots(&non_empty, &empty);
        assert_eq!(removed.summary().removed, 1);
        assert_eq!(removed.hunks()[0].kind(), HunkKind::Removed);
    }

    #[test]
    fn comparison_is_exact_about_case_and_whitespace() {
        let older = snapshot(vec![(0, "Hello world")]);
        let newer = snapshot(vec![(0, "hello  world")]);
        let report = compare_snapshots(&older, &newer);
        assert_eq!(report.summary().changed, 1);
        assert_eq!(
            report.hunks()[0].new_text(),
            Some("hello  world"),
            "even whitespace-only differences are hunks"
        );
    }

    #[test]
    fn snapshot_rejects_duplicate_pages_and_bounds() {
        assert!(matches!(
            TextSnapshot::new(vec![PageText::new(1, "a"), PageText::new(1, "b")]),
            Err(CompareError::DuplicatePage(1))
        ));
        let too_many: Vec<PageText> = (0..MAX_SNAPSHOT_PAGES + 1)
            .map(|i| PageText::new(i, "x"))
            .collect();
        assert!(matches!(
            TextSnapshot::new(too_many),
            Err(CompareError::TooManyPages { .. })
        ));
    }

    #[test]
    fn snapshot_is_sorted_and_supports_lookup() {
        let snap = snapshot(vec![(2, "c"), (0, "a"), (1, "b")]);
        assert_eq!(
            snap.pages().iter().map(PageText::page).collect::<Vec<_>>(),
            vec![0, 1, 2]
        );
        assert_eq!(snap.len(), 3);
        assert!(!snap.is_empty());
        assert_eq!(snap.page_text(1), Some("b"));
        assert_eq!(snap.page_text(9), None);
    }

    #[test]
    fn compare_is_repeatable_and_summary_counts_are_stable() {
        let older = snapshot(vec![(0, "a"), (1, "b"), (2, "c")]);
        let newer = snapshot(vec![(0, "a"), (1, "B"), (2, "c"), (4, "new")]);
        let first = compare_snapshots(&older, &newer);
        assert_eq!(first, compare_snapshots(&older, &newer));
        assert_eq!(first.summary().total_hunks(), first.hunks().len());
        assert_eq!(first.summary().total_pages(), 4);
        assert_eq!(
            first.render_summary(),
            "added=1 removed=0 changed=1 unchanged=2"
        );
    }
}
