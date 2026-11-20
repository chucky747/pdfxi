//! Deterministic, offline benchmark harness for the PDFXI performance budgets.
//!
//! This module implements `cargo xtask bench`. It generates small and large PDFs
//! entirely in memory, measures a fixed set of operations with a warmup phase and
//! repeated runs, compares the medians against explicit budgets derived from
//! `docs/TESTING.md`, and optionally checks a committed baseline JSON for a
//! greater-than-10% regression.
//!
//! # Design constraints
//!
//! - **No new dependencies.** The harness uses only `std` plus `serde`/
//!   `serde_json`, which `xtask` already depends on. Adding a dependency to
//!   `xtask` would modify the integrator-owned `Cargo.lock`, so the harness owns
//!   its (small) fixture builder and operation implementations instead of
//!   linking `pdfxi-core`.
//! - **No network, GUI, sleeps, randomness, or secrets.** Timings are the only
//!   variable part of the output; the JSON *shape* and all non-timing values are
//!   deterministic.
//! - **Mock renderer.** First-page "render" decodes the first content stream and
//!   fills a fixed-size RGB buffer. No PDFium binding is linked. The presence of
//!   `PDFIUM_PATH` is recorded in the report for host context only.
//!
//! The operations are structural proxies: they exercise the same byte scanning,
//! xref handling, text decoding, tokenisation and buffer allocation that the real
//! parsers perform, but they are not a substitute for a Criterion/profile run of
//! the shipping crates. Host caveats and variance handling are documented in
//! `docs/TESTING.md`.

use std::collections::BTreeMap;
use std::env;
use std::fmt::Write as FmtWrite;
use std::fs;
use std::path::PathBuf;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Schema version of the emitted JSON. Bump only when the field layout changes.
pub(crate) const SCHEMA_VERSION: u32 = 1;
/// Harness identifier written into every report.
pub(crate) const HARNESS: &str = "pdfxi-xtask-bench";
/// Page count at which the `docs/TESTING.md` budgets are defined.
const REFERENCE_PAGES: usize = 500;
/// Minimum per-operation budget in milliseconds (fixed overhead allowance).
const MIN_BUDGET_MS: f64 = 5.0;
/// Extra budget headroom for host variance (25%).
const BUDGET_TOLERANCE: f64 = 0.25;
/// Maximum allowed relative regression against the baseline (10%).
pub(crate) const REGRESSION_TOLERANCE: f64 = 0.10;
/// Absolute noise floor: metrics below this are not tracked for regression.
const NOISE_FLOOR_MS: f64 = 1.0;
/// Number of unmeasured warmup iterations per operation.
const WARMUP_RUNS: usize = 2;
/// Number of measured iterations per operation.
const MEASURED_RUNS: usize = 7;
/// Number of independent measurement rounds; the best value per metric is kept.
const MEASUREMENT_ROUNDS: usize = 3;
/// Memory budget multiplier over the fixture size (`< 1.5x file + 512 MB`).
const MEMORY_MULTIPLIER: f64 = 1.5;
/// Flat memory allowance in bytes (512 MiB) from `docs/TESTING.md`.
const MEMORY_ALLOWANCE_BYTES: f64 = 512.0 * 1024.0 * 1024.0;
/// Maximum mock-render width in pixels (keeps the harness fast and bounded).
const MOCK_WIDTH_MAX: usize = 320;
/// Maximum mock-render height in pixels.
const MOCK_HEIGHT_MAX: usize = 240;

/// Stable operation names, in report order.
const OPERATION_NAMES: [&str; 5] = [
    "open",
    "first_page_render",
    "text_extract",
    "search_index_build",
    "save",
];

/// Description of one generated fixture.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct FixtureSpec {
    /// Stable report name.
    pub(crate) name: &'static str,
    /// Number of pages to generate.
    pub(crate) pages: usize,
    /// Width of each page's text marker in bytes.
    pub(crate) text_chars: usize,
}

/// Return the three controlled-size fixtures: small, medium and large.
pub(crate) fn fixtures() -> [FixtureSpec; 3] {
    [
        FixtureSpec {
            name: "small",
            pages: 1,
            text_chars: 24,
        },
        FixtureSpec {
            name: "medium",
            pages: 100,
            text_chars: 24,
        },
        FixtureSpec {
            name: "large",
            pages: 500,
            text_chars: 48,
        },
    ]
}

/// Deterministic timing summary, all values in milliseconds.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct Stats {
    /// Fastest measured run.
    pub(crate) min_ms: f64,
    /// Median measured run (the tracked value).
    pub(crate) median_ms: f64,
    /// Slowest measured run.
    pub(crate) max_ms: f64,
    /// Number of measured samples.
    pub(crate) samples: usize,
}

/// One tracked operation metric for one fixture.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Metric {
    /// Stable operation name.
    pub(crate) name: String,
    /// Unit of `value` and `budget`: `ms` or `bytes`.
    pub(crate) unit: String,
    /// Median measured value (the tracked number).
    pub(crate) value: f64,
    /// Allowed upper bound including tolerance.
    pub(crate) budget: f64,
    /// Whether `value <= budget`.
    pub(crate) within_budget: bool,
    /// Fastest observed value.
    pub(crate) min: f64,
    /// Slowest observed value.
    pub(crate) max: f64,
    /// Number of measured samples.
    pub(crate) samples: usize,
}

/// All measured metrics and fixture metadata for one generated PDF.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct FixtureReport {
    /// Fixture name from [`FixtureSpec::name`].
    pub(crate) name: String,
    /// Page count of the generated PDF.
    pub(crate) pages: usize,
    /// Byte length of the generated PDF.
    pub(crate) bytes: usize,
    /// Operations in the fixed [`OPERATION_NAMES`] order, followed by
    /// `peak_allocation_estimate`.
    pub(crate) operations: Vec<Metric>,
}

/// Host metadata. Only `os`, `arch` and PDFium availability are recorded; no
/// machine-identifying or secret data is emitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct HostInfo {
    /// `std::env::consts::OS`.
    pub(crate) os: String,
    /// `std::env::consts::ARCH`.
    pub(crate) arch: String,
    /// Whether `PDFIUM_PATH` is set (context only; never linked).
    pub(crate) pdfium_available: bool,
    /// Explanation of the mock renderer.
    pub(crate) renderer_note: String,
}

/// Run configuration recorded in every report so a reader can reproduce it.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub(crate) struct ConfigReport {
    /// Unmeasured warmup iterations per operation.
    pub(crate) warmup_runs: usize,
    /// Measured iterations per operation.
    pub(crate) measured_runs: usize,
    /// Budget headroom fraction.
    pub(crate) budget_tolerance: f64,
    /// Regression threshold fraction.
    pub(crate) regression_tolerance: f64,
    /// Absolute noise floor in milliseconds.
    pub(crate) noise_floor_ms: f64,
    /// Independent measurement rounds.
    pub(crate) measurement_rounds: usize,
}

/// Overall run status.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct Summary {
    /// Total number of recorded metrics.
    pub(crate) operation_count: usize,
    /// Number of metrics above budget.
    pub(crate) budget_breaches: usize,
    /// Number of metrics regressed over the baseline threshold.
    pub(crate) regressions: usize,
    /// `present`, `missing`, or `invalid`.
    pub(crate) baseline_status: String,
    /// `pass` or `fail` (before `--report` overrides the exit code).
    pub(crate) status: String,
}

/// Complete, serialisable benchmark report. This is also the baseline format.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub(crate) struct BenchReport {
    /// JSON schema version.
    pub(crate) schema_version: u32,
    /// Harness identifier.
    pub(crate) harness: String,
    /// Run configuration.
    pub(crate) config: ConfigReport,
    /// Host context.
    pub(crate) host: HostInfo,
    /// Per-fixture results.
    pub(crate) fixtures: Vec<FixtureReport>,
    /// Aggregate status.
    pub(crate) summary: Summary,
}

/// Why a baseline file could not be used.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum BaselineError {
    /// The file is not valid JSON.
    Json,
    /// The JSON is valid but not a report of the expected schema version.
    Schema,
}

impl BaselineError {
    /// Short, stable label used in the report and error messages.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Schema => "schema",
        }
    }
}

/// A single baseline regression detail.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Regression {
    /// Fixture name.
    pub(crate) fixture: String,
    /// Operation name.
    pub(crate) operation: String,
    /// Baseline median in milliseconds.
    pub(crate) baseline_ms: f64,
    /// Current median in milliseconds.
    pub(crate) current_ms: f64,
    /// Relative regression as a fraction (for example `0.12` = 12%).
    pub(crate) ratio: f64,
}

/// Return the repository root without panicking when the path cannot be read.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .map(std::path::Path::to_path_buf)
        .unwrap_or_default()
}

/// Absolute path of the committed baseline JSON.
pub(crate) fn baseline_path() -> PathBuf {
    workspace_root().join("docs/evidence/qa/bench-baseline.json")
}

/// Absolute path of the informational latest-report JSON (generated, ignored).
pub(crate) fn latest_report_path() -> PathBuf {
    workspace_root().join("target/perf/bench-report.json")
}

/// Deterministic marker text for page `index` of a `pages`-page fixture.
///
/// The prefix is fixed-width and ASCII-only; the remainder is filled with a
/// deterministic `a`..`z` cycle so `text_chars` controls the fixture byte size.
fn page_marker(index: usize, pages: usize, text_chars: usize) -> String {
    let prefix = format!("PDFXI-BENCH-P{index:04}-OF-{pages:04}");
    let mut text = prefix;
    let mut position = 0usize;
    while text.len() < text_chars {
        let byte = b'a' + (position % 26) as u8;
        text.push(byte as char);
        position += 1;
    }
    text
}

/// Generate a structurally valid, byte-deterministic PDF with `pages` pages.
///
/// The layout mirrors `pdfxi-testkit`'s fixture builder: a catalog, a page tree,
/// two objects per page, a shared font, a classic xref table and a trailer. Xref
/// offsets and stream lengths are computed from the actual emitted bytes.
pub(crate) fn generate_pdf(pages: usize, text_chars: usize) -> Vec<u8> {
    let kids: String = (0..pages)
        .map(|index| format!("{} 0 R ", 3 + 2 * index))
        .collect();
    let mut objects: Vec<String> = Vec::with_capacity(3 + 2 * pages);
    objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
    objects.push(format!(
        "<< /Type /Pages /Kids [{}] /Count {} >>",
        kids.trim_end(),
        pages
    ));
    for index in 0..pages {
        let width = if index % 2 == 0 { 612 } else { 595 };
        let height = if index % 2 == 0 { 792 } else { 842 };
        objects.push(format!(
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
             /Contents {} 0 R >>",
            4 + 2 * index
        ));
        let marker = page_marker(index, pages, text_chars);
        let content = format!("BT /F1 24 Tf 72 720 Td ({marker}) Tj ET");
        objects.push(format!(
            "<< /Length {} >>\nstream\n{content}\nendstream",
            content.len()
        ));
    }
    objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());

    let mut out = String::from("%PDF-1.7\n%PDFXI\n");
    let mut offsets: Vec<usize> = Vec::with_capacity(objects.len());
    for (position, body) in objects.iter().enumerate() {
        offsets.push(out.len());
        let _ = write!(out, "{} 0 obj\n{body}\nendobj\n", position + 1);
    }
    let xref = out.len();
    let _ = write!(out, "xref\n0 {}\n", objects.len() + 1);
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        let _ = writeln!(out, "{offset:010} 00000 n ");
    }
    let _ = write!(
        out,
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
        objects.len() + 1
    );
    out.into_bytes()
}

/// Return the first byte index of `needle` in `haystack`, or `None`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Return every `stream\n` payload range in the PDF bytes.
fn content_streams(bytes: &[u8]) -> Vec<(usize, usize)> {
    let opener = b"stream\n";
    let closer = b"\nendstream";
    let mut ranges = Vec::new();
    let mut cursor = 0usize;
    while let Some(relative) = find(&bytes[cursor..], opener) {
        let start = cursor + relative + opener.len();
        let Some(end_relative) = find(&bytes[start..], closer) else {
            break;
        };
        ranges.push((start, start + end_relative));
        cursor = start + end_relative + closer.len();
    }
    ranges
}

/// Return the ASCII digits immediately after `marker` in `bytes`, if any.
fn digits_after(bytes: &[u8], marker: &[u8]) -> Option<usize> {
    let at = find(bytes, marker)? + marker.len();
    let tail = bytes.get(at..)?;
    let count = tail.iter().take_while(|byte| byte.is_ascii_digit()).count();
    if count == 0 {
        return None;
    }
    let text = std::str::from_utf8(tail.get(..count)?).ok()?;
    text.parse::<usize>().ok()
}

/// Operation `open`: validate the header and xref, then resolve the page count.
///
/// Returns the page count as a correctness checksum so the compiler cannot drop
/// the work. `Err` describes the first structural defect found.
fn op_open(bytes: &[u8]) -> Result<usize, String> {
    if !bytes.starts_with(b"%PDF-1.7") {
        return Err("missing PDF header".to_owned());
    }
    let offset =
        digits_after(bytes, b"startxref\n").ok_or_else(|| "missing startxref".to_owned())?;
    let end = offset
        .checked_add(4)
        .ok_or_else(|| "bad startxref".to_owned())?;
    if bytes.get(offset..end) != Some(b"xref") {
        return Err("startxref does not point at xref".to_owned());
    }
    digits_after(bytes, b"/Count ").ok_or_else(|| "missing page-tree count".to_owned())
}

/// Read the first `/MediaBox [0 0 W H]` from the PDF, clamped for the mock.
fn first_media_box(bytes: &[u8]) -> (usize, usize) {
    let mut width = MOCK_WIDTH_MAX as f64;
    let mut height = MOCK_HEIGHT_MAX as f64;
    if let Some(start) = find(bytes, b"/MediaBox [0 0 ") {
        let tail = &bytes[start + b"/MediaBox [0 0 ".len()..];
        let parts: Vec<&[u8]> = tail
            .split(|byte| *byte == b' ' || *byte == b']')
            .filter(|part| !part.is_empty())
            .take(2)
            .collect();
        if let Some(parsed) = parts
            .first()
            .and_then(|part| std::str::from_utf8(part).ok())
            .and_then(|value| value.parse::<f64>().ok())
        {
            width = parsed;
        }
        if let Some(parsed) = parts
            .get(1)
            .and_then(|part| std::str::from_utf8(part).ok())
            .and_then(|value| value.parse::<f64>().ok())
        {
            height = parsed;
        }
    }
    let width = width.min(MOCK_WIDTH_MAX as f64).max(1.0) as usize;
    let height = height.min(MOCK_HEIGHT_MAX as f64).max(1.0) as usize;
    (width, height)
}

/// Operation `first_page_render`: decode the first content stream and fill a
/// bounded RGB buffer with deterministic values. Returns the buffer length.
fn op_first_page_render(bytes: &[u8]) -> Result<usize, String> {
    let streams = content_streams(bytes);
    let (start, end) = *streams
        .first()
        .ok_or_else(|| "no content stream".to_owned())?;
    let content = bytes
        .get(start..end)
        .ok_or_else(|| "content range out of bounds".to_owned())?;
    let (width, height) = first_media_box(bytes);
    let mut seed = 0x811c_9dc5u32;
    for byte in content {
        seed = (seed ^ u32::from(*byte)).wrapping_mul(0x0100_0193);
    }
    let mut buffer = vec![0u8; width * height * 3];
    for pixel in buffer.as_chunks_mut::<3>().0 {
        seed = seed.wrapping_mul(1_664_525).wrapping_add(1_013_904_223);
        pixel[0] = (seed >> 16) as u8;
        pixel[1] = (seed >> 8) as u8;
        pixel[2] = seed as u8;
    }
    Ok(buffer.len())
}

/// Extract the text shown by all content streams in `bytes`.
fn extracted_text(bytes: &[u8]) -> String {
    let mut out = String::new();
    for (start, end) in content_streams(bytes) {
        let Some(stream) = bytes.get(start..end) else {
            continue;
        };
        let mut cursor = 0usize;
        while let Some(relative) = find(&stream[cursor..], b"(") {
            let open = cursor + relative + 1;
            let mut index = open;
            let mut escaped = false;
            while index < stream.len() {
                let byte = stream[index];
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b')' {
                    break;
                }
                index += 1;
            }
            if let Some(literal) = stream.get(open..index) {
                let _ = write!(out, "{} ", String::from_utf8_lossy(literal));
            }
            cursor = index.saturating_add(1);
        }
    }
    out
}

/// Operation `text_extract`: decode every page's drawn text. Returns its length.
fn op_text_extract(bytes: &[u8]) -> Result<usize, String> {
    if content_streams(bytes).is_empty() {
        return Err("no content stream".to_owned());
    }
    Ok(extracted_text(bytes).len())
}

/// Operation `search_index_build`: build a deterministic inverted index over all
/// extracted page text. Returns the number of distinct terms.
fn op_search_index(bytes: &[u8]) -> Result<usize, String> {
    let text = extracted_text(bytes);
    if text.is_empty() {
        return Err("no text to index".to_owned());
    }
    let mut index: BTreeMap<String, usize> = BTreeMap::new();
    for (page, line) in text.lines().enumerate() {
        for word in line.split_whitespace() {
            let key: String = word
                .chars()
                .filter(|ch| ch.is_ascii_alphanumeric())
                .flat_map(char::to_lowercase)
                .collect();
            if !key.is_empty() {
                let entry = index.entry(key).or_insert(0usize);
                *entry = (*entry).max(page + 1);
            }
        }
    }
    Ok(index.len())
}

/// Operation `save`: clone the source and append a deterministic incremental
/// revision with a regenerated xref section. Returns the output byte length.
fn op_save(bytes: &[u8]) -> Result<usize, String> {
    let previous =
        digits_after(bytes, b"startxref\n").ok_or_else(|| "missing startxref".to_owned())?;
    let size = digits_after(bytes, b"/Size ").ok_or_else(|| "missing trailer size".to_owned())?;
    let mut out = bytes.to_vec();
    out.push(b'\n');
    let object_offset = out.len();
    out.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
    let xref_offset = out.len();
    let mut section = String::new();
    let _ = write!(section, "xref\n1 1\n{object_offset:010} 00000 n \n");
    let _ = write!(
        section,
        "trailer\n<< /Size {size} /Root 1 0 R /Prev {previous} >>\nstartxref\n{xref_offset}\n%%EOF\n"
    );
    out.extend_from_slice(section.as_bytes());
    Ok(out.len())
}

/// Run `op` `warmup` times unmeasured, then `measured` times with timing.
///
/// Only the measured samples are returned. The counter-based tests in this
/// module verify that warmup calls never enter the sample set.
fn warmup_then_measure<F>(warmup: usize, measured: usize, mut op: F) -> Vec<Duration>
where
    F: FnMut() -> Result<usize, String>,
{
    for _ in 0..warmup {
        let _ = op();
    }
    let mut samples = Vec::with_capacity(measured);
    for _ in 0..measured {
        let started = Instant::now();
        let _ = op();
        samples.push(started.elapsed());
    }
    samples
}

/// Convert a duration to milliseconds.
fn as_ms(value: Duration) -> f64 {
    value.as_secs_f64() * 1_000.0
}

/// Summarise timing samples. `total_cmp` avoids `partial_cmp` on NaN.
fn stats(samples: &[Duration]) -> Option<Stats> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted: Vec<f64> = samples.iter().copied().map(as_ms).collect();
    sorted.sort_by(f64::total_cmp);
    let middle = (sorted.len() - 1) / 2;
    Some(Stats {
        min_ms: sorted[0],
        median_ms: sorted[middle],
        max_ms: sorted[sorted.len() - 1],
        samples: sorted.len(),
    })
}

/// Budget in milliseconds for an operation at a given page count.
///
/// Budgets come from the 500-page reference values in `docs/TESTING.md`, scale
/// linearly with page count, have a fixed minimum for tiny fixtures, then receive
/// a documented tolerance allowance for host variance.
pub(crate) fn budget_ms(base_at_reference: f64, pages: usize) -> f64 {
    let scaled = base_at_reference * (pages as f64) / (REFERENCE_PAGES as f64);
    scaled.max(MIN_BUDGET_MS) * (1.0 + BUDGET_TOLERANCE)
}

/// Base budget at the 500-page reference point, keyed by stable operation name.
fn base_budget_ms(name: &str) -> Option<f64> {
    match name {
        // "Open 50 MB / 500 pages < 1.5 s".
        "open" => Some(1_500.0),
        // "1080p page render < 150 ms" (the mock renders at reduced resolution).
        "first_page_render" => Some(150.0),
        // "Search 500 pages < 1 s" (text decode is the dominant part).
        "text_extract" | "search_index_build" => Some(1_000.0),
        // "Incremental annotation save < 2 s".
        "save" => Some(2_000.0),
        _ => None,
    }
}

/// Return `true` when a measured value is within its budget.
pub(crate) fn within_budget(value: f64, budget: f64) -> bool {
    value <= budget
}

/// Build one metric from measured samples and its budget.
fn metric(name: &str, samples: &[Duration], budget: f64) -> Option<Metric> {
    let summary = stats(samples)?;
    Some(Metric {
        name: name.to_owned(),
        unit: "ms".to_owned(),
        value: summary.median_ms,
        budget,
        within_budget: within_budget(summary.median_ms, budget),
        min: summary.min_ms,
        max: summary.max_ms,
        samples: summary.samples,
    })
}

/// Build the peak-allocation estimate metric (bytes).
fn allocation_metric(bytes: &[u8]) -> Metric {
    // The dominant live buffers are the source bytes and the save output; the
    // latter is measured once here rather than timed.
    let saved = op_save(bytes).unwrap_or(bytes.len());
    let estimate = bytes.len().saturating_add(saved);
    let budget = MEMORY_MULTIPLIER * (bytes.len() as f64) + MEMORY_ALLOWANCE_BYTES;
    Metric {
        name: "peak_allocation_estimate".to_owned(),
        unit: "bytes".to_owned(),
        value: estimate as f64,
        budget,
        within_budget: within_budget(estimate as f64, budget),
        min: estimate as f64,
        max: estimate as f64,
        samples: 1,
    }
}

/// Return host metadata for the report.
fn host_info() -> HostInfo {
    HostInfo {
        os: env::consts::OS.to_owned(),
        arch: env::consts::ARCH.to_owned(),
        pdfium_available: env::var_os("PDFIUM_PATH").is_some(),
        renderer_note: "mock renderer: content decode + bounded RGB fill; no PDFium link"
            .to_owned(),
    }
}

/// Run every fixture and operation, returning a complete report.
pub(crate) fn run_benchmarks() -> Result<BenchReport, String> {
    let config = ConfigReport {
        warmup_runs: WARMUP_RUNS,
        measured_runs: MEASURED_RUNS,
        budget_tolerance: BUDGET_TOLERANCE,
        regression_tolerance: REGRESSION_TOLERANCE,
        noise_floor_ms: NOISE_FLOOR_MS,
        measurement_rounds: MEASUREMENT_ROUNDS,
    };
    let mut reports = Vec::with_capacity(3);
    for fixture in fixtures() {
        let bytes = generate_pdf(fixture.pages, fixture.text_chars);
        // Verify each operation once before timing; a broken operation must fail
        // the harness rather than produce meaningless numbers.
        for name in OPERATION_NAMES {
            let result = match name {
                "open" => op_open(&bytes),
                "first_page_render" => op_first_page_render(&bytes),
                "text_extract" => op_text_extract(&bytes),
                "search_index_build" => op_search_index(&bytes),
                "save" => op_save(&bytes),
                _ => Err("unknown operation".to_owned()),
            };
            if result.is_err() {
                return Err(format!("fixture {} operation {name} failed", fixture.name));
            }
        }
        let mut operations = Vec::with_capacity(OPERATION_NAMES.len() + 1);
        for name in OPERATION_NAMES {
            let samples = warmup_then_measure(WARMUP_RUNS, MEASURED_RUNS, || match name {
                "open" => op_open(&bytes),
                "first_page_render" => op_first_page_render(&bytes),
                "text_extract" => op_text_extract(&bytes),
                "search_index_build" => op_search_index(&bytes),
                "save" => op_save(&bytes),
                _ => Ok(0),
            });
            let base = base_budget_ms(name).unwrap_or(MIN_BUDGET_MS);
            let budget = budget_ms(base, fixture.pages);
            let measured = metric(name, &samples, budget)
                .ok_or_else(|| format!("no samples for {} / {name}", fixture.name))?;
            operations.push(measured);
        }
        operations.push(allocation_metric(&bytes));
        reports.push(FixtureReport {
            name: fixture.name.to_owned(),
            pages: fixture.pages,
            bytes: bytes.len(),
            operations,
        });
    }
    let operation_count = reports.iter().map(|report| report.operations.len()).sum();
    let budget_breaches = reports
        .iter()
        .flat_map(|report| report.operations.iter())
        .filter(|metric| !metric.within_budget)
        .count();
    let mut report = BenchReport {
        schema_version: SCHEMA_VERSION,
        harness: HARNESS.to_owned(),
        config,
        host: host_info(),
        fixtures: reports,
        summary: Summary {
            operation_count,
            budget_breaches,
            regressions: 0,
            baseline_status: "missing".to_owned(),
            status: if budget_breaches == 0 {
                "pass".to_owned()
            } else {
                "fail".to_owned()
            },
        },
    };
    normalise(&mut report);
    Ok(report)
}

/// Merge `candidate` into `best`, keeping the lower median for every metric.
///
/// Best-of-N is robust to the transient scheduler and load spikes common on a
/// shared host: a real regression persists in every round, a spike does not.
fn merge_best(mut best: BenchReport, candidate: BenchReport) -> BenchReport {
    for fixture in &mut best.fixtures {
        let Some(other_fixture) = candidate
            .fixtures
            .iter()
            .find(|entry| entry.name == fixture.name)
        else {
            continue;
        };
        for metric in &mut fixture.operations {
            let Some(other) = other_fixture
                .operations
                .iter()
                .find(|entry| entry.name == metric.name && entry.unit == metric.unit)
            else {
                continue;
            };
            if other.value < metric.value {
                *metric = other.clone();
            }
        }
    }
    best
}

/// Run the harness [`MEASUREMENT_ROUNDS`] times and keep the best of each metric.
pub(crate) fn measure_best_of() -> Result<BenchReport, String> {
    let mut best = run_benchmarks()?;
    for _ in 1..MEASUREMENT_ROUNDS {
        best = merge_best(best, run_benchmarks()?);
    }
    best.summary.operation_count = best
        .fixtures
        .iter()
        .map(|entry| entry.operations.len())
        .sum();
    best.summary.budget_breaches = best
        .fixtures
        .iter()
        .flat_map(|entry| entry.operations.iter())
        .filter(|metric| !metric.within_budget)
        .count();
    best.summary.status = if best.summary.budget_breaches == 0 {
        "pass".to_owned()
    } else {
        "fail".to_owned()
    };
    normalise(&mut best);
    Ok(best)
}
/// Round timing values to a fixed precision so JSON formatting stays stable.
pub(crate) fn normalise(report: &mut BenchReport) {
    for fixture in &mut report.fixtures {
        for metric in &mut fixture.operations {
            if metric.unit == "ms" {
                metric.value = round4(metric.value);
                metric.min = round4(metric.min);
                metric.max = round4(metric.max);
                metric.budget = round4(metric.budget);
            }
        }
    }
}

/// Round to four decimal places.
fn round4(value: f64) -> f64 {
    (value * 10_000.0).round() / 10_000.0
}

/// Parse and validate a baseline JSON document.
pub(crate) fn parse_baseline(text: &str) -> Result<BenchReport, BaselineError> {
    let report: BenchReport = serde_json::from_str(text).map_err(|_| BaselineError::Json)?;
    if report.schema_version != SCHEMA_VERSION || report.harness != HARNESS {
        return Err(BaselineError::Schema);
    }
    Ok(report)
}

/// Load the committed baseline, distinguishing missing from invalid files.
pub(crate) fn load_baseline() -> Result<Option<BenchReport>, BaselineError> {
    let path = baseline_path();
    let text = match fs::read_to_string(&path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(_) => return Err(BaselineError::Json),
    };
    parse_baseline(&text).map(Some)
}

/// Find a baseline metric by fixture and operation name.
fn find_baseline_metric<'a>(
    baseline: &'a BenchReport,
    fixture: &str,
    operation: &str,
) -> Option<&'a Metric> {
    baseline
        .fixtures
        .iter()
        .find(|entry| entry.name == fixture)?
        .operations
        .iter()
        .find(|metric| metric.name == operation && metric.unit == "ms")
}

/// Compare current timings with the baseline, returning tracked regressions.
///
/// A metric is tracked only when the baseline median is at least
/// [`NOISE_FLOOR_MS`]. It regresses when the current median exceeds
/// `baseline * (1 + 0.10)`. Deterministic byte-size metrics are not compared.
pub(crate) fn compare_with_baseline(
    current: &BenchReport,
    baseline: &BenchReport,
) -> Vec<Regression> {
    let mut regressions = Vec::new();
    for fixture in &current.fixtures {
        for metric in &fixture.operations {
            if metric.unit != "ms" {
                continue;
            }
            let Some(previous) = find_baseline_metric(baseline, &fixture.name, &metric.name) else {
                continue;
            };
            if previous.value < NOISE_FLOOR_MS {
                continue;
            }
            let limit = previous.value * (1.0 + REGRESSION_TOLERANCE);
            if metric.value > limit {
                regressions.push(Regression {
                    fixture: fixture.name.clone(),
                    operation: metric.name.clone(),
                    baseline_ms: previous.value,
                    current_ms: metric.value,
                    ratio: (metric.value - previous.value) / previous.value,
                });
            }
        }
    }
    regressions
}

/// Decide the process exit code. `--report` never fails (informational only).
pub(crate) fn decide_exit(
    report_mode: bool,
    budget_breaches: usize,
    regressions: usize,
    baseline_invalid: bool,
) -> i32 {
    if report_mode {
        return 0;
    }
    if budget_breaches > 0 || regressions > 0 || baseline_invalid {
        1
    } else {
        0
    }
}

/// Render the human-readable summary table.
pub(crate) fn format_summary(report: &BenchReport, regressions: &[Regression]) -> String {
    let mut out = String::new();
    let _ = writeln!(
        out,
        "{} schema={} runs={}+{}x{} budget_tolerance={} regression_tolerance={}",
        report.harness,
        report.schema_version,
        report.config.warmup_runs,
        report.config.measured_runs,
        report.config.measurement_rounds,
        report.config.budget_tolerance,
        report.config.regression_tolerance
    );
    let _ = writeln!(
        out,
        "host={}/{} pdfium_available={} renderer=mock baseline={}",
        report.host.os,
        report.host.arch,
        report.host.pdfium_available,
        report.summary.baseline_status
    );
    out.push_str("fixture       operation                 median        budget       status\n");
    out.push_str("------------  ------------------------  ------------  ------------  ------\n");
    for fixture in &report.fixtures {
        for metric in &fixture.operations {
            let unit = metric.unit.as_str();
            let status = if metric.within_budget { "ok" } else { "BREACH" };
            let _ = writeln!(
                out,
                "{:<12}  {:<24}  {:>10.4} {:<3}  {:>10.4} {:<3}  {}",
                fixture.name, metric.name, metric.value, unit, metric.budget, unit, status
            );
        }
    }
    let _ = writeln!(
        out,
        "summary: metrics={} breaches={} regressions={} status={}",
        report.summary.operation_count,
        report.summary.budget_breaches,
        report.summary.regressions,
        report.summary.status
    );
    for regression in regressions {
        let _ = writeln!(
            out,
            "REGRESSION {}/{}: {} -> {} ms ({:.1}%)",
            regression.fixture,
            regression.operation,
            regression.baseline_ms,
            regression.current_ms,
            regression.ratio * 100.0
        );
    }
    out
}

/// Entry point used by `cargo xtask bench`.
///
/// `report_mode` disables failure; `update_baseline` writes the current report
/// as the new committed baseline and exits successfully.
pub(crate) fn run(report_mode: bool, update_baseline: bool) -> Result<(), String> {
    let mut report = measure_best_of()?;
    let baseline = load_baseline();
    let (baseline_status, baseline_invalid) = match &baseline {
        Ok(Some(_)) => ("present".to_owned(), false),
        Ok(None) => ("missing".to_owned(), false),
        Err(error) => (format!("invalid:{}", error.label()), true),
    };
    let regressions = match &baseline {
        Ok(Some(previous)) => compare_with_baseline(&report, previous),
        _ => Vec::new(),
    };
    report.summary.baseline_status = baseline_status;
    report.summary.regressions = regressions.len();
    report.summary.status =
        if report.summary.budget_breaches == 0 && regressions.is_empty() && !baseline_invalid {
            "pass".to_owned()
        } else {
            "fail".to_owned()
        };

    let json = serde_json::to_string_pretty(&report).map_err(|error| error.to_string())?;
    let latest = latest_report_path();
    if let Some(parent) = latest.parent() {
        let _ = fs::create_dir_all(parent);
    }
    let _ = fs::write(&latest, format!("{json}\n"));

    println!("{json}");
    println!("{}", format_summary(&report, &regressions));
    println!("report_json={}", latest.display());

    if update_baseline {
        let path = baseline_path();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).map_err(|error| error.to_string())?;
        }
        fs::write(&path, format!("{json}\n")).map_err(|error| error.to_string())?;
        println!("baseline_written={}", path.display());
        return Ok(());
    }

    let code = decide_exit(
        report_mode,
        report.summary.budget_breaches,
        report.summary.regressions,
        baseline_invalid,
    );
    if code == 0 {
        println!("BENCH OK (report_mode={report_mode})");
        Ok(())
    } else {
        Err(format!(
            "bench gate failed: breaches={} regressions={} baseline_invalid={}",
            report.summary.budget_breaches, report.summary.regressions, baseline_invalid
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering as AtomicOrdering};

    /// Build a minimal report with one deterministic metric for comparison.
    fn sample_report(value_ms: f64) -> BenchReport {
        BenchReport {
            schema_version: SCHEMA_VERSION,
            harness: HARNESS.to_owned(),
            config: ConfigReport {
                warmup_runs: WARMUP_RUNS,
                measured_runs: MEASURED_RUNS,
                budget_tolerance: BUDGET_TOLERANCE,
                regression_tolerance: REGRESSION_TOLERANCE,
                noise_floor_ms: NOISE_FLOOR_MS,
                measurement_rounds: MEASUREMENT_ROUNDS,
            },
            host: host_info(),
            fixtures: vec![FixtureReport {
                name: "sample".to_owned(),
                pages: 1,
                bytes: 100,
                operations: vec![Metric {
                    name: "open".to_owned(),
                    unit: "ms".to_owned(),
                    value: value_ms,
                    budget: 100.0,
                    within_budget: true,
                    min: value_ms,
                    max: value_ms,
                    samples: MEASURED_RUNS,
                }],
            }],
            summary: Summary {
                operation_count: 1,
                budget_breaches: 0,
                regressions: 0,
                baseline_status: "present".to_owned(),
                status: "pass".to_owned(),
            },
        }
    }

    #[test]
    fn fixture_generation_is_deterministic_and_controlled() {
        let small = generate_pdf(1, 24);
        assert_eq!(small, generate_pdf(1, 24));
        assert!(small.starts_with(b"%PDF-1.7"));
        assert!(small.ends_with(b"%%EOF\n"));
        let text = String::from_utf8_lossy(&small);
        assert!(text.contains("/Count 1"), "page count missing");
        let medium = generate_pdf(100, 24);
        let large = generate_pdf(500, 48);
        assert!(medium.len() > small.len(), "page count must control size");
        assert!(large.len() > medium.len(), "page count must control size");
        assert!(String::from_utf8_lossy(&large).contains("/Count 500"));
    }

    #[test]
    fn budget_pass_fail_logic_handles_boundaries() {
        assert!(within_budget(100.0, 125.0));
        assert!(within_budget(125.0, 125.0), "equal is within budget");
        assert!(!within_budget(125.1, 125.0));
        assert!(!within_budget(f64::NAN, 125.0), "NaN must not pass");
    }

    #[test]
    fn tolerance_math_scales_with_page_count_and_budget_tolerance() {
        assert_eq!(budget_ms(1_000.0, 500), 1_250.0);
        assert_eq!(budget_ms(1_000.0, 100), 250.0);
        // The 1-page budget is floored before the tolerance is applied.
        assert_eq!(
            budget_ms(1_000.0, 1),
            MIN_BUDGET_MS * (1.0 + BUDGET_TOLERANCE)
        );
    }

    #[test]
    fn baseline_comparison_flags_only_over_ten_percent_regressions() {
        let baseline = sample_report(10.0);
        let under = sample_report(11.0);
        assert!(
            compare_with_baseline(&under, &baseline).is_empty(),
            "10% is not >10%"
        );
        let over = sample_report(11.5);
        let found = compare_with_baseline(&over, &baseline);
        assert_eq!(found.len(), 1);
        assert_eq!(found[0].fixture, "sample");
        assert_eq!(found[0].operation, "open");
        assert!((found[0].ratio - 0.15).abs() < 1e-9);
    }

    #[test]
    fn baseline_regression_respects_noise_floor() {
        let baseline = sample_report(0.5);
        let doubled = sample_report(2.0);
        assert!(
            compare_with_baseline(&doubled, &baseline).is_empty(),
            "sub-noise-floor metrics are not tracked"
        );
    }

    #[test]
    fn report_mode_never_fails_even_with_breaches_and_invalid_baseline() {
        assert_eq!(decide_exit(true, 3, 3, true), 0);
        assert_eq!(decide_exit(false, 0, 0, false), 0);
        assert_eq!(decide_exit(false, 1, 0, false), 1);
        assert_eq!(decide_exit(false, 0, 1, false), 1);
        assert_eq!(decide_exit(false, 0, 0, true), 1);
    }

    #[test]
    fn json_schema_is_stable_and_round_trips() {
        let report = sample_report(2.5);
        let first = serde_json::to_string_pretty(&report).expect("serialise");
        let second = serde_json::to_string_pretty(&report).expect("serialise again");
        assert_eq!(first, second);
        assert!(first.contains("\"schema_version\": 1"));
        assert!(first.contains("\"harness\": \"pdfxi-xtask-bench\""));
        let parsed: BenchReport = serde_json::from_str(&first).expect("parse");
        assert_eq!(parsed, report);
    }

    #[test]
    fn warmup_calls_are_not_measured() {
        let counter = AtomicUsize::new(0);
        let samples = warmup_then_measure(2, 3, || {
            counter.fetch_add(1, AtomicOrdering::SeqCst);
            Ok(1usize)
        });
        assert_eq!(counter.load(AtomicOrdering::SeqCst), 5);
        assert_eq!(samples.len(), 3);
        assert_eq!(stats(&samples).expect("stats").samples, 3);
    }

    #[test]
    fn invalid_baseline_handling_is_explicit_and_panic_free() {
        assert_eq!(parse_baseline("not json"), Err(BaselineError::Json));
        assert_eq!(parse_baseline("{}"), Err(BaselineError::Json));
        let mut wrong = sample_report(1.0);
        wrong.schema_version = 99;
        let text = serde_json::to_string(&wrong).expect("serialise");
        assert_eq!(parse_baseline(&text), Err(BaselineError::Schema));
        assert_eq!(BaselineError::Schema.label(), "schema");
    }

    #[test]
    fn operation_names_and_units_are_stable() {
        assert_eq!(
            OPERATION_NAMES,
            [
                "open",
                "first_page_render",
                "text_extract",
                "search_index_build",
                "save"
            ]
        );
        let bytes = generate_pdf(1, 24);
        let metric = allocation_metric(&bytes);
        assert_eq!(metric.unit, "bytes");
        assert_eq!(metric.name, "peak_allocation_estimate");
        assert!(metric.within_budget);
    }

    #[test]
    fn stats_summary_uses_lower_middle_median() {
        let values = [12u64, 10, 11].map(Duration::from_millis);
        let summary = stats(&values).expect("stats");
        assert_eq!(summary.min_ms, 10.0);
        assert_eq!(summary.median_ms, 11.0);
        assert_eq!(summary.max_ms, 12.0);
    }
}
