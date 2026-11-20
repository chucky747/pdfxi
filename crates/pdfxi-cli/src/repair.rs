//! `pdfxi repair` — expose the core repair path as a headless command.
//!
//! The command reads a damaged PDF, runs [`pdfxi_core::repair_document`],
//! writes a verified standalone PDF to a distinct output path and reports the
//! typed repairs that were applied. It never modifies its input and never
//! writes through a traversal path.
//!
//! # Exit codes
//!
//! - `0` — the input was repaired, the output was written and reparsed.
//! - `1` — a CLI, path-validation or I/O error (reported through `anyhow`).
//! - `3` — the input could not be repaired at all ([`EXIT_REPAIR_FAILED`]).
//!
//! In `--json` mode a typed, stable JSON report is printed to stdout on both
//! success and repair failure; the exit code still follows the table above.

use std::io::Write as _;
use std::path::{Component, Path};
use std::process;

use anyhow::{Context, bail};
use pdfxi_core::{Document, RepairWarning, repair_document};
use serde::Serialize;

/// Exit code used when the input cannot be repaired at all.
pub const EXIT_REPAIR_FAILED: i32 = 3;

/// A stable JSON view of one typed repair warning.
#[derive(Debug, Serialize)]
struct WarningJson {
    /// Stable machine-readable warning code.
    code: &'static str,
    /// Human-readable explanation produced by the core crate.
    message: String,
}

/// Stable JSON shape of the whole `repair` result.
#[derive(Debug, Serialize)]
struct RepairJson {
    /// Whether a repaired document was written.
    ok: bool,
    /// Display path of the input file.
    input: String,
    /// Display path of the output file.
    output: String,
    /// Fatal repair error, present only when `ok` is false.
    error: Option<String>,
    /// Effective PDF version of the repaired file, empty on failure.
    version: String,
    /// Size of the repaired file in bytes, zero on failure.
    bytes: usize,
    /// Number of leaf pages in the repaired file, zero on failure.
    pages: usize,
    /// Typed repairs applied to the input, in deterministic order.
    warnings: Vec<WarningJson>,
    /// Number of applied repairs.
    warning_count: usize,
    /// Exit code this command will use.
    exit_code: i32,
}

/// Map a typed core warning to a stable lowercase code.
fn warning_code(warning: &RepairWarning) -> &'static str {
    match warning {
        RepairWarning::HeaderNormalized { .. } => "header_normalized",
        RepairWarning::HeaderNotAtStart { .. } => "header_not_at_start",
        RepairWarning::StartXrefMissing => "startxref_missing",
        RepairWarning::StartXrefMisplaced { .. } => "startxref_misplaced",
        RepairWarning::XrefRebuiltByScan => "xref_rebuilt_by_scan",
        RepairWarning::WrongXrefOffsets { .. } => "wrong_xref_offsets",
        RepairWarning::StreamLengthCorrected { .. } => "stream_length_corrected",
        RepairWarning::TruncatedObjectEnd { .. } => "truncated_object_end",
        RepairWarning::TruncatedEof => "truncated_eof",
        RepairWarning::DuplicateObject { .. } => "duplicate_object",
    }
}

/// Reject an output path that contains a `..` (parent-directory) component.
///
/// Absolute paths are accepted because callers legitimately name outputs with
/// absolute paths; only explicit parent traversal is forbidden. Symlink and
/// overwrite safety are handled separately by [`paths_reference_same_file`].
fn validate_no_traversal(path: &Path) -> anyhow::Result<()> {
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            bail!(
                "output path '{}' contains a forbidden traversal ('..') component",
                path.display()
            );
        }
    }
    Ok(())
}

/// Determine whether `input` and `output` name the same file.
///
/// Both paths are resolved to an absolute, symlink-following form before they
/// are compared. A missing output is resolved through its parent directory so
/// the comparison works before the file is created. This refuses an in-place
/// "repair" through either an identical path or a symlink to the input.
fn paths_reference_same_file(input: &Path, output: &Path) -> anyhow::Result<bool> {
    let input_resolved = std::fs::canonicalize(input)
        .with_context(|| format!("failed to resolve input '{}'", input.display()))?;
    let output_resolved = if output.exists() {
        std::fs::canonicalize(output)
            .with_context(|| format!("failed to resolve output '{}'", output.display()))?
    } else {
        let parent = output
            .parent()
            .filter(|candidate| !candidate.as_os_str().is_empty())
            .unwrap_or_else(|| Path::new("."));
        let parent_resolved = std::fs::canonicalize(parent).with_context(|| {
            format!("failed to resolve output directory '{}'", parent.display())
        })?;
        match output.file_name() {
            Some(name) => parent_resolved.join(name),
            None => parent_resolved,
        }
    };
    Ok(input_resolved == output_resolved)
}

/// Print a failed-repair JSON report, then exit with [`EXIT_REPAIR_FAILED`].
fn fail_with_report(input: &str, output: &str, message: &str, json: bool) -> anyhow::Result<()> {
    if json {
        let payload = RepairJson {
            ok: false,
            input: input.to_owned(),
            output: output.to_owned(),
            error: Some(message.to_owned()),
            version: String::new(),
            bytes: 0,
            pages: 0,
            warnings: Vec::new(),
            warning_count: 0,
            exit_code: EXIT_REPAIR_FAILED,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
    }
    eprintln!("repair failed: {message}");
    let _ = std::io::stdout().flush();
    process::exit(EXIT_REPAIR_FAILED);
}

/// Run the `repair` command.
///
/// The input path is validated for traversal, read from disk, repaired,
/// written to a distinct output path and reparsed before success is reported.
///
/// # Errors
///
/// Returns an `anyhow` error for a traversal path, an unreadable input, an
/// output that resolves to the input, an unwritable output, or a repaired file
/// that fails to reparse. A fatal core repair error exits with
/// [`EXIT_REPAIR_FAILED`] after printing a report instead of returning `Err`.
pub fn run_repair(input: &Path, output: &Path, json: bool) -> anyhow::Result<()> {
    validate_no_traversal(output)?;

    let bytes =
        std::fs::read(input).with_context(|| format!("failed to read '{}'", input.display()))?;
    if paths_reference_same_file(input, output)? {
        bail!(
            "refusing to overwrite the input file '{}'; choose a different output path",
            input.display()
        );
    }

    let input_display = input.display().to_string();
    let output_display = output.display().to_string();
    let repaired = match repair_document(&bytes) {
        Ok(document) => document,
        Err(error) => {
            fail_with_report(&input_display, &output_display, &error.to_string(), json)?;
            return Ok(());
        }
    };

    std::fs::write(output, repaired.bytes())
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    let written = std::fs::read(output)
        .with_context(|| format!("failed to re-read '{}'", output.display()))?;
    let document = Document::from_bytes(written).context("repaired output failed to reparse")?;
    let pages = document
        .page_count()
        .context("repaired output has no page tree")?;

    let warnings: Vec<WarningJson> = repaired
        .report()
        .warnings()
        .iter()
        .map(|warning| WarningJson {
            code: warning_code(warning),
            message: warning.to_string(),
        })
        .collect();
    let warning_count = warnings.len();

    if json {
        let payload = RepairJson {
            ok: true,
            input: input_display.clone(),
            output: output_display.clone(),
            error: None,
            version: repaired.version().to_string(),
            bytes: repaired.bytes().len(),
            pages,
            warnings,
            warning_count,
            exit_code: 0,
        };
        println!("{}", serde_json::to_string_pretty(&payload)?);
    } else {
        println!("input: {input_display}");
        println!("output: {output_display}");
        println!("version: {}", repaired.version());
        println!("bytes: {}", repaired.bytes().len());
        println!("pages: {pages}");
        println!("warnings: {warning_count}");
        for warning in &warnings {
            println!("  {}: {}", warning.code, warning.message);
        }
        println!("repaired: ok");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn traversal_is_rejected_but_absolute_paths_are_allowed() {
        assert!(validate_no_traversal(Path::new("C:/tmp/out.pdf")).is_ok());
        assert!(validate_no_traversal(Path::new("out.pdf")).is_ok());
        let error = validate_no_traversal(Path::new("../out.pdf")).expect_err("traversal");
        assert!(error.to_string().contains("traversal"), "{error}");
        assert!(validate_no_traversal(Path::new("a/../b.pdf")).is_err());
    }

    #[test]
    fn warning_codes_are_stable() {
        assert_eq!(
            warning_code(&RepairWarning::StartXrefMissing),
            "startxref_missing"
        );
        assert_eq!(
            warning_code(&RepairWarning::XrefRebuiltByScan),
            "xref_rebuilt_by_scan"
        );
        assert_eq!(warning_code(&RepairWarning::TruncatedEof), "truncated_eof");
    }
}
