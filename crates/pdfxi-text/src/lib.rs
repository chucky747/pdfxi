//! Text extraction, layout reconstruction, and search contracts for PDFXI.
//!
//! This crate freezes the text-facing public surface of `pdfxi-render`'s sibling
//! lane without depending on a PDF backend. A [`TextExtractor`] turns page
//! indices into [`TextPage`] values. The single-page [`search`] helper finds
//! byte ranges in a page's recovered text, while [`SearchIndex`] builds a
//! reusable multi-page index whose [`SearchIndex::find`] returns character
//! offsets. [`TextSelection`] models a cross-page character range and copies
//! the selected text with documented newline/space rules.
//!
//! Like rendering, extraction is cooperative: callers pass a
//! [`CancellationToken`] and a progress callback so a closed document can stop
//! work and the UI can show advancement.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

use std::collections::BTreeMap;
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

/// A single run of text sharing one font, size, and baseline.
#[derive(Clone, Debug, PartialEq)]
pub struct TextSpan {
    /// The recovered UTF-8 text of this run.
    pub text: String,
    /// Left edge of the run in PDF page coordinates.
    pub x: f32,
    /// Baseline of the run in PDF page coordinates.
    pub y: f32,
    /// Width of the run in page coordinates.
    pub width: f32,
    /// Height of the run in page coordinates.
    pub height: f32,
    /// Font size of the run in page coordinates.
    pub font_size: f32,
}

impl TextSpan {
    /// Create a span with `text` anchored at `(x, y)` and zero metrics.
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

    /// Attach bounding-box and font metrics to this span.
    pub fn with_metrics(mut self, width: f32, height: f32, font_size: f32) -> Self {
        self.width = width;
        self.height = height;
        self.font_size = font_size;
        self
    }

    /// Return `true` when this span carries no characters.
    pub fn is_empty(&self) -> bool {
        self.text.is_empty()
    }
}

/// All text recovered from one page, in reading order.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct TextPage {
    page: usize,
    spans: Vec<TextSpan>,
}

impl TextPage {
    /// Create a text page from its zero-based `page` number and `spans`.
    pub fn new(page: usize, spans: Vec<TextSpan>) -> Self {
        Self { page, spans }
    }

    /// Create an empty text page.
    pub fn empty(page: usize) -> Self {
        Self::new(page, Vec::new())
    }

    /// Return the zero-based page number this text belongs to.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the recovered spans in reading order.
    pub fn spans(&self) -> &[TextSpan] {
        &self.spans
    }

    /// Return the number of recovered spans.
    pub fn span_count(&self) -> usize {
        self.spans.len()
    }

    /// Return `true` when the page has no spans.
    pub fn is_empty(&self) -> bool {
        self.spans.iter().all(TextSpan::is_empty)
    }

    /// Concatenate every span's text into one string.
    ///
    /// Search offsets returned by [`search`] are byte offsets into this string.
    /// Spans are joined without separators, so the result preserves exactly the
    /// characters the extractor recorded.
    pub fn text(&self) -> String {
        let mut out = String::new();
        for span in &self.spans {
            out.push_str(&span.text);
        }
        out
    }
}

/// A snapshot of how far a long-running operation has advanced.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Progress {
    /// Number of work units completed so far.
    pub completed: usize,
    /// Total number of work units, or `0` when unknown.
    pub total: usize,
}

impl Progress {
    /// Create a progress report from `completed` and `total` work units.
    pub const fn new(completed: usize, total: usize) -> Self {
        Self { completed, total }
    }

    /// Return the completed fraction in `0.0..=1.0`, or `0.0` when unknown.
    pub fn fraction(&self) -> f32 {
        if self.total == 0 {
            0.0
        } else {
            (self.completed.min(self.total) as f32) / (self.total as f32)
        }
    }

    /// Return `true` when all known work is finished.
    pub fn is_complete(&self) -> bool {
        self.total > 0 && self.completed >= self.total
    }
}

/// A cheap, cloneable token used to cooperatively cancel extraction.
///
/// All clones share one flag. Implementations check [`Self::is_cancelled`] (or
/// [`Self::check`]) between spans and unwind with [`TextError::Cancelled`].
#[derive(Clone, Debug)]
pub struct CancellationToken {
    cancelled: Arc<AtomicBool>,
}

impl CancellationToken {
    /// Create a token in the not-yet-cancelled state.
    pub fn new() -> Self {
        Self {
            cancelled: Arc::new(AtomicBool::new(false)),
        }
    }

    /// Request cancellation. This is idempotent and visible to all clones.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
    }

    /// Return `true` once [`Self::cancel`] has been called.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    /// Return [`TextError::Cancelled`] if cancellation has been requested.
    pub fn check(&self) -> Result<(), TextError> {
        if self.is_cancelled() {
            Err(TextError::Cancelled)
        } else {
            Ok(())
        }
    }
}

impl Default for CancellationToken {
    fn default() -> Self {
        Self::new()
    }
}

/// Errors produced by a [`TextExtractor`] implementation.
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum TextError {
    /// The operation was cancelled through its [`CancellationToken`].
    #[error("text extraction was cancelled")]
    Cancelled,
    /// The requested page does not exist in the document.
    #[error("page {0} is out of range")]
    PageOutOfRange(usize),
    /// A backend-specific failure that does not fit a typed variant.
    #[error("text extraction backend error: {0}")]
    Backend(String),
}

/// A backend capable of recovering text from document pages.
///
/// Implementations must validate page indices (returning
/// [`TextError::PageOutOfRange`] instead of panicking), report [`Progress`],
/// and check `cancellation` between steps so callers can stop promptly. The
/// trait is object-safe, so callers can hold `&dyn TextExtractor`.
pub trait TextExtractor {
    /// Recover the text of `page` in reading order.
    ///
    /// `progress` receives increasing reports and `cancellation` may stop the
    /// operation at any point.
    fn extract_page(
        &self,
        page: usize,
        cancellation: &CancellationToken,
        progress: &mut dyn FnMut(Progress),
    ) -> Result<TextPage, TextError>;

    /// Return the number of pages available to this extractor.
    fn page_count(&self) -> Result<usize, TextError>;
}

/// Options controlling how [`search`] and [`SearchIndex::find`] match text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// When `true`, `A` and `a` are distinct; when `false` (the default),
    /// ASCII letters are compared case-insensitively.
    pub case_sensitive: bool,
    /// When `true`, a match is accepted only when the characters immediately
    /// before and after it are non-word characters (or page boundaries).
    /// Defaults to `false`. Word characters are Unicode alphanumeric
    /// characters plus `_`, as decided by the boundary helpers shared with
    /// selection.
    pub whole_word: bool,
}

impl SearchOptions {
    /// Create search options with the given `case_sensitive` setting and
    /// `whole_word` disabled.
    pub const fn new(case_sensitive: bool) -> Self {
        Self {
            case_sensitive,
            whole_word: false,
        }
    }

    /// Create search options with both `case_sensitive` and `whole_word`
    /// explicitly set.
    pub const fn with_options(case_sensitive: bool, whole_word: bool) -> Self {
        Self {
            case_sensitive,
            whole_word,
        }
    }

    /// Return a copy of these options with `whole_word` set to `value`.
    pub const fn with_whole_word(self, value: bool) -> Self {
        Self {
            case_sensitive: self.case_sensitive,
            whole_word: value,
        }
    }
}

/// One non-overlapping match found by [`search`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SearchMatch {
    page: usize,
    start: usize,
    end: usize,
    text: String,
}

impl SearchMatch {
    /// Create a match spanning `start..end` (byte offsets) on `page`.
    pub fn new(page: usize, start: usize, end: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            start,
            end,
            text: text.into(),
        }
    }

    /// Return the zero-based page number the match was found on.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the inclusive start byte offset into [`TextPage::text`].
    pub fn start(&self) -> usize {
        self.start
    }

    /// Return the exclusive end byte offset into [`TextPage::text`].
    pub fn end(&self) -> usize {
        self.end
    }

    /// Return the matched substring in bytes.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Return `true` when the match spans no bytes (never produced by [`search`]).
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Return the matched text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// Find every non-overlapping occurrence of `query` in a [`TextPage`].
///
/// Offsets are byte offsets into [`TextPage::text`] and are deterministic for a
/// given page and query. Case-insensitive mode folds ASCII letters only, which
/// preserves byte lengths and therefore keeps offsets valid for Unicode text.
/// An empty query or an empty page yields no matches.
///
/// For character offsets, whole-word matching, and queries across many pages,
/// use [`SearchIndex`] instead.
///
/// ```text
/// use pdfxi_text::{search, SearchOptions, TextPage, TextSpan};
///
/// let page = TextPage::new(0, vec![TextSpan::new("Hello world", 0.0, 0.0)]);
/// let matches = search(&page, "WORLD", &SearchOptions::default());
/// assert_eq!(matches.len(), 1);
/// assert_eq!((matches[0].start(), matches[0].end()), (6, 11));
/// assert_eq!(matches[0].text(), "world");
/// ```
pub fn search(page: &TextPage, query: &str, options: &SearchOptions) -> Vec<SearchMatch> {
    let mut matches = Vec::new();
    if query.is_empty() {
        return matches;
    }

    let text = page.text();
    let mut from = 0usize;
    while let Some((start, end)) = find_from(&text, query, from, options.case_sensitive) {
        matches.push(SearchMatch::new(
            page.page(),
            start,
            end,
            text.get(start..end).unwrap_or_default(),
        ));
        // Non-overlapping matches: resume at the end of this one.
        from = end;
    }
    matches
}

/// Return the byte range of `needle` in `haystack` at or after `from`.
fn find_from(
    haystack: &str,
    needle: &str,
    from: usize,
    case_sensitive: bool,
) -> Option<(usize, usize)> {
    let h = haystack.as_bytes();
    let n = needle.as_bytes();
    if n.is_empty() || from > h.len() || n.len() > h.len().saturating_sub(from) {
        return None;
    }

    let last_start = h.len() - n.len();
    let mut i = from;
    while i <= last_start {
        let mut equal = true;
        for (offset, expected) in n.iter().enumerate() {
            let actual = h[i + offset];
            let (actual, expected) = if case_sensitive {
                (actual, *expected)
            } else {
                (actual.to_ascii_lowercase(), expected.to_ascii_lowercase())
            };
            if actual != expected {
                equal = false;
                break;
            }
        }
        if equal && haystack.is_char_boundary(i) && haystack.is_char_boundary(i + n.len()) {
            return Some((i, i + n.len()));
        }
        i += 1;
    }
    None
}

// ---------------------------------------------------------------------------
// Word/line boundaries
// ---------------------------------------------------------------------------

/// Word and line boundary rules shared by search and selection.
///
/// Keeping these predicates in one place guarantees that whole-word search and
/// selection copying agree on what counts as a word and as a line break.
mod boundaries {
    /// Return `true` when `c` counts as a word character.
    pub(super) fn is_word_char(c: char) -> bool {
        c.is_alphanumeric() || c == '_'
    }

    /// Return `true` when `c` terminates a visual line.
    pub(super) fn is_line_break(c: char) -> bool {
        matches!(c, '\n' | '\r' | '\u{2028}' | '\u{2029}')
    }

    /// Separator inserted when copy output joins two consecutive selected spans.
    ///
    /// Spans on different baselines are separated by a newline; spans on the
    /// same baseline are separated by a single space unless one of them already
    /// carries adjacent whitespace. Vertical distance uses a 0.5-unit epsilon
    /// so floating-point extraction noise does not create spurious lines.
    pub(super) fn span_separator(prev: &crate::TextSpan, next: &crate::TextSpan) -> &'static str {
        let prev_last = prev.text.chars().last();
        if prev_last.is_some_and(is_line_break) {
            // The previous run already ended its line; do not double it.
            return "";
        }
        if (prev.y - next.y).abs() > 0.5 {
            "\n"
        } else {
            let prev_ws = prev_last.is_some_and(char::is_whitespace);
            let next_ws = next.text.chars().next().is_some_and(char::is_whitespace);
            if prev_ws || next_ws { "" } else { " " }
        }
    }
}

// ---------------------------------------------------------------------------
// Case folding
// ---------------------------------------------------------------------------

/// Fold one character for case-insensitive comparison.
///
/// ASCII uppercase letters map to their lowercase form; every non-ASCII
/// character is returned unchanged. Folding never changes the number of
/// characters, so character offsets remain valid. The documented limitation is
/// that non-ASCII case pairs such as `É`/`é` or `Σ`/`σ` do **not** match each
/// other; full Unicode case folding would change character counts and break
/// offset stability, so it is deliberately not used here.
pub fn fold_char(c: char, case_sensitive: bool) -> char {
    if case_sensitive || !c.is_ascii() {
        c
    } else {
        c.to_ascii_lowercase()
    }
}

// ---------------------------------------------------------------------------
// Plain text with span offsets
// ---------------------------------------------------------------------------

/// The half-open character-offset range of one span inside [`PlainText`].
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct SpanOffset {
    /// Index into [`TextPage::spans`] identifying the source span.
    pub span: usize,
    /// Inclusive start character offset.
    pub start: usize,
    /// Exclusive end character offset.
    pub end: usize,
}

/// A page's plain text together with the span each character came from.
///
/// The `text` is identical to [`TextPage::text`]: span texts are concatenated
/// verbatim in reading order with no separators inserted. `span_offsets` maps
/// each span to its half-open range of character offsets, which is what
/// selection and search use to interpret offsets.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PlainText {
    text: String,
    span_offsets: Vec<SpanOffset>,
}

impl PlainText {
    /// Return the concatenated plain text.
    pub fn text(&self) -> &str {
        &self.text
    }

    /// Return the per-span character-offset ranges, in reading order.
    pub fn span_offsets(&self) -> &[SpanOffset] {
        &self.span_offsets
    }

    /// Return the number of characters (not bytes) in the plain text.
    pub fn char_len(&self) -> usize {
        self.text.chars().count()
    }

    /// Return the index of the span containing `char_offset`, if any.
    ///
    /// Offsets equal to the text length, or past it, return `None`.
    pub fn span_at(&self, char_offset: usize) -> Option<usize> {
        self.span_offsets
            .iter()
            .find(|o| char_offset >= o.start && char_offset < o.end)
            .map(|o| o.span)
    }
}

impl TextPage {
    /// Build a [`PlainText`] view of this page, exposing char offsets per span.
    ///
    /// This preserves span order exactly and is the canonical offset space for
    /// [`TextSelection`] and [`SearchIndex`].
    pub fn plain_text(&self) -> PlainText {
        let mut text = String::new();
        let mut cursor = 0usize;
        let mut span_offsets = Vec::with_capacity(self.spans.len());
        for (index, span) in self.spans.iter().enumerate() {
            let start = cursor;
            cursor += span.text.chars().count();
            text.push_str(&span.text);
            span_offsets.push(SpanOffset {
                span: index,
                start,
                end: cursor,
            });
        }
        PlainText { text, span_offsets }
    }

    /// Copy the character range `start..end` (character offsets) from this page.
    ///
    /// Only characters inside the selected range are copied. Consecutive
    /// selected spans are joined using the shared boundary rules: a newline
    /// between different baselines, a single space between runs on the same
    /// baseline, and nothing when a run already supplies adjacent whitespace
    /// or ends with a line break. Those separators appear only in the returned
    /// string; they are not part of the offset space. Out-of-range offsets are
    /// clamped and an inverted or empty range returns an empty string.
    pub fn copy_range(&self, start: usize, end: usize) -> String {
        let plain = self.plain_text();
        let limit = plain.char_len();
        let start = start.min(limit);
        let end = end.min(limit);
        if start >= end {
            return String::new();
        }

        let mut out = String::new();
        let mut previous_span: Option<usize> = None;
        for offset in &plain.span_offsets {
            let from = offset.start.max(start);
            let to = offset.end.min(end);
            if from >= to {
                continue;
            }
            if let Some(previous) = previous_span
                && let (Some(prev), Some(next)) =
                    (self.spans.get(previous), self.spans.get(offset.span))
            {
                out.push_str(boundaries::span_separator(prev, next));
            }
            // The offsets were derived from these same spans, so iterating the
            // span's characters and skipping/taking by count cannot panic.
            let chars: Vec<char> = self.spans[offset.span].text.chars().collect();
            for c in chars
                .iter()
                .skip(from - offset.start)
                .take(to.saturating_sub(from))
            {
                out.push(*c);
            }
            previous_span = Some(offset.span);
        }
        out
    }
}

// ---------------------------------------------------------------------------
// Multi-page search index
// ---------------------------------------------------------------------------

/// Fold one character through [`fold_char`] for an options set.
fn folded(c: char, options: &SearchOptions) -> char {
    fold_char(c, options.case_sensitive)
}

/// One match returned by [`SearchIndex::find`], using character offsets.
///
/// Offsets are indices into the concatenated [`TextPage::text`] of the matched
/// page, counted in Unicode scalar values rather than bytes.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct IndexMatch {
    page: usize,
    start: usize,
    end: usize,
    text: String,
}

impl IndexMatch {
    /// Create a match spanning `start..end` (character offsets) on `page`.
    pub fn new(page: usize, start: usize, end: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            start,
            end,
            text: text.into(),
        }
    }

    /// Return the zero-based page number the match was found on.
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

    /// Return the length of the match in characters.
    pub fn len(&self) -> usize {
        self.end - self.start
    }

    /// Return `true` when the match spans no characters.
    pub fn is_empty(&self) -> bool {
        self.start == self.end
    }

    /// Return the matched text.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// All matches found on one page by [`SearchIndex::find`].
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PageMatches {
    page: usize,
    matches: Vec<IndexMatch>,
}

impl PageMatches {
    /// Create a container for the matches found on `page`.
    pub fn new(page: usize, matches: Vec<IndexMatch>) -> Self {
        Self { page, matches }
    }

    /// Return the zero-based page number these matches belong to.
    pub fn page(&self) -> usize {
        self.page
    }

    /// Return the matches, ordered by ascending character offset.
    pub fn matches(&self) -> &[IndexMatch] {
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

    /// Iterate over the matches in offset order.
    pub fn iter(&self) -> impl Iterator<Item = &IndexMatch> {
        self.matches.iter()
    }
}

/// One page stored in a [`SearchIndex`].
#[derive(Clone, Debug)]
struct IndexEntry {
    page: usize,
    chars: Vec<char>,
}

/// An immutable, reusable text index over several pages.
///
/// Build the index once with [`Self::new`] or [`Self::from_pages`], then run any
/// number of queries through [`Self::find`]. Entries are sorted by page index
/// at construction time, so results are deterministic and independent of the
/// order in which pages were supplied. `find` does not mutate the index, which
/// makes repeated queries cheap and reproducible.
#[derive(Clone, Debug)]
pub struct SearchIndex {
    entries: Vec<IndexEntry>,
}

impl SearchIndex {
    /// Build an index from an ordered list of `(page_index, TextPage)` pairs.
    ///
    /// Entries are sorted by `page_index`; a stable sort keeps equal page
    /// indices in insertion order. [`TextPage::text`] is snapshotted, so later
    /// changes to the source pages cannot affect this index.
    pub fn new(pages: impl IntoIterator<Item = (usize, TextPage)>) -> Self {
        let mut entries: Vec<IndexEntry> = pages
            .into_iter()
            .map(|(page, text_page)| IndexEntry {
                page,
                chars: text_page.text().chars().collect(),
            })
            .collect();
        entries.sort_by_key(|entry| entry.page);
        Self { entries }
    }

    /// Build an index from pages, using each [`TextPage::page`] as its index.
    pub fn from_pages(pages: &[TextPage]) -> Self {
        Self::new(pages.iter().map(|page| (page.page(), page.clone())))
    }

    /// Return the number of indexed pages.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when the index holds no pages.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Iterate over the indexed page numbers in ascending order.
    pub fn page_indices(&self) -> impl Iterator<Item = usize> + '_ {
        self.entries.iter().map(|entry| entry.page)
    }

    /// Find every occurrence of `query` across all indexed pages.
    ///
    /// Results are returned as one [`PageMatches`] per page that has at least
    /// one match, ordered by ascending page number; matches within a page are
    /// ordered by ascending character offset. Offsets are character offsets
    /// into [`TextPage::text`], not byte offsets.
    ///
    /// Case-insensitive mode uses [`fold_char`]: ASCII letters fold, non-ASCII
    /// characters pass through unchanged. Whole-word mode accepts a match only
    /// when the characters adjacent to it are non-word characters (or page
    /// boundaries). Overlapping candidates are resolved leftmost-first and
    /// non-overlapping: after accepting a match, scanning resumes at its end,
    /// so an overlapping candidate that starts inside the accepted match is
    /// discarded. An empty query yields no matches.
    pub fn find(&self, query: &str, options: &SearchOptions) -> Vec<PageMatches> {
        let mut result = Vec::new();
        if query.is_empty() {
            return result;
        }
        let needle: Vec<char> = query.chars().map(|c| folded(c, options)).collect();
        if needle.is_empty() {
            return result;
        }

        for entry in &self.entries {
            let mut matches = Vec::new();
            let mut at = 0usize;
            while at + needle.len() <= entry.chars.len() {
                if let Some(end) = match_at(&entry.chars, &needle, at, options) {
                    let text: String = entry.chars[at..end].iter().collect();
                    matches.push(IndexMatch::new(entry.page, at, end, text));
                    at = end;
                } else {
                    at += 1;
                }
            }
            if !matches.is_empty() {
                result.push(PageMatches::new(entry.page, matches));
            }
        }
        result
    }
}

/// Return the end offset of `needle` in `haystack` starting at `at`, if matched.
fn match_at(
    haystack: &[char],
    needle: &[char],
    at: usize,
    options: &SearchOptions,
) -> Option<usize> {
    let end = at + needle.len();
    if end > haystack.len() {
        return None;
    }
    for (index, expected) in needle.iter().enumerate() {
        if folded(haystack[at + index], options) != *expected {
            return None;
        }
    }
    if options.whole_word {
        let before_ok = at == 0 || !boundaries::is_word_char(haystack[at - 1]);
        let after_ok = end == haystack.len() || !boundaries::is_word_char(haystack[end]);
        if !(before_ok && after_ok) {
            return None;
        }
    }
    Some(end)
}

// ---------------------------------------------------------------------------
// Cross-page selection
// ---------------------------------------------------------------------------

/// A (page, character-offset) cursor used as a selection endpoint.
///
/// Ordering is by `page` first and then `offset`, which is exactly the order
/// needed to normalize a selection that was dragged in either direction.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, PartialOrd, Ord)]
pub struct TextPosition {
    /// Zero-based page index.
    pub page: usize,
    /// Character offset into that page's [`TextPage::text`].
    pub offset: usize,
}

impl TextPosition {
    /// Create a position at `offset` characters into `page`.
    pub const fn new(page: usize, offset: usize) -> Self {
        Self { page, offset }
    }
}

/// A cross-page selection between an `anchor` and a `focus` position.
///
/// The two endpoints may be supplied in either order; [`Self::normalized`]
/// orders them by page and then character offset. A selection whose endpoints
/// are equal is empty.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct TextSelection {
    anchor: TextPosition,
    focus: TextPosition,
}

/// Convenience alias for [`TextSelection`].
pub type Selection = TextSelection;

impl TextSelection {
    /// Create a selection from two endpoints, regardless of drag direction.
    pub const fn new(anchor: TextPosition, focus: TextPosition) -> Self {
        Self { anchor, focus }
    }

    /// Return the endpoint where the selection began.
    pub const fn anchor(&self) -> TextPosition {
        self.anchor
    }

    /// Return the endpoint where the selection ended.
    pub const fn focus(&self) -> TextPosition {
        self.focus
    }

    /// Return `(start, end)` with `start <= end` in page/offset order.
    pub const fn normalized(&self) -> (TextPosition, TextPosition) {
        if self.anchor.page < self.focus.page
            || (self.anchor.page == self.focus.page && self.anchor.offset <= self.focus.offset)
        {
            (self.anchor, self.focus)
        } else {
            (self.focus, self.anchor)
        }
    }

    /// Return the normalized start position.
    pub const fn start(&self) -> TextPosition {
        if self.anchor.page < self.focus.page
            || (self.anchor.page == self.focus.page && self.anchor.offset <= self.focus.offset)
        {
            self.anchor
        } else {
            self.focus
        }
    }

    /// Return the normalized end position.
    pub const fn end(&self) -> TextPosition {
        if self.anchor.page < self.focus.page
            || (self.anchor.page == self.focus.page && self.anchor.offset <= self.focus.offset)
        {
            self.focus
        } else {
            self.anchor
        }
    }

    /// Return `true` when the anchor and focus are the same position.
    pub const fn is_empty(&self) -> bool {
        self.anchor.page == self.focus.page && self.anchor.offset == self.focus.offset
    }

    /// Return `true` when `pos` lies inside the half-open range `[start, end)`.
    ///
    /// The start endpoint is included and the end endpoint is excluded, so
    /// adjacent selections never both claim the same character.
    pub fn contains(&self, pos: TextPosition) -> bool {
        let (start, end) = self.normalized();
        pos >= start && pos < end
    }

    /// Iterate over every page the selection touches, from `start` to `end`.
    ///
    /// For an empty selection this yields the single anchor page.
    pub fn iter_pages(&self) -> impl Iterator<Item = usize> {
        let (start, end) = self.normalized();
        start.page..=end.page
    }

    /// Copy the selected text from `pages`.
    ///
    /// Pages are matched by [`TextPage::page`], so their order in the slice is
    /// irrelevant. The first page contributes `start.offset..end_of_page`, the
    /// last contributes `0..end.offset`, and any pages between contribute their
    /// full text. Within a page, [`TextPage::copy_range`] applies the shared
    /// newline/space rules; whole pages are joined by a single `\n` (an empty
    /// page contributes an empty chunk). An empty selection returns an empty
    /// string.
    pub fn copy_text(&self, pages: &[TextPage]) -> String {
        let (start, end) = self.normalized();
        if start == end {
            return String::new();
        }

        let by_page: BTreeMap<usize, &TextPage> = pages.iter().map(|p| (p.page(), p)).collect();
        let mut chunks: Vec<String> = Vec::new();
        for page_number in start.page..=end.page {
            let Some(page) = by_page.get(&page_number) else {
                continue;
            };
            let from = if page_number == start.page {
                start.offset
            } else {
                0
            };
            let to = if page_number == end.page {
                end.offset
            } else {
                page.text().chars().count()
            };
            chunks.push(page.copy_range(from, to));
        }
        chunks.join("\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A deterministic extractor backed by a fixed set of pages.
    struct MockTextExtractor {
        pages: Vec<TextPage>,
    }

    impl MockTextExtractor {
        fn new(pages: Vec<TextPage>) -> Self {
            Self { pages }
        }
    }

    impl TextExtractor for MockTextExtractor {
        fn extract_page(
            &self,
            page: usize,
            cancellation: &CancellationToken,
            progress: &mut dyn FnMut(Progress),
        ) -> Result<TextPage, TextError> {
            cancellation.check()?;
            if page >= self.pages.len() {
                return Err(TextError::PageOutOfRange(page));
            }
            progress(Progress::new(0, 2));
            cancellation.check()?;
            let result = self.pages[page].clone();
            progress(Progress::new(1, 2));
            cancellation.check()?;
            progress(Progress::new(2, 2));
            Ok(result)
        }

        fn page_count(&self) -> Result<usize, TextError> {
            Ok(self.pages.len())
        }
    }

    #[test]
    fn mock_extractor_returns_spans_and_reports_progress() {
        let extractor = MockTextExtractor::new(vec![TextPage::new(
            0,
            vec![
                TextSpan::new("Hello ", 1.0, 2.0).with_metrics(10.0, 4.0, 12.0),
                TextSpan::new("world", 11.0, 2.0),
            ],
        )]);
        let mut reports = Vec::new();
        let page = extractor
            .extract_page(0, &CancellationToken::new(), &mut |p| reports.push(p))
            .expect("extract succeeds");

        assert_eq!(extractor.page_count(), Ok(1));
        assert_eq!(page.page(), 0);
        assert_eq!(page.span_count(), 2);
        assert_eq!(page.text(), "Hello world");
        assert_eq!(page.spans()[0].font_size, 12.0);
        assert_eq!(page.spans()[0].width, 10.0);
        assert_eq!(
            reports,
            vec![
                Progress::new(0, 2),
                Progress::new(1, 2),
                Progress::new(2, 2)
            ]
        );
    }

    #[test]
    fn cancellation_stops_extraction_before_work() {
        let extractor = MockTextExtractor::new(vec![TextPage::empty(0)]);
        let token = CancellationToken::new();
        token.cancel();
        let mut reports = Vec::new();
        let result = extractor.extract_page(0, &token, &mut |p| reports.push(p));

        assert_eq!(result, Err(TextError::Cancelled));
        assert!(reports.is_empty());
    }

    #[test]
    fn empty_pages_and_out_of_range_pages_are_handled() {
        let extractor = MockTextExtractor::new(vec![TextPage::empty(0)]);
        assert_eq!(extractor.page_count(), Ok(1));
        let empty = extractor
            .extract_page(0, &CancellationToken::new(), &mut |_| {})
            .expect("empty page still extracts");
        assert!(empty.is_empty());
        assert_eq!(empty.text(), "");

        let result = extractor.extract_page(1, &CancellationToken::new(), &mut |_| {});
        assert_eq!(result, Err(TextError::PageOutOfRange(1)));

        let no_pages = MockTextExtractor::new(Vec::new());
        assert_eq!(no_pages.page_count(), Ok(0));
        assert_eq!(
            no_pages.extract_page(0, &CancellationToken::new(), &mut |_| {}),
            Err(TextError::PageOutOfRange(0))
        );
    }

    #[test]
    fn search_returns_exact_byte_offsets_in_reading_order() {
        let page = TextPage::new(
            7,
            vec![
                TextSpan::new("The quick ", 0.0, 0.0),
                TextSpan::new("brown fox", 10.0, 0.0),
                TextSpan::new(" jumps", 20.0, 0.0),
            ],
        );
        assert_eq!(page.text(), "The quick brown fox jumps");

        let matches = search(&page, "brown fox", &SearchOptions::default());
        assert_eq!(matches.len(), 1);
        let m = &matches[0];
        assert_eq!(m.page(), 7);
        assert_eq!((m.start(), m.end()), (10, 19));
        assert_eq!(m.len(), 9);
        assert_eq!(&page.text()[m.start()..m.end()], "brown fox");
        assert_eq!(m.text(), "brown fox");
    }

    #[test]
    fn search_is_case_insensitive_by_default_and_can_be_exact() {
        let page = TextPage::new(0, vec![TextSpan::new("Alpha alpha ALPHA", 0.0, 0.0)]);

        let folded = search(&page, "alpha", &SearchOptions::default());
        assert_eq!(folded.len(), 3);
        assert_eq!(
            folded.iter().map(SearchMatch::start).collect::<Vec<_>>(),
            vec![0, 6, 12]
        );

        let exact = search(&page, "alpha", &SearchOptions::new(true));
        assert_eq!(exact.len(), 1);
        assert_eq!(exact[0].start(), 6);
    }

    #[test]
    fn search_handles_repeats_empty_queries_and_multibyte_text() {
        let page = TextPage::new(0, vec![TextSpan::new("aaaa", 0.0, 0.0)]);
        let repeated = search(&page, "aa", &SearchOptions::default());
        assert_eq!(
            repeated.iter().map(SearchMatch::start).collect::<Vec<_>>(),
            vec![0, 2],
            "matches must not overlap"
        );
        assert!(search(&page, "", &SearchOptions::default()).is_empty());

        let unicode = TextPage::new(0, vec![TextSpan::new("café — naïve", 0.0, 0.0)]);
        let matches = search(&unicode, "naïve", &SearchOptions::default());
        assert_eq!(matches.len(), 1);
        // Before "naïve": c,a,f (3) + é (2) + space (1) + — (3) + space (1) = 10 bytes.
        assert_eq!((matches[0].start(), matches[0].end()), (10, 16));
        assert_eq!(&unicode.text()[10..16], "naïve");
    }

    // ------------------------------------------------------------------
    // Multi-page search index
    // ------------------------------------------------------------------

    fn page_with_text(page: usize, text: &str) -> TextPage {
        TextPage::new(page, vec![TextSpan::new(text, 0.0, 0.0)])
    }

    #[test]
    fn index_orders_results_by_page_then_offset() {
        // Pages are supplied out of order to prove construction sorts them.
        let index = SearchIndex::new(vec![
            (2, page_with_text(2, "hit here and hit there")),
            (0, page_with_text(0, "hit first")),
            (1, page_with_text(1, "no match here")),
            (3, page_with_text(3, "hit last hit")),
        ]);
        assert_eq!(index.page_indices().collect::<Vec<_>>(), vec![0, 1, 2, 3]);

        let found = index.find("hit", &SearchOptions::default());
        assert_eq!(
            found.iter().map(PageMatches::page).collect::<Vec<_>>(),
            vec![0, 2, 3],
            "pages with no match are omitted"
        );
        assert_eq!(found[0].matches()[0].start(), 0);
        assert_eq!(
            found[1]
                .matches()
                .iter()
                .map(IndexMatch::start)
                .collect::<Vec<_>>(),
            vec![0, 13],
            "matches within a page ascend by offset"
        );
    }

    #[test]
    fn index_is_case_insensitive_by_default_and_can_be_exact() {
        let index = SearchIndex::from_pages(&[page_with_text(0, "Raven raven RAVEN")]);
        let folded = index.find("raven", &SearchOptions::default());
        assert_eq!(folded.len(), 1);
        assert_eq!(folded[0].len(), 3);
        assert_eq!(
            folded[0]
                .matches()
                .iter()
                .map(IndexMatch::start)
                .collect::<Vec<_>>(),
            vec![0, 6, 12]
        );
        assert_eq!(
            folded[0].matches()[1].text(),
            "raven",
            "matched text preserves the original casing"
        );

        let exact = index.find("raven", &SearchOptions::new(true));
        assert_eq!(exact[0].len(), 1);
        assert_eq!(exact[0].matches()[0].start(), 6);
    }

    #[test]
    fn index_honors_whole_word_option() {
        let index = SearchIndex::from_pages(&[page_with_text(0, "cat category cat")]);
        let words = index.find("cat", &SearchOptions::new(false).with_whole_word(true));
        assert_eq!(words.len(), 1);
        assert_eq!(
            words[0]
                .matches()
                .iter()
                .map(IndexMatch::start)
                .collect::<Vec<_>>(),
            vec![0, 13],
            "the 'cat' inside 'category' is not a whole word"
        );

        let any = index.find("cat", &SearchOptions::default());
        assert_eq!(any[0].len(), 3);
    }

    #[test]
    fn index_returns_empty_for_no_match_and_empty_query() {
        let index = SearchIndex::from_pages(&[page_with_text(0, "abcdef")]);
        assert!(index.find("zzz", &SearchOptions::default()).is_empty());
        assert!(index.find("", &SearchOptions::default()).is_empty());
        let empty_index = SearchIndex::new(Vec::new());
        assert!(empty_index.is_empty());
        assert_eq!(empty_index.len(), 0);
        assert!(empty_index.find("a", &SearchOptions::default()).is_empty());
    }

    #[test]
    fn index_resolves_overlapping_matches_leftmost_first() {
        // "ana" occurs at 1 and 3 in "banana"; the occurrences overlap.
        let index = SearchIndex::from_pages(&[page_with_text(0, "banana")]);
        let found = index.find("ana", &SearchOptions::default());
        assert_eq!(found[0].len(), 1, "overlapping candidates are discarded");
        assert_eq!(found[0].matches()[0].start(), 1);
        assert_eq!(found[0].matches()[0].end(), 4);

        // A repeating pattern yields evenly spaced non-overlapping matches.
        let repeats = SearchIndex::from_pages(&[page_with_text(0, "aaaa")]);
        let found = repeats.find("aa", &SearchOptions::default());
        assert_eq!(
            found[0]
                .matches()
                .iter()
                .map(IndexMatch::start)
                .collect::<Vec<_>>(),
            vec![0, 2]
        );
    }

    #[test]
    fn index_uses_character_offsets_for_multibyte_text() {
        let index = SearchIndex::from_pages(&[page_with_text(0, "café — naïve")]);
        let found = index.find("naïve", &SearchOptions::default());
        assert_eq!(found.len(), 1);
        let m = &found[0].matches()[0];
        assert_eq!(
            (m.start(), m.end()),
            (7, 12),
            "offsets count chars, not bytes"
        );
        assert_eq!(m.text(), "naïve");
        assert_eq!(m.len(), 5);
    }

    #[test]
    fn index_is_deterministic_when_reused() {
        let index = SearchIndex::new(vec![
            (1, page_with_text(1, "alpha beta alpha")),
            (0, page_with_text(0, "alpha gamma")),
        ]);
        let first = index.find("alpha", &SearchOptions::default());
        let second = index.find("alpha", &SearchOptions::default());
        assert_eq!(first, second, "repeated queries return identical results");

        // Interleaving other queries must not disturb the original result.
        let _ = index.find("beta", &SearchOptions::default());
        let _ = index.find("gamma", &SearchOptions::new(true));
        assert_eq!(first, index.find("alpha", &SearchOptions::default()));
    }

    #[test]
    fn fold_char_folds_ascii_and_passes_non_ascii_through() {
        assert_eq!(fold_char('A', false), 'a');
        assert_eq!(fold_char('Z', false), 'z');
        assert_eq!(fold_char('a', true), 'a');
        assert_eq!(fold_char('É', false), 'É');
        assert_eq!(fold_char('Σ', false), 'Σ');
        let non_ascii = SearchIndex::from_pages(&[page_with_text(0, "ÉCOLE école")]);
        assert_eq!(
            non_ascii.find("école", &SearchOptions::default())[0].len(),
            1,
            "only the lowercase 'école' matches; 'ÉCOLE' is deliberately distinct"
        );
    }

    // ------------------------------------------------------------------
    // Plain text, offsets, and copy helpers
    // ------------------------------------------------------------------

    #[test]
    fn plain_text_exposes_span_character_offsets() {
        let page = TextPage::new(
            0,
            vec![
                TextSpan::new("ab", 0.0, 0.0),
                TextSpan::new("cde", 1.0, 0.0),
            ],
        );
        let plain = page.plain_text();
        assert_eq!(plain.text(), "abcde");
        assert_eq!(plain.char_len(), 5);
        assert_eq!(
            plain.span_offsets(),
            &[
                SpanOffset {
                    span: 0,
                    start: 0,
                    end: 2
                },
                SpanOffset {
                    span: 1,
                    start: 2,
                    end: 5
                }
            ]
        );
        assert_eq!(plain.span_at(0), Some(0));
        assert_eq!(plain.span_at(3), Some(1));
        assert_eq!(plain.span_at(5), None);
    }

    #[test]
    fn copy_range_uses_newline_and_space_rules() {
        let same_line = TextPage::new(
            0,
            vec![
                TextSpan::new("Hello", 0.0, 0.0),
                TextSpan::new("world", 10.0, 0.0),
            ],
        );
        assert_eq!(same_line.copy_range(0, 10), "Hello world");

        let next_line = TextPage::new(
            0,
            vec![
                TextSpan::new("first", 0.0, 0.0),
                TextSpan::new("second", 0.0, 20.0),
            ],
        );
        assert_eq!(next_line.copy_range(0, 11), "first\nsecond");

        let with_whitespace = TextPage::new(
            0,
            vec![
                TextSpan::new("Hi ", 0.0, 0.0),
                TextSpan::new("there", 5.0, 0.0),
            ],
        );
        assert_eq!(with_whitespace.copy_range(0, 8), "Hi there");
    }

    #[test]
    fn copy_range_clamps_offsets_and_handles_multibyte_characters() {
        let page = page_with_text(0, "café — naïve");
        assert_eq!(page.copy_range(4, 7), " — ");
        assert_eq!(page.copy_range(0, 100), "café — naïve");
        assert!(page.copy_range(5, 5).is_empty());
        assert!(page.copy_range(7, 2).is_empty());
    }

    // ------------------------------------------------------------------
    // Cross-page selection
    // ------------------------------------------------------------------

    #[test]
    fn selection_normalizes_in_both_directions() {
        let forward = TextSelection::new(TextPosition::new(0, 2), TextPosition::new(2, 5));
        let backward = TextSelection::new(TextPosition::new(2, 5), TextPosition::new(0, 2));
        for selection in [forward, backward] {
            assert_eq!(selection.start(), TextPosition::new(0, 2));
            assert_eq!(selection.end(), TextPosition::new(2, 5));
            assert_eq!(
                selection.normalized(),
                (TextPosition::new(0, 2), TextPosition::new(2, 5))
            );
            assert!(!selection.is_empty());
        }
        assert_eq!(forward.anchor(), TextPosition::new(0, 2));
        assert_eq!(forward.focus(), TextPosition::new(2, 5));
        assert_eq!(backward.anchor(), TextPosition::new(2, 5));
        assert_eq!(backward.focus(), TextPosition::new(0, 2));
        assert!(TextSelection::new(TextPosition::new(1, 3), TextPosition::new(1, 3)).is_empty());
    }

    #[test]
    fn selection_contains_and_iter_pages() {
        let selection = Selection::new(TextPosition::new(0, 2), TextPosition::new(2, 1));
        assert_eq!(selection.iter_pages().collect::<Vec<_>>(), vec![0, 1, 2]);
        assert!(!selection.contains(TextPosition::new(0, 1)));
        assert!(
            selection.contains(TextPosition::new(0, 2)),
            "start is inclusive"
        );
        assert!(selection.contains(TextPosition::new(1, 0)));
        assert!(
            !selection.contains(TextPosition::new(2, 1)),
            "end is exclusive"
        );
        assert!(!selection.contains(TextPosition::new(3, 0)));
    }

    #[test]
    fn selection_copies_text_across_pages() {
        let pages = vec![
            page_with_text(0, "Hello"),
            TextPage::new(
                1,
                vec![
                    TextSpan::new("world", 0.0, 0.0),
                    TextSpan::new("wide", 0.0, 20.0),
                ],
            ),
            page_with_text(2, "end"),
        ];
        // Supplied in reverse order to prove copy matches by page number.
        let reordered = vec![pages[2].clone(), pages[0].clone(), pages[1].clone()];
        let selection = TextSelection::new(TextPosition::new(0, 0), TextPosition::new(2, 3));
        assert_eq!(selection.copy_text(&reordered), "Hello\nworld\nwide\nend");

        let reversed = TextSelection::new(TextPosition::new(2, 3), TextPosition::new(0, 0));
        assert_eq!(reversed.copy_text(&pages), "Hello\nworld\nwide\nend");
        assert!(selection.copy_text(&[]).is_empty());
    }

    #[test]
    fn index_build_and_find_leave_cancellation_and_progress_untouched() {
        let extractor = MockTextExtractor::new(vec![page_with_text(0, "searchable text")]);
        let token = CancellationToken::new();
        let mut reports = Vec::new();
        let page = extractor
            .extract_page(0, &token, &mut |p| reports.push(p))
            .expect("extraction succeeds");

        let index = SearchIndex::from_pages(&[page]);
        let found = index.find("search", &SearchOptions::default());
        assert_eq!(found.len(), 1);

        assert!(!token.is_cancelled(), "the index must not cancel the token");
        assert_eq!(
            reports,
            vec![
                Progress::new(0, 2),
                Progress::new(1, 2),
                Progress::new(2, 2)
            ],
            "progress reporting is unaffected by indexing"
        );
    }
}
