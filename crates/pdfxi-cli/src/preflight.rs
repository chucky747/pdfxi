//! `pdfxi preflight` — a user-visible surface for byte-level subset preflight.
//!
//! The command renders a [`pdfxi_convert::preflight::BytePreflightReport`] as
//! stable text or JSON. Exit codes are deterministic: 0 when the final report
//! has no error-severity findings, 2 when findings fail the check, and 1 for
//! typed input/CLI errors (reported through `anyhow`). A passing exit code is
//! *not* a conformance certification: the report always states that
//! certification is not claimed and lists reported-only constraints.

use std::io::Write as _;
use std::path::{Path, PathBuf};

use anyhow::Context;
use pdfxi_convert::preflight::{
    BytePreflightReport, PreflightOptions, PreflightReport, preflight_bytes,
};
use pdfxi_core::Document;
use serde::Serialize;

/// Exit code used when the final preflight report has error-severity findings.
pub const EXIT_NON_CONFORMING: i32 = 2;

/// A stable JSON view of one preflight finding.
#[derive(Debug, Serialize)]
struct FindingJson {
    severity: String,
    code: String,
    message: String,
    location: Option<String>,
}

/// A stable JSON view of one before/after report.
#[derive(Debug, Serialize)]
struct ReportJson {
    standard: String,
    conforming: bool,
    error_count: usize,
    warning_count: usize,
    info_count: usize,
    findings: Vec<FindingJson>,
}

/// Stable JSON shape of the whole `preflight` result.
#[derive(Debug, Serialize)]
struct PreflightJson {
    profile: String,
    conforming: bool,
    certification_claimed: bool,
    exit_code: i32,
    before: ReportJson,
    after: Option<ReportJson>,
    enforced: Vec<String>,
    reported_only: Vec<String>,
    fixups: Vec<String>,
    output: Option<String>,
    scope_note: String,
}

fn report_json(report: &PreflightReport) -> ReportJson {
    ReportJson {
        standard: report.standard().to_string(),
        conforming: report.is_conforming(),
        error_count: report.error_count(),
        warning_count: report.warning_count(),
        info_count: report.info_count(),
        findings: report
            .findings()
            .iter()
            .map(|finding| FindingJson {
                severity: finding.severity().label().to_owned(),
                code: finding.code().to_owned(),
                message: finding.message().to_owned(),
                location: finding.location().map(str::to_owned),
            })
            .collect(),
    }
}

/// Derive the deterministic default output path for `--fix` without `--output`.
fn default_output_path(input: &Path) -> PathBuf {
    let stem = input.file_stem().map_or_else(
        || "input".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    input.with_file_name(format!("{stem}.preflight.pdf"))
}

/// Print one report in the stable text format.
fn print_report(label: &str, report: &PreflightReport) {
    println!("{label}: {}", report.summary());
    for finding in report.findings() {
        let location = finding
            .location()
            .map_or_else(String::new, |value| format!(" ({value})"));
        println!(
            "  {} {}: {}{}",
            finding.severity().label(),
            finding.code(),
            finding.message(),
            location
        );
    }
}

/// Print the whole report in the stable text format.
fn print_text(result: &BytePreflightReport, written: Option<&str>) {
    println!("profile: {}", result.profile_label());
    print_report("before", result.before());
    if let Some(after) = result.after() {
        print_report("after", after);
    }
    if result.fixups().applied_count() > 0 {
        let names: Vec<String> = result
            .fixups()
            .applied()
            .iter()
            .map(|record| format!("{:?}", record.fixup()))
            .collect();
        println!("fixups: {}", names.join(", "));
    }
    println!("enforced: {}", result.enforced().join(", "));
    println!("reported-only: {}", result.reported_only().join(", "));
    println!("certification: not claimed (subset preflight)");
    if let Some(path) = written {
        println!("output: {path}");
    }
    println!(
        "verdict: {}",
        if result.is_conforming() {
            "pass"
        } else {
            "fail"
        }
    );
}

/// Run the `preflight` command.
///
/// `output` is used only with `fix`: it names the normalized PDF to write. When
/// absent, a deterministic `<stem>.preflight.pdf` sibling of the input is used.
pub fn run_preflight(
    profile: &str,
    input: &Path,
    fix: bool,
    output: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(input).with_context(|| format!("failed to read '{}'", input.display()))?;
    let result = preflight_bytes(&bytes, profile, &PreflightOptions { fix })
        .with_context(|| format!("preflight of '{}' failed", input.display()))?;

    let mut written = None;
    if fix {
        let out_path = output.map_or_else(|| default_output_path(input), Path::to_path_buf);
        let normalized = result.output_bytes().ok_or_else(|| {
            anyhow::anyhow!("fix requested but no normalized bytes were produced")
        })?;
        std::fs::write(&out_path, normalized)
            .with_context(|| format!("failed to write '{}'", out_path.display()))?;
        let saved = std::fs::read(&out_path)
            .with_context(|| format!("failed to re-read '{}'", out_path.display()))?;
        let reparsed =
            Document::from_bytes(saved).context("normalized output failed to reparse")?;
        let pages = reparsed
            .page_count()
            .context("normalized output has no page tree")?;
        let source_pages = Document::from_bytes(bytes.clone())
            .and_then(|document| document.page_count())
            .context("source has no page tree")?;
        if pages != source_pages {
            anyhow::bail!("normalized output has {pages} page(s), source had {source_pages}");
        }
        written = Some(out_path.display().to_string());
    }

    let conforming = result.is_conforming();
    let exit_code = if conforming { 0 } else { EXIT_NON_CONFORMING };
    if json {
        let payload = PreflightJson {
            profile: result.profile_label().to_owned(),
            conforming,
            certification_claimed: result.certification_claimed(),
            exit_code,
            before: report_json(result.before()),
            after: result.after().map(report_json),
            enforced: result
                .enforced()
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            reported_only: result
                .reported_only()
                .iter()
                .map(|value| (*value).to_owned())
                .collect(),
            fixups: result
                .fixups()
                .applied()
                .iter()
                .map(|record| format!("{:?}", record.fixup()))
                .collect(),
            output: written,
            scope_note: result.scope_note().to_owned(),
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        print_text(&result, written.as_deref());
    }

    if !conforming {
        std::io::stdout()
            .flush()
            .context("failed to flush stdout")?;
        std::process::exit(exit_code);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_output_path_is_deterministic_sibling() {
        let path = default_output_path(Path::new("C:/tmp/report.pdf"));
        assert_eq!(path, PathBuf::from("C:/tmp/report.preflight.pdf"));
    }

    #[test]
    fn non_conforming_exit_code_is_distinct() {
        assert_ne!(EXIT_NON_CONFORMING, 0);
        assert_ne!(EXIT_NON_CONFORMING, 1);
    }
}
