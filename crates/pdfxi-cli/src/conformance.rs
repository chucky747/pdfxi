//! `pdfxi export conformance`: normalize a PDF for a PDF/A, PDF/X or PDF/E target.
//!
//! The normalization itself lives in `pdfxi-convert::conformance`. This module is
//! the host: it resolves the requested profile, optionally prints the byte-free
//! plan, applies the mechanical normalizations, then verifies the written bytes
//! with `pdfxi-core` (page text, header version, and the required catalog
//! entries) before reporting. The report repeats the exporter's
//! `certification_claimed: false` and scope note, so a normalized file is never
//! presented as a certified conformance claim.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{
    ConformanceRequest, ConformanceTarget, ForbiddenEntryPolicy, NormalizationOptions,
    export_conformance_pdf, plan_conformance_export,
};
use pdfxi_core::Document;
use serde::Serialize;

/// One `pdfxi export conformance` request.
pub(crate) struct ExportRequest<'a> {
    /// Input file.
    pub input: &'a Path,
    /// Output file; required unless `plan_only`.
    pub out: Option<&'a Path>,
    /// Target label such as `pdfa-2b`, `pdfx-4` or `pdfe-1`.
    pub profile: &'a str,
    /// `report` or `reject`.
    pub policy: &'a str,
    /// Explicit output-intent subtype.
    pub output_intent: Option<&'a str>,
    /// Whether to normalize the header version.
    pub normalize_version: bool,
    /// Whether to add the XMP metadata placeholder.
    pub add_xmp_metadata: bool,
    /// Whether to add the output-intent placeholder.
    pub add_output_intent: bool,
    /// Print the byte-free plan instead of exporting.
    pub plan_only: bool,
    /// Emit JSON.
    pub json: bool,
}

/// One applied finding in the JSON report.
#[derive(Serialize)]
struct FindingRow {
    /// `info` or `warning`.
    severity: String,
    /// Stable machine code.
    code: String,
    /// Actionable message.
    message: String,
    /// Object or header location.
    location: String,
}

/// JSON shape of a completed export.
#[derive(Serialize)]
struct ExportReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Canonical target label.
    target: String,
    /// Source header version.
    source_version: String,
    /// Output header version.
    output_version: String,
    /// Always `false`: this exporter never certifies conformance.
    certification_claimed: bool,
    /// Constraints the exporter enforced.
    enforced: Vec<String>,
    /// Constraints reported but not enforced.
    reported_only: Vec<String>,
    /// Forbidden catalog entries found in the source, sorted.
    present_forbidden_entries: Vec<String>,
    /// Applied normalizations and warnings.
    findings: Vec<FindingRow>,
    /// Stable scope statement.
    scope_note: String,
    /// Pages whose text was compared with the source.
    pages_verified: usize,
}

/// One plan step in the JSON report.
#[derive(Serialize)]
struct PlanStep {
    /// Stable machine code.
    code: String,
    /// Deterministic detail.
    detail: String,
    /// Whether the local exporter performs it.
    enforced: bool,
}

/// JSON shape of the byte-free plan.
#[derive(Serialize)]
struct PlanReport {
    /// Input file.
    file: String,
    /// Canonical target label.
    target: String,
    /// Minimum header version.
    minimum_version: String,
    /// Plan steps in canonical order.
    steps: Vec<PlanStep>,
    /// Constraints reported but not enforced.
    reported_only: Vec<String>,
    /// Always `false`.
    certification_claimed: bool,
    /// Stable scope statement.
    scope_note: String,
}

/// Parse `report` or `reject`.
fn parse_policy(text: &str) -> anyhow::Result<ForbiddenEntryPolicy> {
    match text.to_ascii_lowercase().as_str() {
        "report" => Ok(ForbiddenEntryPolicy::Report),
        "reject" => Ok(ForbiddenEntryPolicy::Reject),
        other => bail!("unknown --policy {other:?}; expected report or reject"),
    }
}

/// Resolve a profile label, listing the accepted labels on failure.
///
/// Besides the canonical labels (`PDF/A-2b`, `PDF/X-4`), a compact lowercase
/// spelling such as `pdfa-2b` or `pdfx4` is accepted: separators are ignored and
/// ASCII case does not matter.
fn resolve_target(profile: &str) -> anyhow::Result<ConformanceTarget> {
    let normalize = |text: &str| {
        text.chars()
            .filter(|character| character.is_ascii_alphanumeric())
            .map(|character| character.to_ascii_lowercase())
            .collect::<String>()
    };
    let wanted = normalize(profile);
    ConformanceTarget::from_label(profile)
        .or_else(|| {
            ConformanceTarget::all()
                .iter()
                .copied()
                .find(|target| normalize(target.label()) == wanted)
        })
        .ok_or_else(|| {
            let labels: Vec<&str> = ConformanceTarget::all()
                .iter()
                .map(|target| target.label())
                .collect();
            anyhow::anyhow!(
                "unsupported --profile {profile:?}; expected one of {}",
                labels.join(", ")
            )
        })
}

/// Compare every page's text and check the required catalog entries.
fn verify_output(
    source: &Document,
    saved: &Document,
    target: ConformanceTarget,
) -> anyhow::Result<usize> {
    let pages = source
        .pages()
        .map_err(|error| anyhow::anyhow!("count source pages: {error}"))?
        .len();
    let saved_pages = saved
        .pages()
        .map_err(|error| anyhow::anyhow!("count normalized pages: {error}"))?
        .len();
    if pages != saved_pages {
        bail!("page count changed from {pages} to {saved_pages}");
    }
    for index in 0..pages {
        let before = source
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("read source page {} text: {error}", index + 1))?;
        let after = saved.page_text(index).map_err(|error| {
            anyhow::anyhow!("read page {} text after normalization: {error}", index + 1)
        })?;
        if before != after {
            bail!("page {} text changed during normalization", index + 1);
        }
    }

    let minimum = target.minimum_version();
    let saved_version = saved.version();
    let (major, minor) = parse_version(saved_version)
        .with_context(|| format!("normalized header version {saved_version:?} is not a version"))?;
    if (major, minor) < minimum {
        bail!(
            "normalized header {saved_version} is below {} minimum {}.{}",
            target.label(),
            minimum.0,
            minimum.1
        );
    }

    let catalog_id = saved
        .root()
        .map_err(|error| anyhow::anyhow!("read normalized catalog: {error}"))?;
    let catalog_object = saved
        .object(catalog_id)
        .map_err(|error| anyhow::anyhow!("load normalized catalog: {error}"))?;
    let catalog = catalog_object
        .as_dictionary()
        .context("normalized catalog is not a dictionary")?;
    if target.requires_xmp() && !catalog.contains_key("Metadata") {
        bail!(
            "{} requires /Metadata, but the normalized catalog has none",
            target.label()
        );
    }
    if target.output_intent_subtype().is_some() && !catalog.contains_key("OutputIntents") {
        bail!(
            "{} requires /OutputIntents, but the normalized catalog has none",
            target.label()
        );
    }
    Ok(pages)
}

/// Split a `major.minor` header version.
fn parse_version(text: &str) -> Option<(u8, u8)> {
    let (major, minor) = text.split_once('.')?;
    Some((major.parse().ok()?, minor.parse().ok()?))
}

/// `pdfxi export conformance`.
pub(crate) fn run_conformance(request: ExportRequest<'_>) -> anyhow::Result<()> {
    let target = resolve_target(request.profile)?;
    let policy = parse_policy(request.policy)?;
    let mut conformance =
        ConformanceRequest::new(target.label()).with_forbidden_entry_policy(policy);
    if let Some(subtype) = request.output_intent {
        conformance = conformance.with_output_intent_subtype(subtype);
    }

    if request.plan_only {
        let plan = plan_conformance_export(&conformance)
            .map_err(|error| anyhow::anyhow!("build conformance plan: {error}"))?;
        if request.json {
            let report = PlanReport {
                file: request.input.display().to_string(),
                target: plan.target().label().to_owned(),
                minimum_version: plan.minimum_version().to_owned(),
                steps: plan
                    .steps()
                    .iter()
                    .map(|step| PlanStep {
                        code: step.code().to_owned(),
                        detail: step.detail().to_owned(),
                        enforced: step.is_enforced(),
                    })
                    .collect(),
                reported_only: plan
                    .reported_only()
                    .iter()
                    .map(|item| (*item).to_owned())
                    .collect(),
                certification_claimed: false,
                scope_note: pdfxi_convert::conformance::SCOPE_NOTE.to_owned(),
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        } else {
            println!("file: {}", request.input.display());
            for line in plan.to_lines() {
                println!("{line}");
            }
            println!("{}", plan.summary());
            println!("certification_claimed: false");
        }
        return Ok(());
    }

    let out = request
        .out
        .context("--out is required unless --plan is used")?;
    let source_bytes = std::fs::read(request.input)
        .with_context(|| format!("read {}", request.input.display()))?;
    let options = NormalizationOptions {
        normalize_version: request.normalize_version,
        add_xmp_metadata: request.add_xmp_metadata,
        add_output_intent: request.add_output_intent,
    };
    let export = export_conformance_pdf(&source_bytes, &conformance, &options)
        .map_err(|error| anyhow::anyhow!("normalize {}: {error}", request.input.display()))?;

    let source = Document::from_bytes(source_bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", request.input.display()))?;
    let saved = Document::from_bytes(export.bytes.clone())
        .map_err(|error| anyhow::anyhow!("reopen normalized output: {error}"))?;
    let pages_verified = verify_output(&source, &saved, target)?;

    std::fs::write(out, &export.bytes).with_context(|| format!("write {}", out.display()))?;

    let report = ExportReport {
        file: request.input.display().to_string(),
        out: out.display().to_string(),
        target: export.report.target().label().to_owned(),
        source_version: export.report.source_version().to_owned(),
        output_version: export.report.output_version().to_owned(),
        certification_claimed: export.report.certification_claimed(),
        enforced: export
            .report
            .enforced()
            .iter()
            .map(|item| (*item).to_owned())
            .collect(),
        reported_only: export
            .report
            .reported_only()
            .iter()
            .map(|item| (*item).to_owned())
            .collect(),
        present_forbidden_entries: export.report.present_forbidden_entries().to_vec(),
        findings: export
            .report
            .applied()
            .iter()
            .map(|finding| FindingRow {
                severity: finding.severity().to_string(),
                code: finding.code().to_owned(),
                message: finding.message().to_owned(),
                location: finding.location().to_owned(),
            })
            .collect(),
        scope_note: export.report.scope_note().to_owned(),
        pages_verified,
    };

    if request.json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("target: {}", report.target);
    println!(
        "version: {} -> {}",
        report.source_version, report.output_version
    );
    println!("pages_verified: {}", report.pages_verified);
    println!("certification_claimed: false");
    println!("enforced: {}", report.enforced.join(", "));
    println!("reported_only: {}", report.reported_only.join(", "));
    for entry in &report.present_forbidden_entries {
        println!("forbidden_entry: {entry}");
    }
    for finding in &report.findings {
        println!(
            "[{}] {}  {}  ({})",
            finding.severity, finding.code, finding.message, finding.location
        );
    }
    println!("scope: {}", report.scope_note);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{parse_policy, parse_version, resolve_target};
    use pdfxi_convert::{ConformanceTarget, ForbiddenEntryPolicy};

    #[test]
    fn policies_parse_and_reject_unknown_names() {
        assert_eq!(
            parse_policy("Reject").expect("reject"),
            ForbiddenEntryPolicy::Reject
        );
        assert_eq!(
            parse_policy("report").expect("report"),
            ForbiddenEntryPolicy::Report
        );
        assert!(parse_policy("ignore").is_err());
    }

    #[test]
    fn target_labels_resolve_case_insensitively() {
        assert_eq!(
            resolve_target("pdfa-2b").expect("pdfa-2b"),
            ConformanceTarget::PdfA2b
        );
        assert_eq!(
            resolve_target("PDFX-4").expect("pdfx-4"),
            ConformanceTarget::PdfX4
        );
        assert_eq!(
            resolve_target("pdfe-1").expect("pdfe-1"),
            ConformanceTarget::PdfE1
        );
        let error = resolve_target("pdfa-9").expect_err("unknown profile");
        assert!(error.to_string().contains("PDF/A-1b"), "{error}");
    }

    #[test]
    fn header_versions_parse() {
        assert_eq!(parse_version("1.7"), Some((1, 7)));
        assert_eq!(parse_version("1.4"), Some((1, 4)));
        assert_eq!(parse_version("x.y"), None);
        assert_eq!(parse_version("1"), None);
    }
}
