//! `pdfxi office`: plan and execute the Office integration contract.
//!
//! [`pdfxi_convert::office_integration`] validates a caller-supplied envelope and
//! returns a deterministic [`OfficeIntegrationPlan`] that reuses the existing
//! import/export adapters. This host exposes both directions:
//!
//! - `import-to-pdf`: Office-derived markup becomes a PDF.
//! - `pdf-to-office`: recovered page runs/tables become a DOCX/XLSX/PPTX archive.
//!
//! Nothing here starts Office, uses COM, or opens an OOXML archive; the caller
//! supplies the payload. Output is re-read before it is reported as verified.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{
    ExtractedDocument, ExtractedPage, ExtractedRun, ExtractedTable, OfficeEnvelopePayload,
    OfficeFamily, OfficeFlow, OfficeIntegrationEnvelope, OfficeIntegrationPlan,
    plan_office_integration,
};
use pdfxi_core::Document;
use serde::{Deserialize, Serialize};

/// One text run in the manifest.
#[derive(Debug, Deserialize)]
struct RunInput {
    /// Recovered text.
    text: String,
    /// Left edge in points.
    x: f32,
    /// Baseline in points.
    y: f32,
    /// Font size in points.
    font_size: f32,
    /// Tie-breaker for identical geometry; defaults to zero.
    #[serde(default)]
    sequence: usize,
}

/// One table in the manifest.
#[derive(Debug, Deserialize)]
struct TableInput {
    /// Optional sheet name.
    #[serde(default)]
    name: Option<String>,
    /// Rectangular rows.
    rows: Vec<Vec<String>>,
}

/// One extracted page in the manifest.
#[derive(Debug, Deserialize)]
struct PageInput {
    /// Zero-based page index; defaults to the array position.
    #[serde(default)]
    index: Option<usize>,
    /// Page width in points.
    width: f32,
    /// Page height in points.
    height: f32,
    /// Text runs.
    #[serde(default)]
    runs: Vec<RunInput>,
    /// Simple tables.
    #[serde(default)]
    tables: Vec<TableInput>,
}

/// The integration manifest.
#[derive(Debug, Deserialize)]
struct IntegrateFile {
    /// Office family name (`word`, `excel`, `powerpoint`, or extension).
    family: String,
    /// `import-to-pdf` or `pdf-to-office`.
    flow: String,
    /// Output document name (extension optional).
    name: String,
    /// Office-derived markup for `import-to-pdf`.
    #[serde(default)]
    markup: Option<String>,
    /// Extracted pages for `pdf-to-office`.
    #[serde(default)]
    pages: Vec<PageInput>,
}

/// One plan step in the JSON report.
#[derive(Serialize)]
struct StepReport {
    /// Execution order.
    order: usize,
    /// Adapter name.
    adapter: String,
    /// Action performed.
    action: String,
}

/// JSON shape of `pdfxi office plan` and `pdfxi office convert`.
#[derive(Serialize)]
struct OfficeIntegrationReport {
    /// Source manifest.
    manifest: String,
    /// Destination path, when executed.
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    /// Office family label.
    family: String,
    /// Flow label.
    flow: String,
    /// Sanitized output name including the extension.
    output_name: String,
    /// Pages the adapters report.
    page_count: usize,
    /// Blocks the adapters report.
    block_count: usize,
    /// Typed unsupported constructs.
    unsupported: Vec<String>,
    /// Ordered execution steps.
    steps: Vec<StepReport>,
    /// Whether the plan was executed.
    executed: bool,
    /// Written bytes.
    #[serde(skip_serializing_if = "Option::is_none")]
    bytes: Option<usize>,
    /// How the output was verified.
    #[serde(skip_serializing_if = "Option::is_none")]
    verified: Option<String>,
    /// Pages found when the output is a PDF.
    #[serde(skip_serializing_if = "Option::is_none")]
    pdf_pages: Option<usize>,
    /// Extracted text characters when the output is a PDF.
    #[serde(skip_serializing_if = "Option::is_none")]
    text_chars: Option<usize>,
    /// OOXML part names found in the archive when the output is Office.
    #[serde(skip_serializing_if = "Option::is_none")]
    parts: Option<Vec<String>>,
}

/// Parse the requested flow.
fn parse_flow(text: &str) -> anyhow::Result<OfficeFlow> {
    match text.to_ascii_lowercase().as_str() {
        "import-to-pdf" | "import" | "office-to-pdf" => Ok(OfficeFlow::ImportToPdf),
        "pdf-to-office" | "export" | "pdf-to-docx" => Ok(OfficeFlow::PdfToOffice),
        other => bail!("unknown flow {other:?}; expected import-to-pdf or pdf-to-office"),
    }
}

/// Build the extracted model from the manifest pages.
fn extracted_document(name: &str, pages: &[PageInput]) -> ExtractedDocument {
    let pages = pages
        .iter()
        .enumerate()
        .map(|(position, page)| {
            let runs = page
                .runs
                .iter()
                .map(|run| {
                    ExtractedRun::new(run.text.clone(), run.x, run.y, run.font_size)
                        .with_sequence(run.sequence)
                })
                .collect();
            let mut extracted =
                ExtractedPage::new(page.index.unwrap_or(position), page.width, page.height)
                    .with_runs(runs);
            for table in &page.tables {
                let mut model = ExtractedTable::new(table.rows.clone());
                if let Some(label) = &table.name {
                    model = model.with_name(label.clone());
                }
                extracted = extracted.with_table(model);
            }
            extracted
        })
        .collect();
    ExtractedDocument::new(name, pages)
}

/// Build the envelope from the manifest.
fn envelope(file: &IntegrateFile) -> anyhow::Result<OfficeIntegrationEnvelope> {
    let flow = parse_flow(&file.flow)?;
    let payload = match flow {
        OfficeFlow::ImportToPdf => {
            let markup = file
                .markup
                .clone()
                .context("import-to-pdf requires a markup string")?;
            OfficeEnvelopePayload::Markup(markup)
        }
        OfficeFlow::PdfToOffice => {
            if file.pages.is_empty() {
                bail!("pdf-to-office requires at least one page");
            }
            OfficeEnvelopePayload::Extracted(extracted_document(&file.name, &file.pages))
        }
    };
    OfficeIntegrationEnvelope::from_family_name(&file.family, flow, file.name.clone(), payload)
        .map_err(|error| anyhow::anyhow!("office envelope: {error}"))
}

/// Read the manifest and build its plan.
fn read_plan(manifest: &Path) -> anyhow::Result<(OfficeIntegrationPlan, String)> {
    let text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read manifest {}", manifest.display()))?;
    let file: IntegrateFile = serde_json::from_str(&text)
        .with_context(|| format!("parse manifest {}", manifest.display()))?;
    let envelope = envelope(&file)?;
    let plan = plan_office_integration(&envelope)
        .map_err(|error| anyhow::anyhow!("office integration plan: {error}"))?;
    Ok((plan, text))
}

/// Build the shared report skeleton.
fn base_report(manifest: &Path, plan: &OfficeIntegrationPlan) -> OfficeIntegrationReport {
    OfficeIntegrationReport {
        manifest: manifest.display().to_string(),
        out: None,
        family: plan.family().label().to_owned(),
        flow: plan.flow().label().to_owned(),
        output_name: plan.output_name().to_owned(),
        page_count: plan.page_count(),
        block_count: plan.block_count(),
        unsupported: plan
            .unsupported()
            .iter()
            .map(|item| format!("{}:{}:{}", item.stage, item.kind, item.detail))
            .collect(),
        steps: plan
            .steps()
            .iter()
            .map(|step| StepReport {
                order: step.order,
                adapter: step.adapter.to_owned(),
                action: step.action.to_owned(),
            })
            .collect(),
        executed: false,
        bytes: None,
        verified: None,
        pdf_pages: None,
        text_chars: None,
        parts: None,
    }
}

/// Print a report as text.
fn print_report(report: &OfficeIntegrationReport) {
    println!("manifest: {}", report.manifest);
    if let Some(out) = &report.out {
        println!("out:      {out}");
    }
    println!(
        "family: {}  flow: {}  output: {}",
        report.family, report.flow, report.output_name
    );
    println!(
        "pages: {}  blocks: {}  unsupported: {}",
        report.page_count,
        report.block_count,
        if report.unsupported.is_empty() {
            "none".to_owned()
        } else {
            report.unsupported.join(",")
        }
    );
    for step in &report.steps {
        println!("  {}: {} {}", step.order, step.adapter, step.action);
    }
    if report.executed {
        println!(
            "bytes: {}  verified: {}",
            report.bytes.unwrap_or(0),
            report.verified.as_deref().unwrap_or("no")
        );
        if let Some(pages) = report.pdf_pages {
            println!(
                "pdf_pages: {pages}  text_chars: {}",
                report.text_chars.unwrap_or(0)
            );
        }
        if let Some(parts) = &report.parts {
            println!("parts: {}", parts.join(", "));
        }
    }
}

/// `pdfxi office plan`.
pub(crate) fn plan_office(manifest: &Path, json: bool) -> anyhow::Result<()> {
    let (plan, _) = read_plan(manifest)?;
    let report = base_report(manifest, &plan);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_report(&report);
    Ok(())
}

/// `pdfxi office convert`.
pub(crate) fn convert_office(manifest: &Path, out: &Path, json: bool) -> anyhow::Result<()> {
    let (plan, _) = read_plan(manifest)?;
    let output = plan
        .execute()
        .map_err(|error| anyhow::anyhow!("office integration execute: {error}"))?;
    if !out
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case(output.extension))
    {
        bail!(
            "output {} must have the .{} extension produced by this plan",
            out.display(),
            output.extension
        );
    }
    std::fs::write(out, &output.bytes).with_context(|| format!("write {}", out.display()))?;

    let mut report = base_report(manifest, &plan);
    report.out = Some(out.display().to_string());
    report.executed = true;
    report.bytes = Some(output.bytes.len());
    match plan.flow() {
        OfficeFlow::ImportToPdf => {
            if !output.bytes.starts_with(b"%PDF-") {
                bail!("the executed import did not produce a PDF");
            }
            let document = Document::from_bytes(output.bytes.clone())
                .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
            let pages = document
                .page_count()
                .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
            let mut text = String::new();
            for index in 0..pages {
                text.push_str(
                    &document
                        .page_text(index)
                        .map_err(|error| anyhow::anyhow!("extract page {}: {error}", index + 1))?,
                );
                text.push('\n');
            }
            if text.trim().is_empty() {
                bail!("the executed import produced no extractable text");
            }
            report.verified = Some("pdf".to_owned());
            report.pdf_pages = Some(pages);
            report.text_chars = Some(text.chars().count());
        }
        OfficeFlow::PdfToOffice => {
            if !output.bytes.starts_with(b"PK\x03\x04") {
                bail!("the executed export did not produce an OOXML ZIP archive");
            }
            let required = match plan.family() {
                OfficeFamily::Word => b"word/document.xml".as_slice(),
                OfficeFamily::Excel => b"xl/workbook.xml".as_slice(),
                OfficeFamily::PowerPoint => b"ppt/presentation.xml".as_slice(),
            };
            if !output
                .bytes
                .windows(required.len())
                .any(|window| window == required)
            {
                bail!(
                    "the executed export is missing the required part {}",
                    String::from_utf8_lossy(required)
                );
            }
            report.verified = Some("ooxml".to_owned());
            report.parts = Some(vec![String::from_utf8_lossy(required).into_owned()]);
        }
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    print_report(&report);
    Ok(())
}
