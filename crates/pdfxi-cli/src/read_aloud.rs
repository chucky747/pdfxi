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

use std::io::Write;
use std::process::{Command, Stdio};

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
    /// Whether the system synthesizer was asked to speak.
    spoken: bool,
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

/// Everything the `read-aloud` command was asked to do.
///
/// A request carries the plan inputs plus the optional system-speech actions so
/// the entry point keeps a small, documented signature.
pub(crate) struct ReadAloudRequest<'a> {
    /// Source PDF; omitted only for `--voices`.
    pub(crate) file: Option<&'a Path>,
    /// Speaking rate in words per minute.
    pub(crate) wpm: u16,
    /// One-based page filter.
    pub(crate) page: Option<usize>,
    /// Zero-based utterance to start from.
    pub(crate) seek: usize,
    /// Installed voice name, or `default`.
    pub(crate) voice: &'a str,
    /// Speak the planned utterances through the system synthesizer.
    pub(crate) speak: bool,
    /// List installed system voices instead of planning a document.
    pub(crate) voices: bool,
    /// Emit JSON.
    pub(crate) json: bool,
}

/// Map a words-per-minute rate onto the `System.Speech` -10..=10 scale.
fn speech_rate(wpm: u16) -> i32 {
    ((i32::from(wpm) - 180) / 20).clamp(-10, 10)
}

/// Reject a voice name that cannot be embedded in a PowerShell single-quoted
/// literal. Names come from [`list_voices`]; this keeps a hand-typed value from
/// becoming script injection.
fn validate_voice_name(name: &str) -> anyhow::Result<()> {
    if name == "default" {
        return Ok(());
    }
    if name.trim().is_empty() {
        bail!("--voice must not be empty; use `default` for the system voice");
    }
    if name.chars().any(|ch| ch.is_control() || ch == '\'') {
        bail!("--voice {name:?} contains a control character or quote");
    }
    Ok(())
}

/// Run a PowerShell script and return its standard output.
fn run_powershell(script: &str, stdin: Option<&str>) -> anyhow::Result<String> {
    let mut child = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", script])
        .stdin(if stdin.is_some() {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .context("start powershell.exe")?;
    if let Some(text) = stdin {
        let mut pipe = child.stdin.take().context("open powershell stdin")?;
        pipe.write_all(text.as_bytes())
            .context("write powershell stdin")?;
    }
    let output = child.wait_with_output().context("wait for powershell")?;
    if !output.status.success() {
        bail!(
            "system speech failed: {}",
            String::from_utf8_lossy(&output.stderr).trim()
        );
    }
    Ok(String::from_utf8_lossy(&output.stdout).into_owned())
}

/// List the installed Windows speech voices.
pub(crate) fn list_voices(json: bool) -> anyhow::Result<()> {
    if !cfg!(windows) {
        bail!("system speech is only available on Windows");
    }
    let output = run_powershell(
        "Add-Type -AssemblyName System.Speech; \
         (New-Object System.Speech.Synthesis.SpeechSynthesizer).GetInstalledVoices() | \
         ForEach-Object { $_.VoiceInfo.Name }",
        None,
    )?;
    let voices: Vec<String> = output
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect();
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "voices": voices }))?
        );
        return Ok(());
    }
    for voice in &voices {
        println!("{voice}");
    }
    println!("voices: {}", voices.len());
    Ok(())
}

/// Speak every planned utterance through the Windows system synthesizer.
fn speak(utterances: &[UtteranceRow], wpm: u16, voice: &str) -> anyhow::Result<()> {
    if !cfg!(windows) {
        bail!("system speech is only available on Windows");
    }
    validate_voice_name(voice)?;
    let text = utterances
        .iter()
        .map(|row| row.text.as_str())
        .collect::<Vec<&str>>()
        .join(" ");
    let select = if voice == "default" {
        String::new()
    } else {
        format!("$s.SelectVoice('{voice}'); ")
    };
    let script = format!(
        "Add-Type -AssemblyName System.Speech; \
         $s = New-Object System.Speech.Synthesis.SpeechSynthesizer; \
         $s.Rate = {rate}; {select}\
         $text = [Console]::In.ReadToEnd(); \
         $s.Speak($text);",
        rate = speech_rate(wpm)
    );
    run_powershell(&script, Some(&text))?;
    Ok(())
}

/// Print the utterance queue for the selected pages.
pub(crate) fn run_read_aloud(request: &ReadAloudRequest<'_>) -> anyhow::Result<()> {
    if request.voices {
        return list_voices(request.json);
    }
    let file = request.file.context("pass a PDF file or --voices")?;
    let ReadAloudRequest {
        wpm,
        page,
        seek,
        voice,
        speak: speak_text,
        json,
        ..
    } = *request;
    validate_voice_name(voice)?;
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
        spoken: speak_text,
        utterances,
    };

    if speak_text {
        speak(&report.utterances, wpm, voice)?;
    }

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
        if report.spoken {
            println!("spoken: yes (system speech)");
        }
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
    fn speech_rate_maps_wpm_onto_the_system_scale() {
        assert_eq!(speech_rate(180), 0);
        assert_eq!(speech_rate(400), 10, "fast rates saturate at +10");
        assert_eq!(speech_rate(80), -5);
        assert_eq!(speech_rate(0), -9);
    }

    #[test]
    fn voice_names_reject_script_injection() {
        assert!(validate_voice_name("default").is_ok());
        assert!(validate_voice_name("Microsoft Zira Desktop").is_ok());
        assert!(validate_voice_name("").is_err());
        assert!(validate_voice_name("   ").is_err());
        assert!(validate_voice_name("Bad'Voice").is_err());
        assert!(validate_voice_name("Line\nBreak").is_err());
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
