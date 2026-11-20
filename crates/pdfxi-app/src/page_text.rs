//! Caller-supplied page text and a shared deterministic matcher.
//!
//! The home/search/compare features are headless: the host extracts text from a
//! PDF and hands it here as [`PageText`] values. This module defines that small
//! value type plus a `pub(crate)` dependency-free matcher used by both
//! [`crate::cross_file`] and [`crate::compare`]. Offsets are character offsets
//! into [`PageText::text`], so multibyte text is handled correctly.

/// The extracted text of one numbered page.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PageText {
    page: usize,
    text: String,
}

impl PageText {
    /// Create a page-text value from a zero-based page number and its text.
    pub fn new(page: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            text: text.into(),
        }
    }

    /// Return the zero-based page number.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the page's plain text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Return `true` when `c` counts as a word character for whole-word matching.
pub(crate) fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Fold one character for case-insensitive comparison.
///
/// Only ASCII letters fold. Non-ASCII characters pass through unchanged so the
/// number of characters (and therefore offsets) is never altered.
fn fold(c: char, case_sensitive: bool) -> char {
    if case_sensitive || !c.is_ascii() {
        c
    } else {
        c.to_ascii_lowercase()
    }
}

/// Find every non-overlapping occurrence of `needle` in `text`.
///
/// Returned pairs are half-open *character* offsets into `text`, in ascending
/// order. An empty needle yields no matches. Case-insensitive mode folds ASCII
/// letters only. Whole-word mode accepts a match only when the characters
/// immediately before and after it are non-word characters (or text
/// boundaries). After accepting a match, scanning resumes at its end, so
/// overlapping candidates are discarded leftmost-first.
pub(crate) fn find_occurrences(
    text: &str,
    needle: &str,
    case_sensitive: bool,
    whole_word: bool,
) -> Vec<(usize, usize)> {
    let mut out = Vec::new();
    if needle.is_empty() {
        return out;
    }
    let haystack: Vec<char> = text.chars().collect();
    let pattern: Vec<char> = needle.chars().collect();
    if pattern.len() > haystack.len() {
        return out;
    }

    let mut at = 0usize;
    while at + pattern.len() <= haystack.len() {
        if matches_at(&haystack, &pattern, at, case_sensitive, whole_word) {
            let end = at + pattern.len();
            out.push((at, end));
            at = end;
        } else {
            at += 1;
        }
    }
    out
}

fn matches_at(
    haystack: &[char],
    pattern: &[char],
    at: usize,
    case_sensitive: bool,
    whole_word: bool,
) -> bool {
    let end = at + pattern.len();
    for (index, expected) in pattern.iter().enumerate() {
        if fold(haystack[at + index], case_sensitive) != fold(*expected, case_sensitive) {
            return false;
        }
    }
    if whole_word {
        let before_ok = at == 0 || !is_word_char(haystack[at - 1]);
        let after_ok = end == haystack.len() || !is_word_char(haystack[end]);
        if !(before_ok && after_ok) {
            return false;
        }
    }
    true
}

/// Return the substring of `text` spanned by a character-offset range.
///
/// Out-of-range or non-character-boundary ranges yield an empty string rather
/// than panicking, so callers can safely pass offsets from external data.
pub(crate) fn slice_chars(text: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    text.chars()
        .skip(start)
        .take(end.saturating_sub(start))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn occurrences_are_non_overlapping_and_use_char_offsets() {
        let found = find_occurrences("banana", "ana", false, false);
        assert_eq!(found, vec![(1, 4)], "overlapping candidate is discarded");
        let unicode = find_occurrences("café — naïve", "naïve", false, false);
        assert_eq!(unicode, vec![(7, 12)]);
    }

    #[test]
    fn occurrences_honor_case_and_whole_word() {
        assert_eq!(
            find_occurrences("Cat cat CAT", "cat", false, false).len(),
            3
        );
        assert_eq!(find_occurrences("Cat cat CAT", "cat", true, false).len(), 1);
        assert_eq!(
            find_occurrences("cat category cat", "cat", false, true),
            vec![(0, 3), (13, 16)]
        );
    }

    #[test]
    fn slicing_handles_multibyte_and_bad_ranges() {
        assert_eq!(slice_chars("café — naïve", 7, 12), "naïve");
        assert_eq!(slice_chars("abc", 2, 1), "");
        assert_eq!(slice_chars("abc", 2, 100), "c");
    }
}
