//! Reflow layout and a read-aloud cursor over caller-supplied text blocks.
//!
//! [`ReflowLayout`] takes a set of [`TextBlock`]s (caller-supplied extracted
//! page text with an explicit reading order) and a [`FontMetric`], then produces
//! a deterministic, width-bound column layout of [`ReflowLine`]s grouped into
//! [`ReflowBlock`]s. All geometry uses integer hundredths of a point so the
//! output is fully deterministic and comparable. The layout also exposes page
//! navigation and scroll clamping for a host scroll view.
//!
//! A [`ReadAloudCursor`] maps character offsets in the canonical reading text
//! back to source blocks. It reuses the word/sentence chunking in
//! [`crate::read_aloud`] so the same speech plan the host speaks is the text the
//! cursor highlights. Nothing here touches a GUI, a font rasterizer, or an OS
//! speech engine; rendering and audio remain explicit host boundaries.

use crate::read_aloud::{ChunkMode, ReadAloudPlan, SpeechChunk, VoiceSettings, build_chunks};
use crate::selection::OwnedSpan;

// ---------------------------------------------------------------------------
// Bounds and errors
// ---------------------------------------------------------------------------

/// Maximum number of text blocks accepted by a layout.
pub const MAX_REFLOW_BLOCKS: usize = 10_000;
/// Maximum number of laid-out lines a layout may contain.
pub const MAX_REFLOW_LINES: usize = 100_000;
/// Maximum number of characters in the canonical reading text.
pub const MAX_REFLOW_TEXT_LEN: usize = 2_000_000;

/// Typed errors produced while building or navigating a [`ReflowLayout`].
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum ReflowError {
    /// No block contained non-empty text.
    #[error("reflow input has no non-empty text blocks")]
    EmptyInput,
    /// A font metric field was zero or otherwise invalid.
    #[error("invalid font metric: {field}")]
    InvalidMetric {
        /// Name of the offending field.
        field: &'static str,
    },
    /// Too many text blocks were supplied.
    #[error("too many reflow blocks (limit {limit})")]
    TooManyBlocks {
        /// Maximum number of blocks allowed.
        limit: usize,
    },
    /// The layout would contain too many lines.
    #[error("too many reflow lines (limit {limit})")]
    TooManyLines {
        /// Maximum number of lines allowed.
        limit: usize,
    },
    /// The canonical reading text exceeded the character budget.
    #[error("reflow text too long (limit {limit})")]
    TextTooLong {
        /// Maximum number of characters allowed.
        limit: usize,
    },
    /// A character offset was outside the canonical reading text.
    #[error("text offset {offset} out of range (length {len})")]
    OffsetOutOfRange {
        /// Offset that was requested.
        offset: usize,
        /// Length of the canonical text.
        len: usize,
    },
    /// A page number was not present in the layout.
    #[error("page {0} is not present in the layout")]
    UnknownPage(usize),
    /// A block index was not present in the layout.
    #[error("block {0} is not present in the layout")]
    UnknownBlock(usize),
}

// ---------------------------------------------------------------------------
// Inputs
// ---------------------------------------------------------------------------

/// One caller-supplied block of extracted page text.
///
/// `page` is the zero-based source page and `order` is the caller's reading
/// position within that page. Blocks are laid out in ascending `(page, order)`
/// order, which makes the result independent of the order the caller supplied.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TextBlock {
    /// Zero-based source page number.
    pub page: usize,
    /// Reading position within the page.
    pub order: usize,
    /// Plain text of the block.
    pub text: String,
}

impl TextBlock {
    /// Create a text block.
    pub fn new(page: usize, order: usize, text: impl Into<String>) -> Self {
        Self {
            page,
            order,
            text: text.into(),
        }
    }
}

/// Fixed-point font metrics used to wrap and space reflow lines.
///
/// All values are in hundredths of a point. Wrapping uses integer division:
/// a line holds at most `column_width_hp / char_advance_hp` characters, which
/// keeps line breaks identical on every host.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FontMetric {
    /// Advance width of one character, in hundredths of a point.
    pub char_advance_hp: u32,
    /// Target column width, in hundredths of a point.
    pub column_width_hp: u32,
    /// Line height, in hundredths of a point.
    pub line_height_hp: u32,
}

impl FontMetric {
    /// A sensible default: 80 columns at 5 pt/char and 12 pt line height.
    pub const DEFAULT: Self = Self {
        char_advance_hp: 500,
        column_width_hp: 40_000,
        line_height_hp: 1_200,
    };

    /// Validate and construct a font metric. Each field must be non-zero.
    pub fn new(
        char_advance_hp: u32,
        column_width_hp: u32,
        line_height_hp: u32,
    ) -> Result<Self, ReflowError> {
        if char_advance_hp == 0 {
            return Err(ReflowError::InvalidMetric {
                field: "char_advance_hp",
            });
        }
        if column_width_hp == 0 {
            return Err(ReflowError::InvalidMetric {
                field: "column_width_hp",
            });
        }
        if line_height_hp == 0 {
            return Err(ReflowError::InvalidMetric {
                field: "line_height_hp",
            });
        }
        Ok(Self {
            char_advance_hp,
            column_width_hp,
            line_height_hp,
        })
    }

    /// Maximum number of characters that fit on one line (at least one).
    pub fn max_chars_per_line(&self) -> usize {
        (self.column_width_hp / self.char_advance_hp).max(1) as usize
    }
}

impl Default for FontMetric {
    fn default() -> Self {
        Self::DEFAULT
    }
}

// ---------------------------------------------------------------------------
// Layout outputs
// ---------------------------------------------------------------------------

/// One deterministic wrapped line in a reflow layout.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflowLine {
    /// Index of the block this line belongs to.
    pub block_index: usize,
    /// Index of the paragraph this line belongs to.
    pub paragraph_index: usize,
    /// Zero-based source page.
    pub page: usize,
    /// Zero-based line index within the block.
    pub line_index: usize,
    /// Line text (already wrapped, no trailing newline).
    pub text: String,
    /// Start character offset in the canonical reading text.
    pub start: usize,
    /// End character offset (exclusive) in the canonical reading text.
    pub end: usize,
    /// Top edge in hundredths of a point.
    pub top_hp: u32,
    /// Bottom edge in hundredths of a point.
    pub bottom_hp: u32,
}

/// One laid-out block, grouping the lines derived from a [`TextBlock`].
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflowBlock {
    /// Index of this block in the layout.
    pub index: usize,
    /// Zero-based source page.
    pub page: usize,
    /// Reading order supplied by the caller.
    pub order: usize,
    /// Full block text (canonical slice, words separated by single spaces).
    pub text: String,
    /// Start character offset in the canonical reading text.
    pub char_start: usize,
    /// End character offset (exclusive) in the canonical reading text.
    pub char_end: usize,
    /// Index of the first line of this block in [`ReflowLayout::lines`].
    pub first_line: usize,
    /// Number of lines in this block.
    pub line_count: usize,
}

/// One blank-line-delimited paragraph in a reflow layout.
///
/// A source [`TextBlock`] may contain several paragraphs separated by blank
/// lines. Each paragraph is wrapped independently, so its line range and
/// canonical character span are stable and can be used as a read-aloud
/// segmentation boundary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflowParagraph {
    /// Index of this paragraph within its block.
    pub index: usize,
    /// Index of the block that owns this paragraph.
    pub block_index: usize,
    /// Zero-based source page.
    pub page: usize,
    /// Start character offset in the canonical reading text.
    pub char_start: usize,
    /// End character offset (exclusive) in the canonical reading text.
    pub char_end: usize,
    /// Index of the first line of this paragraph in [`ReflowLayout::lines`].
    pub first_line: usize,
    /// Number of lines in this paragraph.
    pub line_count: usize,
}

/// Split block text into paragraphs on blank lines.
///
/// A *blank line* is a line containing only whitespace; a run of consecutive
/// blank lines acts as a single paragraph break. A single newline between
/// non-blank lines is a soft break and is kept inside the same paragraph (it is
/// simply whitespace between words). Each returned paragraph is a trimmed,
/// single-space-joined string in reading order; all-whitespace input returns an
/// empty vector. The function is deterministic and allocates only caller-owned
/// text.
pub fn segment_paragraphs(text: &str) -> Vec<String> {
    let mut out = Vec::new();
    let mut current: Vec<&str> = Vec::new();
    for line in text.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            if !current.is_empty() {
                out.push(current.join(" "));
                current.clear();
            }
        } else {
            current.push(trimmed);
        }
    }
    if !current.is_empty() {
        out.push(current.join(" "));
    }
    out
}

/// Internal per-word record mapping a word to its block and canonical offsets.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct WordRecord {
    block_index: usize,
    start: usize,
    end: usize,
}

/// A deterministic, width-bound reflow layout of caller-supplied text.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReflowLayout {
    metric: FontMetric,
    blocks: Vec<ReflowBlock>,
    block_ranges: Vec<(usize, usize)>,
    paragraphs: Vec<ReflowParagraph>,
    lines: Vec<ReflowLine>,
    canonical: String,
    word_records: Vec<WordRecord>,
    page_starts: Vec<(usize, usize)>,
    content_height_hp: u32,
}

impl ReflowLayout {
    /// Build a deterministic reflow layout from text blocks and a font metric.
    ///
    /// Empty blocks are skipped. Remaining blocks are sorted by `(page, order)`
    /// and wrapped independently. The canonical reading text joins all words in
    /// layout order with single spaces; its character offsets are what a
    /// [`ReadAloudCursor`] maps back to blocks.
    pub fn new(blocks: &[TextBlock], metric: FontMetric) -> Result<Self, ReflowError> {
        if blocks.len() > MAX_REFLOW_BLOCKS {
            return Err(ReflowError::TooManyBlocks {
                limit: MAX_REFLOW_BLOCKS,
            });
        }

        let mut accepted: Vec<TextBlock> = blocks
            .iter()
            .filter(|block| !block.text.trim().is_empty())
            .cloned()
            .collect();
        if accepted.is_empty() {
            return Err(ReflowError::EmptyInput);
        }
        accepted.sort_by_key(|block| (block.page, block.order));

        // Build the canonical reading text and remember per-word char offsets.
        let mut canonical = String::new();
        let mut word_records: Vec<WordRecord> = Vec::new();
        let mut word_starts: Vec<usize> = Vec::new();
        let mut char_ranges: Vec<(usize, usize)> = Vec::new();
        for block in &accepted {
            let char_start = canonical.chars().count();
            let word_start = word_records.len();
            for word in block.text.split_whitespace() {
                if !canonical.is_empty() {
                    canonical.push(' ');
                }
                let start = canonical.chars().count();
                canonical.push_str(word);
                let end = canonical.chars().count();
                word_records.push(WordRecord {
                    block_index: word_start,
                    start,
                    end,
                });
            }
            // Fix each record's block index now that it is known.
            let block_index = char_ranges.len();
            for record in word_records.iter_mut().skip(word_start) {
                record.block_index = block_index;
            }
            word_starts.push(word_start);
            char_ranges.push((char_start, canonical.chars().count()));
        }
        if canonical.chars().count() > MAX_REFLOW_TEXT_LEN {
            return Err(ReflowError::TextTooLong {
                limit: MAX_REFLOW_TEXT_LEN,
            });
        }

        // Wrap each paragraph's words into lines. Paragraph breaks come from
        // blank lines in the source text; the canonical word stream (and hence
        // every word record offset) is unchanged by that segmentation.
        let max_chars = metric.max_chars_per_line();
        let mut lines: Vec<ReflowLine> = Vec::new();
        let mut laid_out: Vec<ReflowBlock> = Vec::new();
        let mut paragraph_out: Vec<ReflowParagraph> = Vec::new();
        for (block_index, block) in accepted.iter().enumerate() {
            let word_start = word_starts[block_index];
            let (char_start, char_end) = char_ranges[block_index];
            let first_line = lines.len();
            let mut line_no = 0usize;
            let mut cursor = word_start;
            for (paragraph_index, paragraph) in segment_paragraphs(&block.text).iter().enumerate() {
                let words: Vec<&str> = paragraph.split_whitespace().collect();
                if words.is_empty() {
                    continue;
                }
                let para_first_line = lines.len();
                let para_char_start = word_records[cursor].start;
                let para_char_end = word_records[cursor + words.len() - 1].end;
                let mut text = String::new();
                let mut len = 0usize;
                let mut start = 0usize;
                let mut end = 0usize;
                let mut count = 0usize;
                for word in &words {
                    let record = word_records[cursor];
                    cursor += 1;
                    let wlen = word.chars().count();
                    if count > 0 && len + 1 + wlen > max_chars {
                        push_line(
                            &mut lines,
                            &mut text,
                            LineSpec {
                                block_index,
                                paragraph_index,
                                page: block.page,
                                line_index: line_no,
                                start,
                                end,
                                line_height_hp: metric.line_height_hp,
                            },
                        )?;
                        line_no += 1;
                        len = 0;
                        count = 0;
                    }
                    if count > 0 {
                        text.push(' ');
                        len += 1;
                    }
                    text.push_str(word);
                    len += wlen;
                    if count == 0 {
                        start = record.start;
                    }
                    end = record.end;
                    count += 1;
                }
                if count > 0 {
                    push_line(
                        &mut lines,
                        &mut text,
                        LineSpec {
                            block_index,
                            paragraph_index,
                            page: block.page,
                            line_index: line_no,
                            start,
                            end,
                            line_height_hp: metric.line_height_hp,
                        },
                    )?;
                }
                paragraph_out.push(ReflowParagraph {
                    index: paragraph_index,
                    block_index,
                    page: block.page,
                    char_start: para_char_start,
                    char_end: para_char_end,
                    first_line: para_first_line,
                    line_count: lines.len() - para_first_line,
                });
            }
            laid_out.push(ReflowBlock {
                index: block_index,
                page: block.page,
                order: block.order,
                text: slice_canonical(&canonical, char_start, char_end),
                char_start,
                char_end,
                first_line,
                line_count: lines.len() - first_line,
            });
        }
        if lines.len() > MAX_REFLOW_LINES {
            return Err(ReflowError::TooManyLines {
                limit: MAX_REFLOW_LINES,
            });
        }

        // Distinct pages in reading order, each mapped to its first line.
        let mut page_starts: Vec<(usize, usize)> = Vec::new();
        for line in &lines {
            match page_starts.last_mut() {
                Some((page, _)) if *page == line.page => {}
                _ => page_starts.push((line.page, line_index_of(&lines, line))),
            }
        }

        let content_height_hp = lines
            .len()
            .checked_mul(metric.line_height_hp as usize)
            .and_then(|value| u32::try_from(value).ok())
            .unwrap_or(u32::MAX);

        let block_ranges = laid_out
            .iter()
            .map(|block| (block.char_start, block.char_end))
            .collect();
        Ok(Self {
            metric,
            blocks: laid_out,
            block_ranges,
            paragraphs: paragraph_out,
            lines,
            canonical,
            word_records,
            page_starts,
            content_height_hp,
        })
    }

    /// Font metric used for this layout.
    pub fn metric(&self) -> FontMetric {
        self.metric
    }

    /// Laid-out blocks in reading order.
    pub fn blocks(&self) -> &[ReflowBlock] {
        &self.blocks
    }

    /// All lines in reading order.
    pub fn lines(&self) -> &[ReflowLine] {
        &self.lines
    }

    /// Blank-line-delimited paragraphs in reading order.
    pub fn paragraphs(&self) -> &[ReflowParagraph] {
        &self.paragraphs
    }

    /// Canonical reading text (words joined by single spaces).
    pub fn text(&self) -> &str {
        &self.canonical
    }

    /// Total content height in hundredths of a point.
    pub fn content_height_hp(&self) -> u32 {
        self.content_height_hp
    }

    /// Distinct source pages in reading order.
    pub fn page_indices(&self) -> Vec<usize> {
        self.page_starts.iter().map(|(page, _)| *page).collect()
    }

    /// Number of distinct source pages in the layout.
    pub fn page_count(&self) -> usize {
        self.page_starts.len()
    }

    /// Index of the first line for `page`.
    pub fn first_line_for_page(&self, page: usize) -> Result<usize, ReflowError> {
        self.page_starts
            .iter()
            .find(|(candidate, _)| *candidate == page)
            .map(|(_, line)| *line)
            .ok_or(ReflowError::UnknownPage(page))
    }

    /// Source page containing character `offset`.
    pub fn page_at_offset(&self, offset: usize) -> Result<usize, ReflowError> {
        let block = self.block_at_offset(offset)?;
        Ok(self.blocks[block].page)
    }

    /// Index of the block containing character `offset`.
    pub fn block_at_offset(&self, offset: usize) -> Result<usize, ReflowError> {
        block_for_offset(&self.block_ranges, offset, self.canonical.chars().count())
    }

    /// The next page after `current`, if any.
    pub fn next_page(&self, current: usize) -> Option<usize> {
        let position = self
            .page_starts
            .iter()
            .position(|(page, _)| *page == current)?;
        self.page_starts.get(position + 1).map(|(page, _)| *page)
    }

    /// The previous page before `current`, if any.
    pub fn previous_page(&self, current: usize) -> Option<usize> {
        let position = self
            .page_starts
            .iter()
            .position(|(page, _)| *page == current)?;
        if position == 0 {
            None
        } else {
            self.page_starts.get(position - 1).map(|(page, _)| *page)
        }
    }

    /// Clamp a scroll top to the valid range for the given viewport height.
    ///
    /// Both arguments are hundredths of a point. A viewport taller than the
    /// content simply yields a scroll top of zero.
    pub fn clamp_scroll(&self, offset_hp: u32, viewport_hp: u32) -> u32 {
        let max = self.content_height_hp.saturating_sub(viewport_hp);
        offset_hp.min(max)
    }

    /// Build a read-aloud speech plan over this layout, reusing the shared
    /// chunking machinery. An empty layout cannot occur, so the plan is non-empty.
    pub fn read_aloud_plan(&self, mode: ChunkMode, voice: VoiceSettings) -> ReadAloudPlan {
        let spans: Vec<Vec<OwnedSpan>> = self
            .blocks
            .iter()
            .map(|block| vec![OwnedSpan::new(block.text.clone(), 0.0, 0.0, 0.0, 0.0)])
            .collect();
        let pages: Vec<(usize, &[OwnedSpan])> = self
            .blocks
            .iter()
            .zip(spans.iter())
            .map(|(block, spans)| (block.page, spans.as_slice()))
            .collect();
        ReadAloudPlan::from_pages(&pages, mode, voice)
    }

    /// Build a read-aloud cursor that maps offsets and chunks to blocks.
    pub fn read_aloud_cursor(&self) -> ReadAloudCursor {
        ReadAloudCursor::new(self)
    }
}

/// Identity and geometry of a line that is ready to be emitted.
struct LineSpec {
    block_index: usize,
    paragraph_index: usize,
    page: usize,
    line_index: usize,
    start: usize,
    end: usize,
    line_height_hp: u32,
}

/// Push a fully accumulated line and reset the text buffer for the next one.
fn push_line(
    lines: &mut Vec<ReflowLine>,
    text: &mut String,
    spec: LineSpec,
) -> Result<(), ReflowError> {
    if lines.len() >= MAX_REFLOW_LINES {
        return Err(ReflowError::TooManyLines {
            limit: MAX_REFLOW_LINES,
        });
    }
    let top_hp = (lines.len() * spec.line_height_hp as usize).min(u32::MAX as usize) as u32;
    let bottom_hp = top_hp.saturating_add(spec.line_height_hp);
    lines.push(ReflowLine {
        block_index: spec.block_index,
        paragraph_index: spec.paragraph_index,
        page: spec.page,
        line_index: spec.line_index,
        text: std::mem::take(text),
        start: spec.start,
        end: spec.end,
        top_hp,
        bottom_hp,
    });
    Ok(())
}

/// Index of a line within a slice, by identity of its position.
fn line_index_of(lines: &[ReflowLine], target: &ReflowLine) -> usize {
    lines
        .iter()
        .position(|line| std::ptr::eq(line, target))
        .unwrap_or(lines.len().saturating_sub(1))
}

/// Return the substring of `text` spanned by half-open *character* offsets.
fn slice_canonical(text: &str, start: usize, end: usize) -> String {
    if start >= end {
        return String::new();
    }
    text.chars().skip(start).take(end - start).collect()
}

/// Resolve a canonical character offset to a block index.
fn block_for_offset(
    ranges: &[(usize, usize)],
    offset: usize,
    canonical_len: usize,
) -> Result<usize, ReflowError> {
    if offset >= canonical_len {
        return Err(ReflowError::OffsetOutOfRange {
            offset,
            len: canonical_len,
        });
    }
    let mut last_before = None;
    for (index, &(start, end)) in ranges.iter().enumerate() {
        if offset < start {
            break;
        }
        if offset < end {
            return Ok(index);
        }
        last_before = Some(index);
    }
    last_before.ok_or(ReflowError::OffsetOutOfRange {
        offset,
        len: canonical_len,
    })
}

// ---------------------------------------------------------------------------
// Read-aloud cursor
// ---------------------------------------------------------------------------

/// One cursor entry: a speech chunk paired with its block and canonical span.
#[derive(Clone, Debug, PartialEq, Eq)]
struct CursorEntry {
    block_index: usize,
    start: usize,
    end: usize,
}

/// A read-aloud cursor that maps canonical text offsets to reflow blocks.
///
/// The cursor walks the word [`SpeechChunk`]s produced by the shared read-aloud
/// chunker. A host advances the cursor as words are spoken and asks for the
/// current block to highlight; it can also seek by character offset or block
/// index. All lookups are deterministic and return typed errors on bad input.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadAloudCursor {
    entries: Vec<CursorEntry>,
    chunks: Vec<SpeechChunk>,
    blocks: Vec<(usize, usize)>,
    canonical_len: usize,
    cursor: usize,
}

impl ReadAloudCursor {
    /// Build a cursor from a layout, reusing word-level read-aloud chunking.
    pub fn new(layout: &ReflowLayout) -> Self {
        let spans: Vec<Vec<OwnedSpan>> = layout
            .blocks
            .iter()
            .map(|block| vec![OwnedSpan::new(block.text.clone(), 0.0, 0.0, 0.0, 0.0)])
            .collect();
        let pages: Vec<(usize, &[OwnedSpan])> = layout
            .blocks
            .iter()
            .zip(spans.iter())
            .map(|(block, spans)| (block.page, spans.as_slice()))
            .collect();
        let chunks = build_chunks(&pages, ChunkMode::Word);
        let mut entries = Vec::new();
        for chunk in &chunks {
            if let Some(record) = layout.word_records.get(chunk.word_index) {
                entries.push(CursorEntry {
                    block_index: record.block_index,
                    start: record.start,
                    end: record.end,
                });
            }
        }
        let blocks = layout
            .blocks
            .iter()
            .map(|block| (block.char_start, block.char_end))
            .collect();
        Self {
            entries,
            chunks,
            blocks,
            canonical_len: layout.canonical.chars().count(),
            cursor: 0,
        }
    }

    /// Number of speech chunks in the cursor.
    pub fn len(&self) -> usize {
        self.chunks.len()
    }

    /// Whether the cursor has no chunks.
    pub fn is_empty(&self) -> bool {
        self.chunks.is_empty()
    }

    /// Index of the current chunk.
    pub fn cursor(&self) -> usize {
        self.cursor
    }

    /// The current speech chunk, if any.
    pub fn current_chunk(&self) -> Option<&SpeechChunk> {
        self.chunks.get(self.cursor)
    }

    /// Block index of the current chunk, if any.
    pub fn current_block(&self) -> Option<usize> {
        self.entries.get(self.cursor).map(|entry| entry.block_index)
    }

    /// Resolve a canonical character offset to a block index.
    pub fn block_at_offset(&self, offset: usize) -> Result<usize, ReflowError> {
        block_for_offset(&self.blocks, offset, self.canonical_len)
    }

    /// Move the cursor to the chunk containing canonical `offset`.
    pub fn seek(&mut self, offset: usize) -> Result<usize, ReflowError> {
        self.block_at_offset(offset)?;
        let position = self
            .entries
            .iter()
            .position(|entry| entry.start <= offset && offset < entry.end)
            .or_else(|| self.entries.iter().position(|entry| entry.start <= offset))
            .ok_or(ReflowError::OffsetOutOfRange {
                offset,
                len: self.canonical_len,
            })?;
        self.cursor = position;
        Ok(position)
    }

    /// Move the cursor to the first chunk of `block_index`.
    pub fn seek_to_block(&mut self, block_index: usize) -> Result<usize, ReflowError> {
        let position = self
            .entries
            .iter()
            .position(|entry| entry.block_index == block_index)
            .ok_or(ReflowError::UnknownBlock(block_index))?;
        self.cursor = position;
        Ok(position)
    }

    /// Advance one chunk. At the end, the cursor stays put.
    pub fn advance(&mut self) {
        if self.cursor + 1 < self.chunks.len() {
            self.cursor += 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn metric(width: u32) -> FontMetric {
        FontMetric::new(500, width, 1_200).expect("valid test metric")
    }

    fn blocks(pairs: &[(usize, usize, &str)]) -> Vec<TextBlock> {
        pairs
            .iter()
            .map(|(page, order, text)| TextBlock::new(*page, *order, *text))
            .collect()
    }

    #[test]
    fn layout_is_deterministic_and_order_independent() {
        let input = blocks(&[
            (0, 1, "Second block."),
            (1, 0, "Third page text."),
            (0, 0, "First block."),
        ]);
        let a = ReflowLayout::new(&input, metric(40_000)).expect("layout a");
        let reordered = blocks(&[
            (1, 0, "Third page text."),
            (0, 0, "First block."),
            (0, 1, "Second block."),
        ]);
        let b = ReflowLayout::new(&reordered, metric(40_000)).expect("layout b");
        assert_eq!(a, b);
        assert_eq!(a.text(), "First block. Second block. Third page text.");
        assert_eq!(a.page_indices(), vec![0, 1]);
    }

    #[test]
    fn width_boundaries_control_wrapping() {
        // 3 chars per line (1_500 / 500). Words are not split, so longer words
        // occupy a whole line.
        let input = blocks(&[(0, 0, "aa b cccc")]);
        let layout = ReflowLayout::new(&input, metric(1_500)).expect("layout");
        let texts: Vec<&str> = layout
            .lines()
            .iter()
            .map(|line| line.text.as_str())
            .collect();
        assert_eq!(texts, vec!["aa", "b", "cccc"]);
        let tops: Vec<u32> = layout.lines().iter().map(|line| line.top_hp).collect();
        assert_eq!(tops, vec![0, 1_200, 2_400]);
        assert_eq!(layout.content_height_hp(), 3_600);
    }

    #[test]
    fn a_single_char_width_puts_every_word_on_its_own_line() {
        let input = blocks(&[(0, 0, "one two")]);
        let layout = ReflowLayout::new(&input, metric(10)).expect("layout");
        assert_eq!(layout.metric().max_chars_per_line(), 1);
        assert_eq!(layout.lines().len(), 2);
    }

    #[test]
    fn empty_and_all_blank_inputs_are_typed_errors() {
        let empty: Vec<TextBlock> = Vec::new();
        assert_eq!(
            ReflowLayout::new(&empty, FontMetric::DEFAULT),
            Err(ReflowError::EmptyInput)
        );
        let blank = blocks(&[(0, 0, "  "), (1, 0, "\t\n")]);
        assert_eq!(
            ReflowLayout::new(&blank, FontMetric::DEFAULT),
            Err(ReflowError::EmptyInput)
        );
    }

    #[test]
    fn invalid_metrics_are_rejected() {
        assert_eq!(
            FontMetric::new(0, 100, 100),
            Err(ReflowError::InvalidMetric {
                field: "char_advance_hp"
            })
        );
        assert_eq!(
            FontMetric::new(100, 0, 100),
            Err(ReflowError::InvalidMetric {
                field: "column_width_hp"
            })
        );
        assert_eq!(
            FontMetric::new(100, 100, 0),
            Err(ReflowError::InvalidMetric {
                field: "line_height_hp"
            })
        );
    }

    #[test]
    fn read_aloud_cursor_maps_offsets_to_blocks() {
        let input = blocks(&[(0, 0, "alpha beta"), (0, 1, "gamma delta")]);
        let layout = ReflowLayout::new(&input, FontMetric::DEFAULT).expect("layout");
        assert_eq!(layout.text(), "alpha beta gamma delta");
        let mut cursor = layout.read_aloud_cursor();
        assert_eq!(cursor.len(), 4);
        assert_eq!(cursor.current_block(), Some(0));
        assert_eq!(cursor.block_at_offset(0), Ok(0)); // "alpha"
        assert_eq!(cursor.block_at_offset(6), Ok(0)); // "beta"
        assert_eq!(cursor.block_at_offset(11), Ok(1)); // "gamma"
        assert_eq!(cursor.block_at_offset(17), Ok(1)); // "delta"
        // Seeking into block 1 positions at the "gamma" chunk.
        assert_eq!(cursor.seek(12), Ok(2));
        assert_eq!(cursor.current_block(), Some(1));
        assert_eq!(cursor.seek_to_block(0), Ok(0));
        cursor.advance();
        assert_eq!(cursor.cursor(), 1);
        assert_eq!(
            cursor.current_chunk().map(|c| c.text.as_str()),
            Some("beta")
        );
    }

    #[test]
    fn cursor_rejects_out_of_range_offsets_and_unknown_blocks() {
        let input = blocks(&[(0, 0, "one two")]);
        let layout = ReflowLayout::new(&input, FontMetric::DEFAULT).expect("layout");
        let mut cursor = layout.read_aloud_cursor();
        let len = layout.text().chars().count();
        assert_eq!(
            cursor.block_at_offset(len),
            Err(ReflowError::OffsetOutOfRange { offset: len, len })
        );
        assert!(matches!(
            cursor.seek(len),
            Err(ReflowError::OffsetOutOfRange { .. })
        ));
        assert_eq!(cursor.seek_to_block(9), Err(ReflowError::UnknownBlock(9)));
    }

    #[test]
    fn scroll_position_is_clamped_to_content() {
        let input = blocks(&[(0, 0, "a b c d e f")]);
        let layout = ReflowLayout::new(&input, metric(500)).expect("layout"); // 1 char/line
        assert_eq!(layout.lines().len(), 6);
        assert_eq!(layout.content_height_hp(), 7_200);
        assert_eq!(layout.clamp_scroll(10_000, 2_400), 4_800); // content - viewport
        assert_eq!(layout.clamp_scroll(100, 2_400), 100);
        assert_eq!(layout.clamp_scroll(10_000, 20_000), 0); // viewport taller than content
    }

    #[test]
    fn page_navigation_and_offset_lookup_are_consistent() {
        let input = blocks(&[(2, 0, "first"), (4, 0, "second"), (7, 0, "third")]);
        let layout = ReflowLayout::new(&input, FontMetric::DEFAULT).expect("layout");
        assert_eq!(layout.page_indices(), vec![2, 4, 7]);
        assert_eq!(layout.first_line_for_page(2), Ok(0));
        assert_eq!(layout.first_line_for_page(4), Ok(1));
        assert_eq!(layout.first_line_for_page(7), Ok(2));
        assert_eq!(
            layout.first_line_for_page(3),
            Err(ReflowError::UnknownPage(3))
        );
        assert_eq!(layout.next_page(2), Some(4));
        assert_eq!(layout.next_page(7), None);
        assert_eq!(layout.previous_page(7), Some(4));
        assert_eq!(layout.previous_page(2), None);
        assert_eq!(layout.page_at_offset(0), Ok(2));
        assert_eq!(layout.page_at_offset(6), Ok(4));
        assert_eq!(layout.page_at_offset(13), Ok(7));
    }

    #[test]
    fn read_aloud_plan_reuses_shared_chunking() {
        let input = blocks(&[(0, 0, "Hello world. Bye now.")]);
        let layout = ReflowLayout::new(&input, FontMetric::DEFAULT).expect("layout");
        let plan = layout.read_aloud_plan(ChunkMode::Sentence, VoiceSettings::default());
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.chunks()[0].text, "Hello world.");
        assert_eq!(plan.chunks()[1].text, "Bye now.");
    }

    #[test]
    fn block_spans_and_line_offsets_cover_the_canonical_text() {
        let input = blocks(&[(0, 0, "alpha beta gamma")]);
        let layout = ReflowLayout::new(&input, metric(600)).expect("layout");
        let block = &layout.blocks()[0];
        assert_eq!(block.char_start, 0);
        assert_eq!(block.char_end, layout.text().chars().count());
        for line in layout.lines() {
            assert_eq!(
                slice_canonical(layout.text(), line.start, line.end),
                line.text
            );
        }
    }

    #[test]
    fn segment_paragraphs_splits_on_blank_lines_and_keeps_soft_breaks() {
        assert_eq!(segment_paragraphs("  \n\t"), Vec::<String>::new());
        assert_eq!(segment_paragraphs("one line"), vec!["one line"]);
        assert_eq!(
            segment_paragraphs("soft\nbreak\n\nsecond\n\n\nthird"),
            vec!["soft break", "second", "third"]
        );
    }

    #[test]
    fn paragraph_boundaries_and_line_indices_are_deterministic() {
        let input = blocks(&[(0, 0, "alpha beta\n\ngamma delta epsilon\n\nzeta")]);
        let layout = ReflowLayout::new(&input, metric(1_500)).expect("layout");
        assert_eq!(layout.text(), "alpha beta gamma delta epsilon zeta");
        let paragraphs = layout.paragraphs();
        assert_eq!(paragraphs.len(), 3);
        assert_eq!(paragraphs[0].char_start, 0);
        assert_eq!(paragraphs[0].char_end, 10); // "alpha beta"
        assert_eq!(paragraphs[1].char_start, 11);
        assert_eq!(paragraphs[1].char_end, 30); // "gamma delta epsilon"
        assert_eq!(paragraphs[2].char_start, 31);
        assert_eq!(paragraphs[2].char_end, 35); // "zeta"
        // Each paragraph's first line pointer and line count are contiguous.
        for (position, paragraph) in paragraphs.iter().enumerate() {
            assert_eq!(paragraph.index, position);
            assert_eq!(paragraph.block_index, 0);
            assert_eq!(paragraph.page, 0);
            assert_eq!(
                paragraph.line_count,
                layout.lines()[paragraph.first_line..]
                    .iter()
                    .take_while(|line| line.paragraph_index == position)
                    .count()
            );
            let slice = slice_canonical(layout.text(), paragraph.char_start, paragraph.char_end);
            assert!(!slice.is_empty());
        }
        // The same words in a different source layout produce the same spans.
        let reordered = blocks(&[(0, 0, "alpha beta\r\n\r\ngamma delta epsilon\r\n\r\nzeta")]);
        assert_eq!(
            layout,
            ReflowLayout::new(&reordered, metric(1_500)).expect("crlf")
        );
    }

    #[test]
    fn paragraph_boundaries_survive_block_reordering() {
        let first = blocks(&[(1, 0, "b one\n\nb two"), (0, 0, "a one\n\na two")]);
        let second = blocks(&[(0, 0, "a one\n\na two"), (1, 0, "b one\n\nb two")]);
        let a = ReflowLayout::new(&first, metric(2_000)).expect("a");
        let b = ReflowLayout::new(&second, metric(2_000)).expect("b");
        assert_eq!(a, b);
        let pages: Vec<usize> = a.paragraphs().iter().map(|p| p.page).collect();
        assert_eq!(pages, vec![0, 0, 1, 1]);
    }
}
