//! Text extraction, layout reconstruction, and search contracts for PDFXI.
//!
//! This crate freezes the text-facing public surface of `pdfxi-render`'s sibling
//! lane without depending on a PDF backend. A [`TextExtractor`] turns page
//! indices into [`TextPage`] values; [`search`] then finds byte ranges in the
//! page's recovered text deterministically, entirely in memory.
//!
//! Like rendering, extraction is cooperative: callers pass a
//! [`CancellationToken`] and a progress callback so a closed document can stop
//! work and the UI can show advancement.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

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

/// Options controlling how [`search`] matches text.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct SearchOptions {
    /// When `true`, `A` and `a` are distinct; when `false` (the default),
    /// ASCII letters are compared case-insensitively.
    pub case_sensitive: bool,
}

impl SearchOptions {
    /// Create search options with the given `case_sensitive` setting.
    pub const fn new(case_sensitive: bool) -> Self {
        Self { case_sensitive }
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
}
