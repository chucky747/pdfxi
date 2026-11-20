//! Cross-file search over caller-supplied page text.
//!
//! [`CrossFileSearch`] indexes a set of files, each holding its own
//! [`PageText`] values, and runs one query across every file. It is entirely
//! headless: the caller supplies already-extracted text, so this module
//! performs no I/O and depends on no PDF or GUI backend. Results are
//! deterministic (files sorted by id, matches by page and character offset),
//! ranked by match count, grouped by file and page, and bounded by explicit
//! result caps.

use std::collections::BTreeMap;

use crate::page_text::{PageText, find_occurrences, slice_chars};

/// Default cap on the total number of matches returned by one query.
pub const DEFAULT_MAX_RESULTS: usize = 200;
/// Default cap on the number of matches returned from any single file.
pub const DEFAULT_MAX_PER_FILE: usize = 50;
/// Maximum number of files accepted by [`CrossFileSearch::new`].
pub const MAX_FILES: usize = 256;
/// Maximum number of pages accepted in a single [`SearchFile`].
pub const MAX_PAGES_PER_FILE: usize = 10_000;

/// A stable identifier for one file, supplied by the caller.
pub type FileId = String;

/// One caller-supplied file and its extracted page text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchFile {
    id: FileId,
    pages: Vec<PageText>,
}

impl SearchFile {
    /// Create a file from an id and its page text.
    pub fn new(id: impl Into<FileId>, pages: Vec<PageText>) -> Self {
        Self {
            id: id.into(),
            pages,
        }
    }

    /// Return this file's identifier.
    pub fn id(&self) -> &str {
        &self.id
    }

    /// Return this file's pages.
    pub fn pages(&self) -> &[PageText] {
        &self.pages
    }
}

/// Query options and result caps for [`CrossFileSearch::search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossFileQuery {
    /// The text to find; an empty or whitespace-only string is rejected.
    pub text: String,
    /// When `true`, ASCII letter case is significant.
    pub case_sensitive: bool,
    /// When `true`, matches must be bounded by non-word characters.
    pub whole_word: bool,
    /// Maximum total matches returned by the query. Must be non-zero.
    pub max_results: usize,
    /// Maximum matches returned from any one file. Must be non-zero.
    pub max_per_file: usize,
}

impl Default for CrossFileQuery {
    fn default() -> Self {
        Self {
            text: String::new(),
            case_sensitive: false,
            whole_word: false,
            max_results: DEFAULT_MAX_RESULTS,
            max_per_file: DEFAULT_MAX_PER_FILE,
        }
    }
}

impl CrossFileQuery {
    /// Build a query for `text` with default options and caps.
    pub fn new(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            ..Self::default()
        }
    }

    /// Return a copy with case sensitivity set to `value`.
    pub fn with_case_sensitive(mut self, value: bool) -> Self {
        self.case_sensitive = value;
        self
    }

    /// Return a copy with whole-word matching set to `value`.
    pub fn with_whole_word(mut self, value: bool) -> Self {
        self.whole_word = value;
        self
    }

    /// Return a copy with explicit result caps.
    pub fn with_caps(mut self, max_results: usize, max_per_file: usize) -> Self {
        self.max_results = max_results;
        self.max_per_file = max_per_file;
        self
    }
}

/// One match found in a specific file and page, with character offsets into
/// that page's [`PageText::text`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossMatch {
    page: usize,
    start: usize,
    end: usize,
    text: String,
}

impl CrossMatch {
    /// Create a match spanning `start..end` (character offsets) on `page`.
    pub fn new(page: usize, start: usize, end: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            start,
            end,
            text: text.into(),
        }
    }

    /// Return the zero-based page number of this match.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the inclusive start character offset.
    pub fn start(&self) -> usize {
        self.start
    }

    /// Return the exclusive end character offset.
    pub fn end(&self) -> usize {
        self.end
    }

    /// Return the matched text, preserving the source casing.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// All matches from one page of a file, ordered by ascending offset.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossPageGroup {
    page: usize,
    matches: Vec<CrossMatch>,
}

impl CrossPageGroup {
    /// Create a group for `page`.
    pub fn new(page: usize, matches: Vec<CrossMatch>) -> Self {
        Self { page, matches }
    }

    /// Return the zero-based page number.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the matches, ordered by ascending character offset.
    pub fn matches(&self) -> &[CrossMatch] {
        &self.matches
    }

    /// Return the number of matches on this page.
    pub fn len(&self) -> usize {
        self.matches.len()
    }

    /// Return `true` when this page has no matches.
    pub fn is_empty(&self) -> bool {
        self.matches.is_empty()
    }
}

/// Ranked, grouped results for one file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossFileHit {
    file: FileId,
    match_count: usize,
    pages: Vec<CrossPageGroup>,
}

impl CrossFileHit {
    /// Create a hit for `file` with its total match count and page groups.
    pub fn new(file: impl Into<FileId>, match_count: usize, pages: Vec<CrossPageGroup>) -> Self {
        Self {
            file: file.into(),
            match_count,
            pages,
        }
    }

    /// Return the file identifier.
    pub fn file(&self) -> &str {
        &self.file
    }

    /// Return the number of matches counted for this file (up to its cap).
    pub fn match_count(&self) -> usize {
        self.match_count
    }

    /// Return the page groups, ordered by ascending page number.
    pub fn pages(&self) -> &[CrossPageGroup] {
        &self.pages
    }
}

/// The complete result of one cross-file query.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CrossFileReport {
    query: String,
    total_matches: usize,
    files: Vec<CrossFileHit>,
    truncated: bool,
}

impl CrossFileReport {
    /// Create a report.
    pub fn new(
        query: impl Into<String>,
        total_matches: usize,
        files: Vec<CrossFileHit>,
        truncated: bool,
    ) -> Self {
        Self {
            query: query.into(),
            total_matches,
            files,
            truncated,
        }
    }

    /// Return the query text that produced this report.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Return the total number of matches included in the report.
    pub fn total_matches(&self) -> usize {
        self.total_matches
    }

    /// Return the ranked file hits.
    pub fn files(&self) -> &[CrossFileHit] {
        &self.files
    }

    /// Return `true` when a result cap discarded at least one match.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// Return `true` when no matches were found.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }
}

/// Typed errors produced by cross-file search setup and queries.
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum CrossFileError {
    /// The query text was empty or whitespace-only.
    #[error("cross-file query text may not be empty")]
    EmptyQuery,
    /// A result cap was zero.
    #[error("cross-file result caps must be non-zero")]
    ZeroCap,
    /// A file id was empty.
    #[error("cross-file ids may not be empty")]
    EmptyFileId,
    /// Two files shared an id.
    #[error("duplicate cross-file id: {0}")]
    DuplicateFile(FileId),
    /// Too many files were supplied.
    #[error("too many cross-files (limit {limit})")]
    TooManyFiles {
        /// Maximum number of files allowed.
        limit: usize,
    },
    /// A file supplied too many pages.
    #[error("too many pages in cross-file {file} (limit {limit})")]
    TooManyPages {
        /// The offending file id.
        file: FileId,
        /// Maximum number of pages allowed per file.
        limit: usize,
    },
}

/// An immutable index of caller-supplied files for cross-file search.
///
/// Files are sorted by id at construction, so query results do not depend on
/// the order in which files were supplied. Page text is snapshotted, so later
/// changes to the caller's data cannot affect this index.
#[derive(Clone, Debug)]
pub struct CrossFileSearch {
    files: Vec<(FileId, Vec<PageText>)>,
}

impl CrossFileSearch {
    /// Build an index from `files`, validating ids and bounds.
    pub fn new(files: Vec<SearchFile>) -> Result<Self, CrossFileError> {
        if files.len() > MAX_FILES {
            return Err(CrossFileError::TooManyFiles { limit: MAX_FILES });
        }
        let mut files = files;
        files.sort_by(|a, b| a.id.cmp(&b.id));
        let mut indexed: Vec<(FileId, Vec<PageText>)> = Vec::with_capacity(files.len());
        let mut previous: Option<&str> = None;
        for file in &files {
            if file.id.trim().is_empty() {
                return Err(CrossFileError::EmptyFileId);
            }
            if previous == Some(file.id.as_str()) {
                return Err(CrossFileError::DuplicateFile(file.id.clone()));
            }
            if file.pages.len() > MAX_PAGES_PER_FILE {
                return Err(CrossFileError::TooManyPages {
                    file: file.id.clone(),
                    limit: MAX_PAGES_PER_FILE,
                });
            }
            previous = Some(file.id.as_str());
            indexed.push((file.id.clone(), file.pages.clone()));
        }
        Ok(Self { files: indexed })
    }

    /// Return the number of indexed files.
    pub fn len(&self) -> usize {
        self.files.len()
    }

    /// Return `true` when no files are indexed.
    pub fn is_empty(&self) -> bool {
        self.files.is_empty()
    }

    /// Return the indexed file ids in ascending order.
    pub fn file_ids(&self) -> impl Iterator<Item = &str> {
        self.files.iter().map(|(id, _)| id.as_str())
    }

    /// Run `query` across every indexed file.
    ///
    /// Within a file, pages are visited in ascending page number and matches in
    /// ascending character offset; matches are truncated to `max_per_file`.
    /// Files are ranked by descending match count with ascending file id as a
    /// tie-break, then matches are added in ranked order until `max_results` is
    /// reached. [`CrossFileReport::is_truncated`] reports whether either cap
    /// discarded a match.
    pub fn search(&self, query: &CrossFileQuery) -> Result<CrossFileReport, CrossFileError> {
        if query.text.trim().is_empty() {
            return Err(CrossFileError::EmptyQuery);
        }
        if query.max_results == 0 || query.max_per_file == 0 {
            return Err(CrossFileError::ZeroCap);
        }

        let mut truncated = false;
        let mut per_file: Vec<(FileId, usize, Vec<CrossPageGroup>)> = Vec::new();
        for (id, pages) in &self.files {
            let mut by_page: BTreeMap<usize, Vec<CrossMatch>> = BTreeMap::new();
            for page in pages {
                for (start, end) in find_occurrences(
                    page.text(),
                    &query.text,
                    query.case_sensitive,
                    query.whole_word,
                ) {
                    let text = slice_chars(page.text(), start, end);
                    by_page
                        .entry(page.page())
                        .or_default()
                        .push(CrossMatch::new(page.page(), start, end, text));
                }
            }

            let raw_count: usize = by_page.values().map(Vec::len).sum();
            if raw_count > query.max_per_file {
                truncated = true;
            }
            let mut remaining = query.max_per_file;
            let mut groups: Vec<CrossPageGroup> = Vec::new();
            let mut counted = 0usize;
            for (page, matches) in by_page {
                let mut kept = Vec::new();
                for candidate in matches {
                    if remaining == 0 {
                        break;
                    }
                    kept.push(candidate);
                    remaining -= 1;
                    counted += 1;
                }
                if !kept.is_empty() {
                    groups.push(CrossPageGroup::new(page, kept));
                }
                if remaining == 0 {
                    break;
                }
            }
            if counted > 0 {
                per_file.push((id.clone(), counted, groups));
            }
        }

        // Stable descending-count sort keeps ascending file id as the tie-break.
        per_file.sort_by_key(|entry| std::cmp::Reverse(entry.1));

        let mut budget = query.max_results;
        let mut hits = Vec::new();
        for (id, counted, groups) in per_file {
            if budget == 0 {
                truncated = true;
                continue;
            }
            if counted > budget {
                truncated = true;
            }
            let mut kept_groups = Vec::new();
            let mut taken = 0usize;
            for group in groups {
                if taken >= budget {
                    truncated = true;
                    break;
                }
                let room = budget - taken;
                if group.len() > room {
                    truncated = true;
                }
                let kept: Vec<CrossMatch> = group.matches().iter().take(room).cloned().collect();
                taken += kept.len();
                kept_groups.push(CrossPageGroup::new(group.page(), kept));
            }
            budget = budget.saturating_sub(taken);
            hits.push(CrossFileHit::new(id, taken, kept_groups));
        }

        let total = hits.iter().map(CrossFileHit::match_count).sum();
        Ok(CrossFileReport::new(
            query.text.clone(),
            total,
            hits,
            truncated,
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page(number: usize, text: &str) -> PageText {
        PageText::new(number, text)
    }

    fn index(files: Vec<(&str, Vec<PageText>)>) -> CrossFileSearch {
        let files = files
            .into_iter()
            .map(|(id, pages)| SearchFile::new(id, pages))
            .collect();
        CrossFileSearch::new(files).expect("valid test index")
    }

    #[test]
    fn ranks_files_by_match_count_and_groups_by_page() {
        let search = index(vec![
            ("alpha", vec![page(0, "hit hit"), page(1, "nothing")]),
            ("beta", vec![page(0, "hit"), page(2, "hit hit hit")]),
            ("gamma", vec![page(0, "none")]),
        ]);
        assert_eq!(
            search.file_ids().collect::<Vec<_>>(),
            vec!["alpha", "beta", "gamma"]
        );

        let report = search.search(&CrossFileQuery::new("hit")).expect("query");
        assert_eq!(report.query(), "hit");
        assert_eq!(report.total_matches(), 6);
        assert!(!report.is_truncated());
        assert_eq!(
            report
                .files()
                .iter()
                .map(CrossFileHit::file)
                .collect::<Vec<_>>(),
            vec!["beta", "alpha"],
            "files with more matches rank first"
        );
        let beta = &report.files()[0];
        assert_eq!(beta.match_count(), 4);
        assert_eq!(
            beta.pages()
                .iter()
                .map(CrossPageGroup::page)
                .collect::<Vec<_>>(),
            vec![0, 2],
            "only pages with matches are grouped"
        );
        assert_eq!(beta.pages()[1].len(), 3);
    }

    #[test]
    fn equal_counts_break_ties_by_file_id() {
        let search = index(vec![
            ("zeta", vec![page(0, "x")]),
            ("alpha", vec![page(0, "x")]),
            ("mid", vec![page(0, "x")]),
        ]);
        let report = search.search(&CrossFileQuery::new("x")).expect("query");
        assert_eq!(
            report
                .files()
                .iter()
                .map(CrossFileHit::file)
                .collect::<Vec<_>>(),
            vec!["alpha", "mid", "zeta"]
        );
    }

    #[test]
    fn case_and_whole_word_options_are_honored() {
        let search = index(vec![("f", vec![page(0, "cat category CAT")])]);
        let folded = search.search(&CrossFileQuery::new("cat")).expect("query");
        assert_eq!(folded.total_matches(), 3);

        let exact = search
            .search(&CrossFileQuery::new("cat").with_case_sensitive(true))
            .expect("query");
        assert_eq!(
            exact.total_matches(),
            2,
            "lowercase 'cat' and 'cat' in 'category' match"
        );

        let words = search
            .search(&CrossFileQuery::new("cat").with_whole_word(true))
            .expect("query");
        assert_eq!(
            words.total_matches(),
            2,
            "'cat' inside 'category' is excluded"
        );
    }

    #[test]
    fn global_cap_truncates_in_ranked_order() {
        let search = index(vec![
            ("a", vec![page(0, "hit")]),
            ("b", vec![page(0, "hit hit hit")]),
        ]);
        let report = search
            .search(&CrossFileQuery::new("hit").with_caps(2, 50))
            .expect("query");
        assert!(report.is_truncated());
        assert_eq!(report.total_matches(), 2);
        assert_eq!(
            report
                .files()
                .iter()
                .map(CrossFileHit::file)
                .collect::<Vec<_>>(),
            vec!["b"],
            "files with no remaining budget are dropped"
        );
        assert_eq!(report.files()[0].match_count(), 2);
    }

    #[test]
    fn per_file_cap_truncates_within_a_file() {
        let search = index(vec![("a", vec![page(0, "hit hit hit hit")])]);
        let report = search
            .search(&CrossFileQuery::new("hit").with_caps(100, 2))
            .expect("query");
        assert!(report.is_truncated());
        assert_eq!(report.total_matches(), 2);
        assert_eq!(report.files()[0].match_count(), 2);
        assert_eq!(report.files()[0].pages()[0].len(), 2);
    }

    #[test]
    fn results_are_deterministic_and_input_order_independent() {
        let a = index(vec![
            ("b", vec![page(0, "needle here")]),
            ("a", vec![page(1, "needle")]),
        ]);
        let b = index(vec![
            ("a", vec![page(1, "needle")]),
            ("b", vec![page(0, "needle here")]),
        ]);
        let query = CrossFileQuery::new("needle");
        let first = a.search(&query).expect("query");
        assert_eq!(first, b.search(&query).expect("query"));
        assert_eq!(first, a.search(&query).expect("repeat"));
    }

    #[test]
    fn no_matches_and_empty_index_produce_empty_reports() {
        let empty = CrossFileSearch::new(Vec::new()).expect("empty index");
        assert!(empty.is_empty());
        assert_eq!(empty.len(), 0);
        let report = empty.search(&CrossFileQuery::new("x")).expect("query");
        assert!(report.is_empty());
        assert_eq!(report.total_matches(), 0);
        assert!(!report.is_truncated());

        let search = index(vec![("a", vec![page(0, "abcdef")])]);
        assert!(
            search
                .search(&CrossFileQuery::new("zzz"))
                .expect("query")
                .is_empty()
        );
    }

    #[test]
    fn matches_use_character_offsets_for_multibyte_text() {
        let search = index(vec![("u", vec![page(0, "café — naïve")])]);
        let report = search.search(&CrossFileQuery::new("naïve")).expect("query");
        let m = &report.files()[0].pages()[0].matches()[0];
        assert_eq!((m.start(), m.end()), (7, 12));
        assert_eq!(m.text(), "naïve");
    }

    #[test]
    fn invalid_input_returns_typed_errors() {
        let search = index(vec![("a", vec![page(0, "x")])]);
        assert!(matches!(
            search.search(&CrossFileQuery::new("   ")),
            Err(CrossFileError::EmptyQuery)
        ));
        assert!(matches!(
            search.search(&CrossFileQuery::new("x").with_caps(0, 5)),
            Err(CrossFileError::ZeroCap)
        ));
        assert!(matches!(
            search.search(&CrossFileQuery::new("x").with_caps(5, 0)),
            Err(CrossFileError::ZeroCap)
        ));
        assert!(matches!(
            CrossFileSearch::new(vec![SearchFile::new("", vec![page(0, "x")])]),
            Err(CrossFileError::EmptyFileId)
        ));
        assert!(matches!(
            CrossFileSearch::new(vec![
                SearchFile::new("dup", vec![page(0, "x")]),
                SearchFile::new("dup", vec![page(0, "y")]),
            ]),
            Err(CrossFileError::DuplicateFile(_))
        ));
    }

    #[test]
    fn file_and_page_bounds_are_enforced() {
        let too_many_files: Vec<SearchFile> = (0..MAX_FILES + 1)
            .map(|i| SearchFile::new(format!("f-{i}"), Vec::new()))
            .collect();
        assert!(matches!(
            CrossFileSearch::new(too_many_files),
            Err(CrossFileError::TooManyFiles { .. })
        ));

        let pages = (0..MAX_PAGES_PER_FILE + 1).map(|i| page(i, "x")).collect();
        assert!(matches!(
            CrossFileSearch::new(vec![SearchFile::new("big", pages)]),
            Err(CrossFileError::TooManyPages { .. })
        ));
    }

    #[test]
    fn input_pages_are_snapshotted() {
        let pages = vec![page(0, "original")];
        let search =
            CrossFileSearch::new(vec![SearchFile::new("f", pages.clone())]).expect("index");
        let report = search
            .search(&CrossFileQuery::new("original"))
            .expect("query");
        assert_eq!(report.total_matches(), 1);
        // The index cloned the pages, so reusing the caller's vector is safe.
        assert_eq!(pages[0].text(), "original");
    }
}
