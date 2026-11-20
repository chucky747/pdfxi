//! Document-wide literal find and replace model (EDIT-003).
//!
//! This module is a **model only**. It searches text that a caller has already
//! extracted page-by-page and produces an ordered list of [`SearchHit`]s or a
//! deterministic [`ReplaceAllPlan`]. It performs **no PDF parsing** and
//! **never reads or rewrites content streams**: a host writer serializes the
//! plan's [`ReplaceStep`]s into whichever representation it owns. No source
//! bytes are mutated here.
//!
//! Matching is **literal only**. There is no regex engine and no pattern
//! syntax; a query is matched by Unicode scalar equality after an optional,
//! deterministic fold (ASCII/uniform case folding and a bounded diacritic
//! fold). This keeps worst-case work bounded by `query_len x text_len` per
//! line and makes hostile input safe. All output is deterministic: hits and
//! steps are sorted by `(page, line, scalar_offset)`, and equal inputs always
//! produce equal results. Oversized or degenerate inputs are rejected with
//! typed [`FindReplaceError`] variants rather than panicking.

use core::fmt;
use std::collections::BTreeSet;

/// One page of text supplied by the caller.
///
/// `page` is a zero-based page index; `lines` are the page's extracted lines in
/// reading order, without trailing newline characters.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchPage {
    /// Zero-based page index this text was extracted from.
    pub page: usize,
    /// Lines of text in reading order.
    pub lines: Vec<String>,
}

/// A document of already-extracted text, keyed by page.
///
/// Page indices must be unique; [`SearchDocument::new`] enforces that.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct SearchDocument {
    pages: Vec<SearchPage>,
}

impl SearchDocument {
    /// Builds a document from explicit pages, rejecting duplicate page indices.
    pub fn new(pages: Vec<SearchPage>) -> Result<Self, FindReplaceError> {
        let mut seen = BTreeSet::new();
        for page in &pages {
            if !seen.insert(page.page) {
                return Err(FindReplaceError::DuplicatePage(page.page));
            }
        }
        Ok(Self { pages })
    }

    /// Returns the document's pages in the order supplied to [`Self::new`].
    pub fn pages(&self) -> &[SearchPage] {
        &self.pages
    }
}

/// Matching behaviour for a search.
///
/// All flags default to `false`: case-insensitive, substring matching with no
/// diacritic folding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// When `true`, matching distinguishes upper and lower case.
    pub case_sensitive: bool,
    /// When `true`, a match is accepted only when the scalars immediately
    /// before and after it are not word characters (alphanumeric or `_`).
    pub whole_word: bool,
    /// When `true`, diacritics are folded before comparison (e.g. `cafe`
    /// matches `café`, and precomposed and combining accents both fold away).
    pub diacritic_insensitive: bool,
}

/// Size and count bounds applied to a [`find_all`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SearchLimits {
    /// Maximum Unicode scalars in the query.
    pub max_query_chars: usize,
    /// Maximum Unicode scalars in any single extracted line.
    pub max_line_chars: usize,
    /// Maximum number of hits before [`FindReplaceError::TooManyMatches`].
    pub max_hits: usize,
}

impl Default for SearchLimits {
    fn default() -> Self {
        Self {
            max_query_chars: 256,
            max_line_chars: 1_000_000,
            max_hits: 100_000,
        }
    }
}

/// Size and count bounds applied to a [`build_replace_all_plan`] call.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ReplaceLimits {
    /// Maximum number of replacements in a plan.
    pub max_replacements: usize,
    /// Maximum Unicode scalars in the replacement text.
    pub max_replacement_chars: usize,
}

impl Default for ReplaceLimits {
    fn default() -> Self {
        Self {
            max_replacements: 10_000,
            max_replacement_chars: 4096,
        }
    }
}

/// One literal match.
///
/// `start` and `end` are half-open Unicode **scalar** indices into the matched
/// line (`start` inclusive, `end` exclusive), so they remain stable for UTF-8
/// text.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct SearchHit {
    /// Zero-based page index of the match.
    pub page: usize,
    /// Zero-based line index within the page.
    pub line: usize,
    /// Inclusive start scalar offset within the line.
    pub start: usize,
    /// Exclusive end scalar offset within the line.
    pub end: usize,
}

/// One step in a replace-all plan.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReplaceStep {
    /// Zero-based page index of the match to replace.
    pub page: usize,
    /// Zero-based line index within the page.
    pub line: usize,
    /// Inclusive start scalar offset within the line.
    pub start: usize,
    /// Exclusive end scalar offset within the line.
    pub end: usize,
    /// Replacement text to insert in place of `[start, end)`.
    pub replacement: String,
}

/// A deterministic, non-overlapping replace-all plan for a host writer.
#[derive(Clone, Debug, PartialEq, Eq, Default)]
pub struct ReplaceAllPlan {
    /// Replacement steps sorted by `(page, line, start)`.
    pub steps: Vec<ReplaceStep>,
}

impl ReplaceAllPlan {
    /// Returns the number of replacements in this plan.
    pub fn replacement_count(&self) -> usize {
        self.steps.len()
    }
}

/// Typed errors produced while searching or building a replace plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FindReplaceError {
    /// The query contained no scalars, or folded to none.
    EmptyQuery,
    /// The query exceeded [`SearchLimits::max_query_chars`].
    QueryTooLarge {
        /// Query length in scalars.
        chars: usize,
        /// Permitted maximum.
        max: usize,
    },
    /// A line exceeded [`SearchLimits::max_line_chars`].
    LineTooLarge {
        /// Page index of the offending line.
        page: usize,
        /// Line index of the offending line.
        line: usize,
        /// Line length in scalars.
        chars: usize,
        /// Permitted maximum.
        max: usize,
    },
    /// Two pages shared the same page index.
    DuplicatePage(usize),
    /// A combination of options or limits was invalid.
    InvalidOptions(&'static str),
    /// More matches were found than the configured cap.
    TooManyMatches {
        /// The configured cap.
        cap: usize,
    },
    /// Replacement text exceeded [`ReplaceLimits::max_replacement_chars`].
    ReplacementTooLarge {
        /// Replacement length in scalars.
        chars: usize,
        /// Permitted maximum.
        max: usize,
    },
}

impl fmt::Display for FindReplaceError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::EmptyQuery => write!(f, "search query must not be empty"),
            Self::QueryTooLarge { chars, max } => {
                write!(f, "query has {chars} scalars, exceeding the limit of {max}")
            }
            Self::LineTooLarge {
                page,
                line,
                chars,
                max,
            } => write!(
                f,
                "line {line} on page {page} has {chars} scalars, exceeding the limit of {max}"
            ),
            Self::DuplicatePage(page) => write!(f, "duplicate page index {page}"),
            Self::InvalidOptions(reason) => write!(f, "invalid search options: {reason}"),
            Self::TooManyMatches { cap } => {
                write!(f, "match count exceeds the configured cap of {cap}")
            }
            Self::ReplacementTooLarge { chars, max } => {
                write!(
                    f,
                    "replacement has {chars} scalars, exceeding the limit of {max}"
                )
            }
        }
    }
}

impl std::error::Error for FindReplaceError {}

/// Returns `true` when `c` is a word character used for whole-word boundaries.
fn is_word_char(c: char) -> bool {
    c.is_alphanumeric() || c == '_'
}

/// Folds one scalar for comparison.
///
/// Case folding takes the first scalar of `to_lowercase` so the operation is
/// always one-to-one with the input scalar; the diacritic fold is a bounded,
/// documented subset (combining marks plus common precomposed Latin letters)
/// rather than full Unicode normalization.
fn fold_char(c: char, options: &SearchOptions) -> Option<char> {
    let lowered = if options.case_sensitive {
        c
    } else {
        c.to_lowercase().next().unwrap_or(c)
    };
    if options.diacritic_insensitive {
        strip_diacritic(lowered)
    } else {
        Some(lowered)
    }
}

/// Returns the base letter for common Latin diacritics, or `None` for a
/// combining mark that should be ignored entirely.
fn strip_diacritic(c: char) -> Option<char> {
    match c {
        '\u{0300}'..='\u{036F}'
        | '\u{1AB0}'..='\u{1AFF}'
        | '\u{1DC0}'..='\u{1DFF}'
        | '\u{20D0}'..='\u{20FF}'
        | '\u{FE20}'..='\u{FE2F}' => None,
        'À'..='Å' => Some('A'),
        'à'..='å' => Some('a'),
        'Æ' => Some('A'),
        'æ' => Some('a'),
        'Ç' => Some('C'),
        'ç' => Some('c'),
        'È'..='Ë' => Some('E'),
        'è'..='ë' => Some('e'),
        'Ì'..='Ï' => Some('I'),
        'ì'..='ï' => Some('i'),
        'Ñ' => Some('N'),
        'ñ' => Some('n'),
        'Ò'..='Ö' | 'Ø' => Some('O'),
        'ò'..='ö' | 'ø' => Some('o'),
        'Ù'..='Ü' => Some('U'),
        'ù'..='ü' => Some('u'),
        'Ý' | 'Ÿ' => Some('Y'),
        'ý' | 'ÿ' => Some('y'),
        _ => Some(c),
    }
}

/// Produces folded scalars paired with the original scalar index each came
/// from, so offsets stay valid even when ignorable marks are dropped.
fn fold_with_indices(text: &str, options: &SearchOptions) -> (Vec<char>, Vec<usize>) {
    let mut folded = Vec::new();
    let mut indices = Vec::new();
    for (index, c) in text.chars().enumerate() {
        if let Some(value) = fold_char(c, options) {
            folded.push(value);
            indices.push(index);
        }
    }
    (folded, indices)
}

/// Validates the query and folds it into comparison scalars.
fn prepared_query(
    query: &str,
    options: &SearchOptions,
    limits: &SearchLimits,
) -> Result<Vec<char>, FindReplaceError> {
    let chars = query.chars().count();
    if chars == 0 {
        return Err(FindReplaceError::EmptyQuery);
    }
    if chars > limits.max_query_chars {
        return Err(FindReplaceError::QueryTooLarge {
            chars,
            max: limits.max_query_chars,
        });
    }
    let (folded, _) = fold_with_indices(query, options);
    if folded.is_empty() {
        return Err(FindReplaceError::EmptyQuery);
    }
    if options.whole_word {
        let first = query.chars().next().unwrap_or(' ');
        let last = query.chars().last().unwrap_or(' ');
        if !is_word_char(first) || !is_word_char(last) {
            return Err(FindReplaceError::InvalidOptions(
                "whole_word requires a query starting and ending with a word character",
            ));
        }
    }
    Ok(folded)
}

/// Computes every match of `folded_query` in one line, mapping results back to
/// original scalar offsets and applying the whole-word boundary rule.
fn hits_in_line(
    page: usize,
    line_index: usize,
    line: &str,
    folded_query: &[char],
    options: &SearchOptions,
    limits: &SearchLimits,
) -> Result<Vec<SearchHit>, FindReplaceError> {
    let chars: Vec<char> = line.chars().collect();
    if chars.len() > limits.max_line_chars {
        return Err(FindReplaceError::LineTooLarge {
            page,
            line: line_index,
            chars: chars.len(),
            max: limits.max_line_chars,
        });
    }
    let (folded_text, indices) = fold_with_indices(line, options);
    let query_len = folded_query.len();
    let mut hits = Vec::new();
    if query_len > folded_text.len() {
        return Ok(hits);
    }
    for start in 0..=(folded_text.len() - query_len) {
        if folded_query != &folded_text[start..start + query_len] {
            continue;
        }
        let original_start = indices[start];
        let original_end = indices[start + query_len - 1] + 1;
        if options.whole_word {
            let before = original_start
                .checked_sub(1)
                .and_then(|index| chars.get(index))
                .copied();
            let after = chars.get(original_end).copied();
            if before.is_some_and(is_word_char) || after.is_some_and(is_word_char) {
                continue;
            }
        }
        hits.push(SearchHit {
            page,
            line: line_index,
            start: original_start,
            end: original_end,
        });
    }
    Ok(hits)
}

/// Finds every literal match of `query` in `document`.
///
/// Hits are sorted by `(page, line, start)` and include overlapping matches;
/// the count is bounded by [`SearchLimits::max_hits`].
pub fn find_all(
    document: &SearchDocument,
    query: &str,
    options: &SearchOptions,
    limits: &SearchLimits,
) -> Result<Vec<SearchHit>, FindReplaceError> {
    let folded_query = prepared_query(query, options, limits)?;
    let mut hits = Vec::new();
    for page in document.pages() {
        for (line_index, line) in page.lines.iter().enumerate() {
            let found = hits_in_line(page.page, line_index, line, &folded_query, options, limits)?;
            hits.extend(found);
            if hits.len() > limits.max_hits {
                return Err(FindReplaceError::TooManyMatches {
                    cap: limits.max_hits,
                });
            }
        }
    }
    hits.sort_unstable();
    Ok(hits)
}

/// Builds a deterministic, non-overlapping replace-all plan.
///
/// Matches are chosen leftmost-first on each line; once a match is accepted,
/// the scan resumes at its end so replacements never overlap. The number of
/// steps is bounded by [`ReplaceLimits::max_replacements`].
pub fn build_replace_all_plan(
    document: &SearchDocument,
    query: &str,
    replacement: &str,
    options: &SearchOptions,
    search_limits: &SearchLimits,
    replace_limits: &ReplaceLimits,
) -> Result<ReplaceAllPlan, FindReplaceError> {
    let folded_query = prepared_query(query, options, search_limits)?;
    let replacement_chars = replacement.chars().count();
    if replacement_chars > replace_limits.max_replacement_chars {
        return Err(FindReplaceError::ReplacementTooLarge {
            chars: replacement_chars,
            max: replace_limits.max_replacement_chars,
        });
    }
    if replace_limits.max_replacements == 0 {
        return Err(FindReplaceError::InvalidOptions(
            "max_replacements must be at least one",
        ));
    }

    let mut steps = Vec::new();
    for page in document.pages() {
        for (line_index, line) in page.lines.iter().enumerate() {
            let found = hits_in_line(
                page.page,
                line_index,
                line,
                &folded_query,
                options,
                search_limits,
            )?;
            let mut last_end = 0usize;
            for hit in found {
                if hit.start < last_end {
                    continue;
                }
                last_end = hit.end;
                steps.push(ReplaceStep {
                    page: hit.page,
                    line: hit.line,
                    start: hit.start,
                    end: hit.end,
                    replacement: replacement.to_owned(),
                });
                if steps.len() > replace_limits.max_replacements {
                    return Err(FindReplaceError::TooManyMatches {
                        cap: replace_limits.max_replacements,
                    });
                }
            }
        }
    }
    steps.sort_by_key(|step| (step.page, step.line, step.start));
    Ok(ReplaceAllPlan { steps })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn document(entries: &[(usize, &[&str])]) -> SearchDocument {
        let pages = entries
            .iter()
            .map(|(page, lines)| SearchPage {
                page: *page,
                lines: lines.iter().map(|line| (*line).to_owned()).collect(),
            })
            .collect();
        SearchDocument::new(pages).expect("valid document")
    }

    fn default_search() -> (SearchOptions, SearchLimits) {
        (SearchOptions::default(), SearchLimits::default())
    }

    #[test]
    fn case_sensitivity_controls_matching() {
        let doc = document(&[(0, &["Alpha alpha ALPHA"])]);
        let (options, limits) = default_search();
        let insensitive = find_all(&doc, "alpha", &options, &limits).expect("search");
        assert_eq!(insensitive.len(), 3);

        let sensitive = find_all(
            &doc,
            "alpha",
            &SearchOptions {
                case_sensitive: true,
                ..SearchOptions::default()
            },
            &limits,
        )
        .expect("search");
        assert_eq!(sensitive.len(), 1);
        assert_eq!(sensitive[0].start, 6);
    }

    #[test]
    fn whole_word_rejects_substrings() {
        let doc = document(&[(0, &["cat category cat"])]);
        let options = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        let limits = SearchLimits::default();
        let hits = find_all(&doc, "cat", &options, &limits).expect("search");
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].start, 0);
        assert_eq!(hits[1].start, 13);
        assert!(hits.iter().all(|hit| hit.end - hit.start == 3));
    }

    #[test]
    fn diacritics_fold_precomposed_and_combining_forms() {
        let doc = document(&[(0, &["café cafe\u{301} CAFÉ"])]);
        let options = SearchOptions {
            diacritic_insensitive: true,
            ..SearchOptions::default()
        };
        let limits = SearchLimits::default();
        let hits = find_all(&doc, "cafe", &options, &limits).expect("search");
        assert_eq!(hits.len(), 3);
        // "café" is four scalars; the combining mark in "cafe\u{301}" is
        // ignored by the fold but still counted in the original scalar range.
        assert_eq!(hits[0].start, 0);
        assert_eq!(hits[0].end, 4);
        assert_eq!(hits[1].start, 5);
        assert_eq!(hits[1].end, 9);
        assert_eq!(hits[2].start, 11);
        assert_eq!(hits[2].end, 15);
    }

    #[test]
    fn find_all_is_deterministically_ordered_regardless_of_input_order() {
        let a = document(&[(2, &["x ax", "ax"]), (0, &["ax zz ax"])]);
        let b = document(&[(0, &["ax zz ax"]), (2, &["x ax", "ax"])]);
        let (options, limits) = default_search();
        let first = find_all(&a, "ax", &options, &limits).expect("search");
        let second = find_all(&b, "ax", &options, &limits).expect("search");
        assert_eq!(first, second);
        let keys: Vec<(usize, usize, usize)> = first
            .iter()
            .map(|hit| (hit.page, hit.line, hit.start))
            .collect();
        assert_eq!(keys, vec![(0, 0, 0), (0, 0, 6), (2, 0, 2), (2, 1, 0)]);
    }

    #[test]
    fn empty_query_is_rejected() {
        let doc = document(&[(0, &["anything"])]);
        let (options, limits) = default_search();
        assert_eq!(
            find_all(&doc, "", &options, &limits),
            Err(FindReplaceError::EmptyQuery)
        );
        let folding = SearchOptions {
            diacritic_insensitive: true,
            ..SearchOptions::default()
        };
        assert_eq!(
            find_all(&doc, "\u{301}", &folding, &limits),
            Err(FindReplaceError::EmptyQuery)
        );
    }

    #[test]
    fn oversized_query_and_line_are_rejected() {
        let doc = document(&[(0, &["abcd"])]);
        let (options, _) = default_search();
        let limits = SearchLimits {
            max_query_chars: 2,
            ..SearchLimits::default()
        };
        assert_eq!(
            find_all(&doc, "abc", &options, &limits),
            Err(FindReplaceError::QueryTooLarge { chars: 3, max: 2 })
        );
        let limits = SearchLimits {
            max_line_chars: 3,
            ..SearchLimits::default()
        };
        assert_eq!(
            find_all(&doc, "a", &options, &limits),
            Err(FindReplaceError::LineTooLarge {
                page: 0,
                line: 0,
                chars: 4,
                max: 3,
            })
        );
    }

    #[test]
    fn invalid_whole_word_query_and_zero_cap_are_rejected() {
        let doc = document(&[(0, &["a (b) c"])]);
        let options = SearchOptions {
            whole_word: true,
            ..SearchOptions::default()
        };
        let limits = SearchLimits::default();
        assert!(matches!(
            find_all(&doc, "(b)", &options, &limits),
            Err(FindReplaceError::InvalidOptions(_))
        ));
        assert!(matches!(
            build_replace_all_plan(
                &doc,
                "b",
                "x",
                &SearchOptions::default(),
                &limits,
                &ReplaceLimits {
                    max_replacements: 0,
                    ..ReplaceLimits::default()
                },
            ),
            Err(FindReplaceError::InvalidOptions(_))
        ));
    }

    #[test]
    fn replace_all_plan_is_non_overlapping() {
        // "aaa" contains the literal "aa" at offsets 0 and 1; only the
        // leftmost match may be replaced.
        let doc = document(&[(0, &["aaaa"])]);
        let plan = build_replace_all_plan(
            &doc,
            "aa",
            "X",
            &SearchOptions::default(),
            &SearchLimits::default(),
            &ReplaceLimits::default(),
        )
        .expect("plan");
        assert_eq!(plan.replacement_count(), 2);
        assert_eq!((plan.steps[0].start, plan.steps[0].end), (0, 2));
        assert_eq!((plan.steps[1].start, plan.steps[1].end), (2, 4));
        assert!(plan.steps[0].end <= plan.steps[1].start);
        assert!(plan.steps.iter().all(|step| step.replacement == "X"));
    }

    #[test]
    fn replace_all_respects_the_cap() {
        let doc = document(&[(0, &["a a a a"])]);
        let limits = ReplaceLimits {
            max_replacements: 3,
            ..ReplaceLimits::default()
        };
        assert_eq!(
            build_replace_all_plan(
                &doc,
                "a",
                "b",
                &SearchOptions::default(),
                &SearchLimits::default(),
                &limits,
            ),
            Err(FindReplaceError::TooManyMatches { cap: 3 })
        );
    }

    #[test]
    fn replacement_size_is_validated() {
        let doc = document(&[(0, &["a"])]);
        let limits = ReplaceLimits {
            max_replacement_chars: 1,
            ..ReplaceLimits::default()
        };
        assert_eq!(
            build_replace_all_plan(
                &doc,
                "a",
                "ab",
                &SearchOptions::default(),
                &SearchLimits::default(),
                &limits,
            ),
            Err(FindReplaceError::ReplacementTooLarge { chars: 2, max: 1 })
        );
    }

    #[test]
    fn offsets_are_unicode_scalar_indices() {
        // 'é' is one scalar; offsets must not be UTF-8 byte offsets.
        let doc = document(&[(0, &["αβéγ"])]);
        let hits = find_all(
            &doc,
            "éγ",
            &SearchOptions::default(),
            &SearchLimits::default(),
        )
        .expect("search");
        assert_eq!(hits.len(), 1);
        assert_eq!((hits[0].start, hits[0].end), (2, 4));
    }

    #[test]
    fn duplicate_pages_are_rejected() {
        let pages = vec![
            SearchPage {
                page: 1,
                lines: Vec::new(),
            },
            SearchPage {
                page: 1,
                lines: Vec::new(),
            },
        ];
        assert_eq!(
            SearchDocument::new(pages),
            Err(FindReplaceError::DuplicatePage(1))
        );
    }
}
