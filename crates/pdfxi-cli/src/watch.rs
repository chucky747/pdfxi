//! `pdfxi watch`: one host-driven poll of a watched folder.
//!
//! [`pdfxi_automation::WatchedFolder`] scans and converts real files but starts
//! no thread and keeps no clock. This host supplies the missing pieces: a real
//! directory, an output directory, a persisted marker snapshot, and a
//! deterministic number of host ticks through `PollSchedule`. The OS scheduler
//! that invokes the command repeatedly is intentionally outside this program.
//!
//! Converted bytes come from the real in-process adapters: `.txt` and `.md`
//! become text PDFs, `.html`/`.htm` go through the bounded HTML importer,
//! `.png`/`.jpg`/`.jpeg` are embedded, and `.pdf` is passed through unchanged.
//! A converter error is a typed per-file failure that stays eligible for retry.
//!
//! Safety: the output directory is canonicalized, must exist outside the watched
//! root, and every output path is checked to stay beneath it.

use std::path::{Component, Path, PathBuf};

use anyhow::{Context, bail};
use pdfxi_automation::{
    BackoffPolicy, FileOutcomeState, FolderConverter, MarkerSnapshot, PollInterval, PollSchedule,
    WatchedFolder, WatchedFolderConfig,
};
use pdfxi_convert::{
    CaptureOptions, CreateImageFormat, CreateOptions, capture_html_markup, create_from_image,
    create_from_markdown, create_from_text,
};
use serde::Serialize;

/// One file outcome.
#[derive(Serialize)]
struct FileRow {
    /// Path relative to the watched root.
    relative: String,
    /// `converted`, `skipped`, `unsupported` or `failed`.
    state: &'static str,
    /// Optional explanation.
    #[serde(skip_serializing_if = "Option::is_none")]
    message: Option<String>,
    /// Output name for converted files.
    #[serde(skip_serializing_if = "Option::is_none")]
    output: Option<String>,
}

/// One poll's report.
#[derive(Serialize)]
struct PollRow {
    /// Tick number that triggered the poll.
    tick: u64,
    /// Files converted.
    converted: usize,
    /// Files already processed.
    skipped: usize,
    /// Files with an ineligible extension.
    unsupported: usize,
    /// Files that failed and remain eligible for retry.
    failed: usize,
    /// Per-file outcomes in discovery order.
    files: Vec<FileRow>,
}

/// One schedule decision.
#[derive(Serialize)]
struct DecisionRow {
    /// Tick number.
    tick: u64,
    /// `due` or `wait`.
    decision: &'static str,
    /// Ticks the host should wait, when not due.
    #[serde(skip_serializing_if = "Option::is_none")]
    wait_ticks: Option<u64>,
}

/// JSON shape of `pdfxi watch`.
#[derive(Serialize)]
struct WatchReport {
    /// Watched root.
    folder: String,
    /// Output root.
    out: String,
    /// Eligible extensions.
    extensions: Vec<String>,
    /// Output suffix.
    suffix: String,
    /// Marker snapshot path, when persisted.
    markers: Option<String>,
    /// Host ticks advanced.
    ticks: u64,
    /// Polls that became due.
    polls: usize,
    /// Schedule status line.
    schedule: String,
    /// Every tick decision.
    decisions: Vec<DecisionRow>,
    /// Every poll report.
    reports: Vec<PollRow>,
    /// Output files written.
    written: usize,
    /// Markers after the run.
    marker_count: usize,
}

/// Converts each watched file with the real in-process PDF adapters.
struct PdfConverter;

impl FolderConverter for PdfConverter {
    fn convert(&self, relative_path: &str, bytes: &[u8]) -> Result<Vec<u8>, String> {
        let extension = Path::new(relative_path)
            .extension()
            .and_then(|value| value.to_str())
            .unwrap_or_default()
            .to_ascii_lowercase();
        match extension.as_str() {
            "pdf" => Ok(bytes.to_vec()),
            "txt" => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|error| format!("{relative_path}: not UTF-8 text: {error}"))?;
                create_from_text(text, &CreateOptions::default())
                    .map_err(|error| format!("{relative_path}: {error}"))
            }
            "md" | "markdown" => {
                let text = std::str::from_utf8(bytes)
                    .map_err(|error| format!("{relative_path}: not UTF-8 text: {error}"))?;
                create_from_markdown(text, &CreateOptions::default())
                    .map_err(|error| format!("{relative_path}: {error}"))
            }
            "html" | "htm" => {
                let markup = std::str::from_utf8(bytes)
                    .map_err(|error| format!("{relative_path}: not UTF-8 HTML: {error}"))?;
                capture_html_markup(relative_path, markup, &CaptureOptions::default())
                    .map(|result| result.into_pdf())
                    .map_err(|error| format!("{relative_path}: {error}"))
            }
            "png" | "jpg" | "jpeg" => {
                let format = CreateImageFormat::from_extension(&extension)
                    .ok_or_else(|| format!("{relative_path}: unsupported image {extension:?}"))?;
                create_from_image(bytes, format, &CreateOptions::default())
                    .map_err(|error| format!("{relative_path}: {error}"))
            }
            other => Err(format!(
                "{relative_path}: no PDF converter for extension {other:?}"
            )),
        }
    }
}

/// Whether `child` is inside `root` (both canonical).
fn is_inside(root: &Path, child: &Path) -> bool {
    child.starts_with(root) && child != root
}

/// Join a relative model output name safely beneath `root`.
fn safe_output(root: &Path, relative: &str) -> anyhow::Result<PathBuf> {
    let candidate = Path::new(relative);
    for component in candidate.components() {
        if !matches!(component, Component::Normal(_)) {
            bail!("output name {relative:?} is not a safe relative path");
        }
    }
    let joined = root.join(candidate);
    if joined.parent().is_none() {
        bail!("output name {relative:?} has no parent");
    }
    Ok(joined)
}

/// `pdfxi watch`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_watch(
    folder: &Path,
    out: &Path,
    extensions: &[String],
    suffix: &str,
    markers_path: Option<&Path>,
    poll_every: u64,
    base_ticks: u64,
    max_ticks: u64,
    multiplier: u32,
    ticks: u64,
    json: bool,
) -> anyhow::Result<()> {
    if ticks == 0 {
        bail!("--ticks must be at least 1");
    }
    let root = std::fs::canonicalize(folder)
        .with_context(|| format!("resolve watched folder {}", folder.display()))?;
    if !root.is_dir() {
        bail!("watched folder {} is not a directory", root.display());
    }
    if !out.exists() {
        std::fs::create_dir_all(out)
            .with_context(|| format!("create output directory {}", out.display()))?;
    }
    let out_root = std::fs::canonicalize(out)
        .with_context(|| format!("resolve output directory {}", out.display()))?;
    if !out_root.is_dir() {
        bail!("output {} is not a directory", out_root.display());
    }
    if is_inside(&root, &out_root) {
        bail!(
            "output directory {} is inside the watched folder; outputs would be rescanned",
            out_root.display()
        );
    }
    let mut config = WatchedFolderConfig::new(&root)
        .map_err(|error| anyhow::anyhow!("watched folder: {error}"))?;
    if !extensions.is_empty() {
        config = config
            .with_extensions(extensions.iter().map(String::as_str))
            .map_err(|error| anyhow::anyhow!("eligible extensions: {error}"))?;
    }
    let suffix = if suffix.trim().is_empty() {
        "pdf"
    } else {
        suffix.trim()
    };
    config = config
        .with_output_suffix(suffix)
        .map_err(|error| anyhow::anyhow!("output suffix: {error}"))?;
    let mut watched =
        WatchedFolder::new(config).map_err(|error| anyhow::anyhow!("watched folder: {error}"))?;
    if let Some(path) = markers_path
        && path.exists()
    {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("read markers {}", path.display()))?;
        let snapshot = MarkerSnapshot::from_json_string(&text)
            .map_err(|error| anyhow::anyhow!("parse markers {}: {error}", path.display()))?;
        *watched.markers_mut() = snapshot.restore();
    }

    let interval = if poll_every == 0 {
        PollInterval::EveryPoll
    } else {
        PollInterval::EveryTicks(poll_every)
    };
    let backoff = BackoffPolicy {
        base_ticks,
        multiplier_per_mille: multiplier,
        max_ticks,
    };
    let mut schedule = PollSchedule::new(interval, backoff)
        .map_err(|error| anyhow::anyhow!("schedule: {error}"))?;

    let mut decisions = Vec::new();
    let mut reports = Vec::new();
    let mut written = 0usize;
    for tick in 1..=ticks {
        match schedule.advance_tick() {
            pdfxi_automation::ScheduleDecision::Due => {
                decisions.push(DecisionRow {
                    tick,
                    decision: "due",
                    wait_ticks: None,
                });
                let report = watched
                    .poll_once(&PdfConverter)
                    .map_err(|error| anyhow::anyhow!("poll tick {tick}: {error}"))?;
                let mut files = Vec::with_capacity(report.entries().len());
                for entry in report.entries() {
                    let mut output = None;
                    if entry.state() == FileOutcomeState::Converted {
                        let bytes = entry
                            .output()
                            .context("converted outcome carries no bytes")?;
                        let name = entry
                            .output_name()
                            .context("converted outcome carries no output name")?;
                        let target = safe_output(&out_root, name)?;
                        if let Some(parent) = target.parent() {
                            std::fs::create_dir_all(parent).with_context(|| {
                                format!("create output directory {}", parent.display())
                            })?;
                        }
                        std::fs::write(&target, bytes)
                            .with_context(|| format!("write {}", target.display()))?;
                        written += 1;
                        output = Some(name.to_owned());
                    }
                    files.push(FileRow {
                        relative: entry.relative().to_owned(),
                        state: entry.state().as_str(),
                        message: entry.message().map(str::to_owned),
                        output,
                    });
                }
                let failed = report.count(FileOutcomeState::Failed);
                if failed > 0 {
                    schedule.record_failure();
                } else {
                    schedule.record_success();
                }
                reports.push(PollRow {
                    tick,
                    converted: report.count(FileOutcomeState::Converted),
                    skipped: report.count(FileOutcomeState::Skipped),
                    unsupported: report.count(FileOutcomeState::Unsupported),
                    failed,
                    files,
                });
            }
            pdfxi_automation::ScheduleDecision::Wait { wait_ticks } => {
                decisions.push(DecisionRow {
                    tick,
                    decision: "wait",
                    wait_ticks: Some(wait_ticks),
                });
            }
        }
    }

    let marker_count = watched.markers().len();
    if let Some(path) = markers_path {
        let text = MarkerSnapshot::capture(watched.markers()).to_json_string();
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, text.as_bytes())
            .with_context(|| format!("write markers {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("replace markers {}", path.display()))?;
    }

    let report = WatchReport {
        folder: root.display().to_string(),
        out: out_root.display().to_string(),
        extensions: watched.config().extensions().to_vec(),
        suffix: watched.config().output_suffix().to_owned(),
        markers: markers_path.map(|path| path.display().to_string()),
        ticks,
        polls: reports.len(),
        schedule: schedule.status_summary(),
        decisions,
        reports,
        written,
        marker_count,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("folder: {}", report.folder);
    println!("out: {}", report.out);
    println!(
        "extensions: {}  suffix: {}",
        report.extensions.join(","),
        report.suffix
    );
    println!("ticks: {}  polls: {}", report.ticks, report.polls);
    for decision in &report.decisions {
        if let Some(wait) = decision.wait_ticks {
            println!("tick {}: wait {} more", decision.tick, wait);
        } else {
            println!("tick {}: due", decision.tick);
        }
    }
    for poll in &report.reports {
        println!(
            "poll tick {}: {} converted, {} skipped, {} unsupported, {} failed",
            poll.tick, poll.converted, poll.skipped, poll.unsupported, poll.failed
        );
        for file in &poll.files {
            println!("  {}\t{}", file.state, file.relative);
        }
    }
    println!("written: {}", report.written);
    println!("markers: {} ({})", report.marker_count, report.schedule);
    Ok(())
}
