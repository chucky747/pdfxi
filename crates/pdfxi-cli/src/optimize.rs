//! `pdfxi optimize`: rewrite a PDF with the document optimizer.
//!
//! The optimizer lives in `pdfxi-core` (`Document::optimize`). This command is
//! the scriptable host for it: it loads a document, runs the optimizer with a
//! fresh cancellation token, writes the standalone result and prints the
//! deterministic report. It refuses to overwrite its own input so a failed or
//! interrupted run cannot destroy the source.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{CancellationToken, Document, OptimizeOptions};
use serde::Serialize;

/// JSON shape of `pdfxi optimize --json`.
#[derive(Serialize)]
struct OptimizeReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Input size in bytes.
    bytes_before: usize,
    /// Output size in bytes.
    bytes_after: usize,
    /// `bytes_after - bytes_before`, negative when the file shrank.
    saved_bytes: i64,
    /// Objects before any transform.
    objects_before: usize,
    /// Objects written.
    objects_after: usize,
    /// Streams Flate-compressed.
    streams_compressed: usize,
    /// Already-encoded image streams passed through.
    images_passed_through: usize,
    /// Objects merged into an identical counterpart.
    objects_deduplicated: usize,
    /// Unreachable objects removed.
    objects_removed: usize,
    /// Sorted, deduplicated optimizer notes.
    warnings: Vec<String>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Optimize `file` into `out` and print the report.
pub(crate) fn run_optimize(
    file: &Path,
    out: &Path,
    remove_metadata: bool,
    remove_optional: bool,
    compress: bool,
    json: bool,
) -> anyhow::Result<()> {
    if out.exists() {
        let input =
            std::fs::canonicalize(file).with_context(|| format!("resolve {}", file.display()))?;
        let output =
            std::fs::canonicalize(out).with_context(|| format!("resolve {}", out.display()))?;
        if input == output {
            bail!("refusing to overwrite the input document; choose a different --out");
        }
    }

    let document = load(file)?;
    let options = OptimizeOptions {
        remove_metadata,
        remove_optional_entries: remove_optional,
        compress_streams: compress,
        ..OptimizeOptions::default()
    };
    let token = CancellationToken::new();
    let result = document
        .optimize(&options, &token)
        .with_context(|| format!("optimize {}", file.display()))?;

    std::fs::write(out, &result.bytes).with_context(|| format!("write {}", out.display()))?;

    let report = OptimizeReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        bytes_before: result.report.bytes_before,
        bytes_after: result.report.bytes_after,
        saved_bytes: result.report.bytes_after as i64 - result.report.bytes_before as i64,
        objects_before: result.report.objects_before,
        objects_after: result.report.objects_after,
        streams_compressed: result.report.streams_compressed,
        images_passed_through: result.report.images_passed_through,
        objects_deduplicated: result.report.objects_deduplicated,
        objects_removed: result.report.objects_removed,
        warnings: result.report.warnings,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("file: {}", report.file);
        println!("out:  {}", report.out);
        println!(
            "bytes: {} -> {} ({})",
            report.bytes_before,
            report.bytes_after,
            size_delta(report.bytes_before, report.bytes_after)
        );
        println!(
            "objects: {} -> {}",
            report.objects_before, report.objects_after
        );
        println!("streams compressed: {}", report.streams_compressed);
        println!("images passed through: {}", report.images_passed_through);
        println!("deduplicated: {}", report.objects_deduplicated);
        println!("removed: {}", report.objects_removed);
        println!("warnings: {}", report.warnings.len());
        for warning in &report.warnings {
            println!("  {warning}");
        }
    }
    Ok(())
}

/// Percentage size change with one decimal and an explicit sign.
///
/// A zero-byte input has no meaningful ratio, so it reports `0.0%` instead of
/// dividing by zero.
fn size_delta(before: usize, after: usize) -> String {
    if before == 0 {
        return "0.0%".to_owned();
    }
    let percent = (after as f64 - before as f64) / before as f64 * 100.0;
    format!("{percent:+.1}%")
}

#[cfg(test)]
mod tests {
    use super::size_delta;

    #[test]
    fn size_delta_reports_a_signed_percentage() {
        assert_eq!(size_delta(100, 90), "-10.0%");
        assert_eq!(size_delta(100, 110), "+10.0%");
        assert_eq!(size_delta(200, 200), "+0.0%");
        assert_eq!(size_delta(0, 0), "0.0%");
    }
}
