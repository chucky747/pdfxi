//! Dependency-light, deterministic in-document find state.
//!
//! This module implements the *model* behind the viewer's find bar. It has no
//! `egui` dependency and no dependency on a PDF/text backend: the caller supplies
//! page text as `(page_index, String)` pairs via [`FindState::set_pages`], and all
//! matching happens over an owned snapshot. That keeps searching testable without
//! a window and keeps the default `pdfxi-app` build dependency-light.
//!
//! # Offsets and matching rules
//!
//! [`FindMatch`] offsets are indices into a page's text counted in **Unicode
//! scalar values** (characters), not bytes, so they stay valid for multibyte text
//! and line up with the character offsets used by [`crate::selection`]. Matching
//! mirrors the text lane's search contract: case-insensitive mode folds ASCII
//! letters only, whole-word mode requires non-word neighbours, and matches are
//! found leftmost-first and non-overlapping. Pages are sorted by index before
//! searching so results are deterministic regardless of supply order.
//!
//! # Bounded work
//!
//! Search stops after [`DEFAULT_MATCH_CAP`] matches (configurable with
//! [`FindState::with_cap`]). When more matches exist, [`FindState::is_truncated`]
//! becomes `true` and the summary gains a `+` suffix. The cap bounds both memory
//! and CPU for hostile or repetitive documents.

/// Default maximum number of matches retained by [`FindState`].
///
/// Ten thousand highlights are far more than a user can inspect and keep the
/// find model's memory bounded for adversarial documents.
pub const DEFAULT_MATCH_CAP: usize = 10_000;

/// Options controlling how [`FindState`] matches text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FindOptions {
    /// When `true`, `A` and `a` are distinct; when `false` (the default), ASCII
    /// letters are compared case-insensitively.
    pub case_sensitive: bool,
    /// When `true`, a match is accepted only when the characters immediately
    /// before and after it are non-word characters (or page boundaries).
    pub whole_word: bool,
}

impl FindOptions {
    /// Create options with `case_sensitive` set and whole-word matching off.
    pub const fn new(case_sensitive: bool) -> Self {
        Self {
            case_sensitive,
            whole_word: false,
        }
    }

    /// Return a copy with `whole_word` set to `value`.
    pub const fn with_whole_word(self, value: bool) -> Self {
        Self {
            case_sensitive: self.case_sensitive,
            whole_word: value,
        }
    }
}

/// One non-overlapping match found by [`FindState`].
///
/// `start` and `end` are half-open character offsets into the matched page's
/// text; `text` preserves the original casing from the page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FindMatch {
    page: usize,
    start: usize,
    end: usize,
    text: String,
}

impl FindMatch {
    /// Create a match spanning `start..end` (character offsets) on `page`.
    pub fn new(page: usize, start: usize, end: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            start,
            end,
            text: text.into(),
        }
    }

    /// The zero-based page number the match was found on.
    pub fn page(&self) -> usize {
        self.page
    }

    /// The inclusive start character offset.
    pub fn start(&self) -> usize {
        self.start
    }

    /// The exclusive end character offset.
    pub fn end(&self) -> usize {
        self.end
    }

    /// The length of the match in characters.
    pub fn len(&self) -> usize {
        self.end.saturating_sub(self.start)
    }

    /// Return `true` when the match spans no characters.
    pub fn is_empty(&self) -> bool {
        self.start >= self.end
    }

    /// The matched text, preserving the page's original casing.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// The complete, UI-independent state of an in-document find operation.
///
/// [`Self::set_pages`] installs a snapshot of the document's page text;
/// [`Self::set_query`] and [`Self::set_options`] re-run the search over that
/// snapshot. [`Self::next_match`] / [`Self::previous_match`] move the current
/// match with wrap-around, and [`Self::current`] / [`Self::match_summary`]
/// describe where the search is.
#[derive(Clone, Debug)]
pub struct FindState {
    query: String,
    options: FindOptions,
    pages: Vec<(usize, String)>,
    matches: Vec<FindMatch>,
    current: usize,
    truncated: bool,
    cap: usize,
}

impl Default for FindState {
    fn default() -> Self {
        Self::new()
    }
}

impl FindState {
    /// Create an empty finder with the default match cap.
    pub fn new() -> Self {
        Self::with_cap(DEFAULT_MATCH_CAP)
    }

    /// Create an empty finder that retains at most `cap` matches.
    ///
    /// A `cap` of zero still reports whether a query has any match but retains
    /// none, which is useful for cheap existence checks.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            query: String::new(),
            options: FindOptions::default(),
            pages: Vec::new(),
            matches: Vec::new(),
            current: 0,
            truncated: false,
            cap,
        }
    }

    /// Install a snapshot of page text as `(page_index, text)` pairs.
    ///
    /// Entries are sorted by page index so results are independent of the order
    /// in which pages were supplied. If a query is already set, results are
    /// recomputed immediately and the current match resets to the first one.
    pub fn set_pages(&mut self, pages: impl IntoIterator<Item = (usize, String)>) {
        let mut pages: Vec<(usize, String)> = pages.into_iter().collect();
        pages.sort_by_key(|(page, _)| *page);
        self.pages = pages;
        self.recompute();
    }

    /// Set the search query and recompute results from the installed pages.
    ///
    /// An empty query clears all results. A non-empty query resets the current
    /// match to the first result (if any).
    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = query.into();
        self.recompute();
    }

    /// Set the search options and recompute results from the installed pages.
    pub fn set_options(&mut self, options: FindOptions) {
        self.options = options;
        self.recompute();
    }

    /// Advance to the next match, wrapping from the last back to the first.
    ///
    /// Returns the new current match, or `None` when there are no results.
    pub fn next_match(&mut self) -> Option<&FindMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + 1) % self.matches.len();
        self.matches.get(self.current)
    }

    /// Move to the previous match, wrapping from the first to the last.
    ///
    /// Returns the new current match, or `None` when there are no results.
    pub fn previous_match(&mut self) -> Option<&FindMatch> {
        if self.matches.is_empty() {
            return None;
        }
        self.current = (self.current + self.matches.len() - 1) % self.matches.len();
        self.matches.get(self.current)
    }

    /// The current match, or `None` when there are no results.
    pub fn current(&self) -> Option<&FindMatch> {
        self.matches.get(self.current)
    }

    /// A compact one-line status describing the search position.
    ///
    /// - no query: `"No search"`
    /// - query with no results: `"No matches"`
    /// - results: `"{n} of {m}"`, with a trailing `+` on `m` when the cap
    ///   truncated the result set.
    pub fn match_summary(&self) -> String {
        if self.query.is_empty() {
            return "No search".to_string();
        }
        if self.matches.is_empty() {
            return "No matches".to_string();
        }
        let suffix = if self.truncated { "+" } else { "" };
        format!("{} of {}{}", self.current + 1, self.matches.len(), suffix)
    }

    /// The active query string.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The active search options.
    pub fn options(&self) -> FindOptions {
        self.options
    }

    /// All retained matches, in page-then-offset order.
    pub fn matches(&self) -> &[FindMatch] {
        &self.matches
    }

    /// The number of retained matches.
    pub fn match_count(&self) -> usize {
        self.matches.len()
    }

    /// The zero-based index of the current match, if any.
    pub fn current_index(&self) -> Option<usize> {
        if self.matches.is_empty() {
            None
        } else {
            Some(self.current)
        }
    }

    /// Whether the result set was cut short by the match cap.
    pub fn is_truncated(&self) -> bool {
        self.truncated
    }

    /// The maximum number of matches this finder retains.
    pub fn cap(&self) -> usize {
        self.cap
    }

    /// Recompute results from the current query, options, and page snapshot.
    fn recompute(&mut self) {
        self.current = 0;
        self.truncated = false;
        self.matches.clear();

        if self.query.is_empty() {
            return;
        }
        let needle: Vec<char> = self
            .query
            .chars()
            .map(|c| fold_char(c, self.options.case_sensitive))
            .collect();
        if needle.is_empty() {
            return;
        }

        'pages: for (page_number, text) in &self.pages {
            let chars: Vec<char> = text.chars().collect();
            let mut at = 0usize;
            while at.saturating_add(needle.len()) <= chars.len() {
                if let Some(end) = match_at(&chars, &needle, at, &self.options) {
                    if self.matches.len() >= self.cap {
                        self.truncated = true;
                        break 'pages;
                    }
                    let matched: String = chars
                        .get(at..end)
                        .map(|slice| slice.iter().collect())
                        .unwrap_or_default();
                    self.matches
                        .push(FindMatch::new(*page_number, at, end, matched));
                    at = end;
                } else {
                    at += 1;
                }
            }
        }
    }
}

/// Fold one character for comparison: ASCII letters fold, others pass through.
fn fold_char(c: char, case_sensitive: bool) -> char {
    if case_sensitive || !c.is_ascii() {
        c
    } else {
        c.to_ascii_lowercase()
    }
}

/// Whether `c` counts as a word character for whole-word matching.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Return the end offset of `needle` in `chars` starting at `at`, if matched.
fn match_at(chars: &[char], needle: &[char], at: usize, options: &FindOptions) -> Option<usize> {
    let end = at.checked_add(needle.len())?;
    if end > chars.len() {
        return None;
    }
    for (index, expected) in needle.iter().enumerate() {
        let actual = chars.get(at + index).copied()?;
        if fold_char(actual, options.case_sensitive) != *expected {
            return None;
        }
    }
    if options.whole_word {
        let before_ok = at == 0 || chars.get(at - 1).copied().is_none_or(|c| !is_word_char(c));
        let after_ok =
            end == chars.len() || chars.get(end).copied().is_none_or(|c| !is_word_char(c));
        if !(before_ok && after_ok) {
            return None;
        }
    }
    Some(end)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages() -> Vec<(usize, String)> {
        vec![
            (1, "Alpha alpha ALPHA".to_string()),
            (0, "the cat in the hat".to_string()),
            (2, "banana".to_string()),
        ]
    }

    fn finder() -> FindState {
        let mut state = FindState::new();
        state.set_pages(pages());
        state
    }

    #[test]
    fn query_and_option_transitions_recompute_results() {
        let mut state = finder();

        state.set_query("alpha");
        // Case-insensitive by default: three occurrences on page 1.
        assert_eq!(state.match_count(), 3);
        assert!(state.matches().iter().all(|m| m.page() == 1));
        assert_eq!(state.query(), "alpha");

        state.set_options(FindOptions::new(true));
        assert_eq!(state.match_count(), 1, "only the lowercase 'alpha' remains");
        assert_eq!(
            state.matches().first().map(FindMatch::start),
            Some(6),
            "current resets to the first result"
        );
        assert_eq!(state.current_index(), Some(0));
    }

    #[test]
    fn whole_word_option_filters_substrings() {
        let mut state = finder();
        state.set_query("cat");
        assert_eq!(state.match_count(), 1); // "cat" in page 0
        state.set_options(FindOptions::new(false).with_whole_word(true));
        assert_eq!(state.match_count(), 1, "'cat' is already a whole word here");

        // A dedicated page proves the substring case is actually rejected.
        let mut wordy = FindState::new();
        wordy.set_pages([(0, "cat category cat".to_string())]);
        wordy.set_query("cat");
        assert_eq!(wordy.match_count(), 3);
        wordy.set_options(FindOptions::new(false).with_whole_word(true));
        assert_eq!(wordy.match_count(), 2);
        assert_eq!(
            wordy
                .matches()
                .iter()
                .map(FindMatch::start)
                .collect::<Vec<_>>(),
            vec![0, 13]
        );
    }

    #[test]
    fn empty_query_clears_results_and_resets_summary() {
        let mut state = finder();
        state.set_query("alpha");
        assert_eq!(state.match_count(), 3);
        state.set_query("");
        assert_eq!(state.match_count(), 0);
        assert_eq!(state.current(), None);
        assert_eq!(state.match_summary(), "No search");
    }

    #[test]
    fn no_match_state_is_reported_cleanly() {
        let mut state = finder();
        state.set_query("zzzz");
        assert!(state.matches().is_empty());
        assert_eq!(state.current(), None);
        assert_eq!(state.next_match(), None);
        assert_eq!(state.previous_match(), None);
        assert_eq!(state.match_summary(), "No matches");
    }

    #[test]
    fn next_and_previous_wrap_around_deterministically() {
        let mut state = finder();
        state.set_query("alpha"); // three matches
        assert_eq!(state.current_index(), Some(0));
        assert_eq!(state.previous_match().map(FindMatch::start), Some(12));
        assert_eq!(state.next_match().map(FindMatch::start), Some(0));
        assert_eq!(state.next_match().map(FindMatch::start), Some(6));
        assert_eq!(state.next_match().map(FindMatch::start), Some(12));
        assert_eq!(state.next_match().map(FindMatch::start), Some(0), "wraps");
        assert_eq!(
            state.current(),
            state.matches().first(),
            "repeated navigation is deterministic"
        );
    }

    #[test]
    fn navigation_after_query_change_starts_at_the_first_match() {
        let mut state = finder();
        state.set_query("the");
        assert_eq!(state.match_count(), 2);
        let _ = state.next_match();
        assert_eq!(state.current_index(), Some(1));
        state.set_query("alpha");
        assert_eq!(state.current_index(), Some(0));
        assert_eq!(state.current().map(FindMatch::page), Some(1));
    }

    #[test]
    fn match_cap_bounds_results_and_marks_truncation() {
        let mut state = FindState::with_cap(2);
        state.set_pages([(0, "x x x x x".to_string())]);
        state.set_query("x");
        assert_eq!(state.match_count(), 2, "only the cap is retained");
        assert!(state.is_truncated());
        assert_eq!(state.match_summary(), "1 of 2+");

        let mut exact = FindState::with_cap(3);
        exact.set_pages([(0, "x x x".to_string())]);
        exact.set_query("x");
        assert_eq!(exact.match_count(), 3);
        assert!(!exact.is_truncated(), "an exact fit is not truncated");
        assert_eq!(exact.match_summary(), "1 of 3");
    }

    #[test]
    fn summary_formats_all_states() {
        let mut state = finder();
        assert_eq!(state.match_summary(), "No search");
        state.set_query("hat");
        assert_eq!(state.match_summary(), "1 of 1");
        let _ = state.next_match();
        assert_eq!(state.match_summary(), "1 of 1", "single result never moves");
        state.set_query("alpha");
        let _ = state.next_match();
        assert_eq!(state.match_summary(), "2 of 3");
    }

    #[test]
    fn character_offsets_are_stable_for_multibyte_text() {
        let mut state = FindState::new();
        state.set_pages([(0, "café — naïve".to_string())]);
        state.set_query("naïve");
        assert_eq!(state.match_count(), 1);
        let m = state.current().expect("one match");
        // Characters: c a f é(space)(em dash)(space) n a ï v e => index 7.
        assert_eq!((m.start(), m.end()), (7, 12));
        assert_eq!(m.text(), "naïve");
        assert_eq!(m.len(), 5);
    }

    #[test]
    fn pages_are_sorted_before_searching() {
        let mut ordered = finder();
        let mut shuffled = FindState::new();
        shuffled.set_pages(pages().into_iter().rev());
        ordered.set_query("the");
        shuffled.set_query("the");
        assert_eq!(ordered.matches(), shuffled.matches());
        assert_eq!(
            ordered
                .matches()
                .iter()
                .map(FindMatch::page)
                .collect::<Vec<_>>(),
            vec![0, 0]
        );
    }
}
