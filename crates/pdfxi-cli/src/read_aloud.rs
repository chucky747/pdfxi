//! `pdfxi read-aloud`: turn page text into a deterministic utterance queue.
//!
//! This is the speaking half of the accessibility pair. It performs **no OS
//! speech calls**: it builds the ordered utterances a speech backend would be
//! handed, each with a fixed-point millisecond start, duration and trailing
//! pause, derived from the speaking rate. `--seek` starts the queue at a given
//! utterance and rebases its clock, which is the deterministic equivalent of the
//! desktop shell's seek.
//!
//! The cadence is integer arithmetic in milliseconds:
//!
//! ```text
//! word_ms     = 60_000 / wpm
//! sentence_pause_ms = word_ms / 2
//! page_pause_ms     = word_ms * 2
//! ```
//!
//! A sentence's duration is `words * word_ms`; the last sentence of a non-final
//! page also carries the page pause. No floating point is involved, so the same
//! document always produces the same timings.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use serde::Serialize;

use crate::reflow::{selected_pages, split_sentences};

/// Slowest accepted speaking rate, in words per minute.
const MIN_WPM: u16 = 80;
/// Fastest accepted speaking rate, in words per minute.
const MAX_WPM: u16 = 400;
/// Default speaking rate, in words per minute.
const DEFAULT_WPM: u16 = 180;

/// One utterance in the JSON report.
#[derive(Serialize)]
struct UtteranceRow {
    /// Zero-based position in the full queue, even after `--seek`.
    index: usize,
    /// One-based page the sentence came from.
    page: usize,
    /// Sentence text handed to the speech backend.
    text: String,
    /// Whitespace-delimited word count used for the duration.
    words: usize,
    /// Cumulative start time in milliseconds after any seek.
    start_ms: u64,
    /// Time spent speaking the sentence.
    duration_ms: u32,
    /// Trailing pause before the next utterance.
    pause_ms: u32,
    /// Cumulative end time, including the pause.
    end_ms: u64,
}

/// JSON shape of `pdfxi read-aloud --json`.
#[derive(Serialize)]
struct ReadAloudReport {
    /// File that was inspected.
    file: String,
    /// Speaking rate used for the cadence.
    wpm: u16,
    /// Milliseconds spent speaking one word.
    word_ms: u32,
    /// Utterances skipped by `--seek`.
    seek: usize,
    /// End time of the last emitted utterance.
    total_duration_ms: u64,
    /// Ordered utterances.
    utterances: Vec<UtteranceRow>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Fixed-point cadence for a speaking rate.
///
/// Returns `(word_ms, sentence_pause_ms, page_pause_ms)`. The rate is clamped
/// before the division so a parsed zero cannot divide by zero.
fn cadence(wpm: u16) -> (u32, u32, u32) {
    let word_ms = 60_000 / u32::from(wpm.max(1));
    (word_ms, word_ms / 2, word_ms * 2)
}

/// Build the utterance queue for the selected pages.
fn build_utterances(pages: &[(usize, String)], wpm: u16) -> Vec<UtteranceRow> {
    let (word_ms, sentence_pause_ms, page_pause_ms) = cadence(wpm);
    let mut rows = Vec::new();
    let mut clock = 0_u64;
    let page_count = pages.len();
    for (position, (page, text)) in pages.iter().enumerate() {
        let sentences = split_sentences(text);
        let is_last_page = position + 1 == page_count;
        let sentence_count = sentences.len();
        for (sentence_index, sentence) in sentences.into_iter().enumerate() {
            let words = sentence.split_whitespace().count();
            let duration_ms = u32::try_from(words)
                .unwrap_or(u32::MAX)
                .saturating_mul(word_ms);
            let is_last_sentence = sentence_index + 1 == sentence_count;
            let pause_ms = if is_last_sentence && !is_last_page {
                sentence_pause_ms.saturating_add(page_pause_ms)
            } else {
                sentence_pause_ms
            };
            let start_ms = clock;
            let end_ms = start_ms + u64::from(duration_ms) + u64::from(pause_ms);
            rows.push(UtteranceRow {
                index: rows.len(),
                page: *page,
                text: sentence,
                words,
                start_ms,
                duration_ms,
                pause_ms,
                end_ms,
            });
            clock = end_ms;
        }
    }
    rows
}

/// Rebase a sought queue so playback starts at time zero.
fn rebase(rows: &mut [UtteranceRow]) {
    let mut clock = 0_u64;
    for row in rows.iter_mut() {
        row.start_ms = clock;
        row.end_ms = row.start_ms + u64::from(row.duration_ms) + u64::from(row.pause_ms);
        clock = row.end_ms;
    }
}

/// Print the utterance queue for the selected pages.
pub(crate) fn run_read_aloud(
    file: &Path,
    wpm: u16,
    page: Option<usize>,
    seek: usize,
    json: bool,
) -> anyhow::Result<()> {
    if !(MIN_WPM..=MAX_WPM).contains(&wpm) {
        bail!("--wpm must be between {MIN_WPM} and {MAX_WPM}");
    }
    let document = load(file)?;
    let pages = selected_pages(&document, file, page)?;

    let mut utterances = build_utterances(&pages, wpm);
    if seek > 0 {
        if seek >= utterances.len() {
            bail!(
                "--seek {seek} is past the end: the selection has {} utterance(s)",
                utterances.len()
            );
        }
        utterances.drain(..seek);
        rebase(&mut utterances);
    }
    let total_duration_ms = utterances.last().map_or(0, |row| row.end_ms);
    let (word_ms, _, _) = cadence(wpm);

    let report = ReadAloudReport {
        file: file.display().to_string(),
        wpm,
        word_ms,
        seek,
        total_duration_ms,
        utterances,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &report.utterances {
            println!(
                "utterance {}  page {}  {}-{} ms  {} words",
                row.index, row.page, row.start_ms, row.end_ms, row.words
            );
            println!("  {}", row.text);
        }
        println!(
            "total: {} utterance(s), {} ms at {} wpm",
            report.utterances.len(),
            report.total_duration_ms,
            report.wpm
        );
    }
    Ok(())
}

/// Default speaking rate used when `--wpm` is not given.
pub(crate) fn default_wpm() -> u16 {
    DEFAULT_WPM
}

#[cfg(test)]
mod tests {
    use super::*;

    fn page_text() -> Vec<(usize, String)> {
        vec![
            (
                1,
                "Lantern Slide•The lantern marker. Second sentence.".to_owned(),
            ),
            (2, "Harbor Slide•The harbor marker.".to_owned()),
        ]
    }

    #[test]
    fn cadence_is_integer_milliseconds_at_the_default_rate() {
        assert_eq!(cadence(180), (333, 166, 666));
        assert_eq!(cadence(60), (1000, 500, 2000));
        assert_eq!(cadence(0), (60_000, 30_000, 120_000), "zero is clamped");
    }

    #[test]
    fn utterances_are_ordered_with_monotonic_fixed_point_times() {
        let rows = build_utterances(&page_text(), 180);
        assert_eq!(rows.len(), 5);
        assert_eq!(rows[0].text, "Lantern Slide");
        assert_eq!(rows[1].text, "The lantern marker.");
        assert_eq!(rows[2].text, "Second sentence.");
        assert_eq!(rows[3].text, "Harbor Slide");
        assert_eq!(rows[4].text, "The harbor marker.");
        assert_eq!(rows[0].start_ms, 0);
        for pair in rows.windows(2) {
            assert_eq!(pair[0].end_ms, pair[1].start_ms, "no gaps or overlaps");
        }
        // The last sentence of page 1 carries the page pause.
        assert_eq!(rows[2].pause_ms, 166 + 666);
        assert_eq!(rows[4].pause_ms, 166);
    }

    #[test]
    fn seek_rebases_the_clock_and_keeps_original_indices() {
        let mut rows = build_utterances(&page_text(), 180);
        let full_total = rows.last().map(|row| row.end_ms).unwrap_or(0);
        rows.drain(..2);
        rebase(&mut rows);
        assert_eq!(rows[0].index, 2);
        assert_eq!(rows[0].start_ms, 0);
        assert!(rows.last().map(|row| row.end_ms).unwrap_or(0) < full_total);
    }
}
