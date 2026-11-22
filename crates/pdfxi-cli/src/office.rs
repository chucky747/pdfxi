//! `pdfxi create office`: convert an Office intake bundle into a PDF.
//!
//! [`pdfxi_convert::office_contract`] validates a caller-supplied
//! [`OfficeBundle`](pdfxi_convert::OfficeBundle) (already-extracted Word/Excel/PowerPoint markup) and builds
//! a deterministic conversion plan, but nothing wrote a PDF. This host imports
//! the bundle into the shared [`OfficeDocument`](pdfxi_convert::OfficeDocument) model, lays each model page out
//! as text in a real PDF page of the same size, merges the pages in order, and
//! verifies the converted text by reopening the output.
//!
//! The extractor (Word/Excel/PowerPoint to markup) remains host-side; no OOXML
//! archive, Office process, or COM object is involved.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{
    CreateOptions, Margins, OfficeBlock, OfficeBundle, OfficeBundleKind, OfficeDocument,
    OfficePage, PageSize, build_office_conversion_plan, build_office_conversion_plan_strict,
    import_office_bundle_with_report,
};
use pdfxi_core::{Document, PagePlan};
use serde::{Deserialize, Serialize};

/// Margin used when laying an Office page out as text, in PDF points.
const OFFICE_PAGE_MARGIN: f64 = 36.0;

/// One bundle manifest.
#[derive(Debug, Deserialize)]
struct BundleFile {
    /// Source family: `word`, `excel`, or `powerpoint` (extensions accepted).
    kind: String,
    /// Document title.
    title: String,
    /// Already-extracted HTML/XML markup.
    markup: String,
}

/// JSON shape of `pdfxi create office`.
#[derive(Serialize)]
struct OfficePdfReport {
    /// Source manifest.
    bundle: String,
    /// Destination PDF.
    out: String,
    /// Declared source family.
    kind: String,
    /// Document title.
    title: String,
    /// Canonical adapter target selected by the plan.
    target: String,
    /// Deterministic output part name from the plan.
    output_name: String,
    /// Plan summary line.
    summary: String,
    /// Model pages.
    model_pages: usize,
    /// Model blocks.
    model_blocks: usize,
    /// Typed unsupported tokens.
    unsupported: Vec<String>,
    /// Whether strict mode was requested.
    strict: bool,
    /// Pages in the written PDF.
    pdf_pages: usize,
    /// Non-empty model text lines expected in the output.
    expected_lines: usize,
    /// Expected lines found by text extraction.
    verified_lines: usize,
    /// Extracted characters.
    extracted_chars: usize,
    /// Bytes written.
    bytes: usize,
}

/// Map a manifest kind to the bundle family.
fn bundle_kind(text: &str) -> anyhow::Result<OfficeBundleKind> {
    OfficeBundleKind::from_name(text).ok_or_else(|| {
        anyhow::anyhow!(
            "unknown Office kind {text:?}; expected word, excel or powerpoint (or a known extension)"
        )
    })
}

/// Render one table as a line with ` | ` between cells.
fn table_line(cells: &[pdfxi_convert::OfficeCell]) -> String {
    cells
        .iter()
        .map(|cell| match &cell.value {
            pdfxi_convert::CellValue::Text(text) => text.clone(),
            pdfxi_convert::CellValue::Number(value) => value.to_string(),
            pdfxi_convert::CellValue::Boolean(value) => value.to_string(),
            pdfxi_convert::CellValue::Empty => String::new(),
        })
        .collect::<Vec<String>>()
        .join(" | ")
}

/// Flatten one Office page's blocks into plain text lines in reading order.
fn page_lines(page: &OfficePage) -> Vec<String> {
    let mut lines = Vec::new();
    for block in &page.blocks {
        match block {
            OfficeBlock::Paragraph { text, .. } => lines.extend(text.lines().map(str::to_owned)),
            OfficeBlock::ListItem {
                text,
                level,
                ordered,
            } => {
                let indent = "  ".repeat(usize::from(*level));
                let marker = if *ordered { "1." } else { "-" };
                for line in text.lines() {
                    lines.push(format!("{indent}{marker} {line}"));
                }
            }
            OfficeBlock::Table(table) => {
                lines.extend(table.rows.iter().map(|row| table_line(row)));
            }
            OfficeBlock::TextBox(text_box) => {
                lines.extend(text_box.text.lines().map(str::to_owned));
            }
            OfficeBlock::Unsupported(_) => {}
        }
    }
    lines
}

/// Build a one-or-more-page PDF for one model page.
fn page_pdf(page: &OfficePage) -> anyhow::Result<Vec<u8>> {
    let mut lines = page_lines(page);
    if lines.iter().all(|line| line.trim().is_empty()) {
        // A non-breaking space keeps an empty source page a real blank page; the
        // text creator rejects whitespace-only input.
        lines = vec!["\u{00a0}".to_owned()];
    }
    let text = lines.join("\n");
    let size = PageSize::new(f64::from(page.width), f64::from(page.height))?;
    let options = CreateOptions::new()
        .with_page_size(size)
        .with_margins(Margins::uniform(OFFICE_PAGE_MARGIN)?);
    pdfxi_convert::create_from_text(&text, &options)
        .map_err(|error| anyhow::anyhow!("lay out page {}: {error}", page.index + 1))
}

/// Collapse runs of whitespace so layout and extraction compare reliably.
fn normalize(line: &str) -> String {
    line.split_whitespace().collect::<Vec<&str>>().join(" ")
}

/// Collect every non-blank expected line for verification.
fn expected_lines(document: &OfficeDocument) -> Vec<String> {
    document
        .pages
        .iter()
        .flat_map(page_lines)
        .map(|line| normalize(&line))
        .filter(|line| !line.is_empty())
        .collect()
}

/// `pdfxi create office`.
pub(crate) fn create_office(
    manifest: &Path,
    out: &Path,
    strict: bool,
    json: bool,
) -> anyhow::Result<()> {
    let manifest_text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read bundle {}", manifest.display()))?;
    let file: BundleFile = serde_json::from_str(&manifest_text)
        .with_context(|| format!("parse bundle {}", manifest.display()))?;
    let kind = bundle_kind(&file.kind)?;
    let bundle = OfficeBundle::new(kind, file.title.clone(), file.markup);
    let plan = if strict {
        build_office_conversion_plan_strict(&bundle)
    } else {
        build_office_conversion_plan(&bundle)
    }
    .map_err(|error| anyhow::anyhow!("office conversion: {error}"))?;
    let imported = import_office_bundle_with_report(&bundle)
        .map_err(|error| anyhow::anyhow!("office import: {error}"))?;
    let document = imported.document;
    if document.pages.is_empty() {
        bail!("Office bundle produced no pages");
    }

    let mut part_pdfs = Vec::with_capacity(document.pages.len());
    for page in &document.pages {
        part_pdfs.push(page_pdf(page)?);
    }
    let mut parts = part_pdfs.into_iter();
    let primary_pdf = parts.next().context("Office bundle produced no pages")?;
    let primary = Document::from_bytes(primary_pdf)
        .map_err(|error| anyhow::anyhow!("parse first Office page: {error}"))?;
    let extras = parts
        .map(|bytes| {
            Document::from_bytes(bytes)
                .map_err(|error| anyhow::anyhow!("parse Office page: {error}"))
        })
        .collect::<anyhow::Result<Vec<Document>>>()?;
    let saved = if extras.is_empty() {
        primary
            .save()
            .map_err(|error| anyhow::anyhow!("save Office page: {error}"))?
    } else {
        primary
            .apply_page_plan(&PagePlan::merge(extras))
            .map_err(|error| anyhow::anyhow!("merge Office pages: {error}"))?
    };
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let pdf_pages = reopened
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let mut extracted = String::new();
    for index in 0..pdf_pages {
        let text = reopened
            .page_text(index)
            .map_err(|error| anyhow::anyhow!("extract page {} text: {error}", index + 1))?;
        extracted.push_str(&text);
        extracted.push('\n');
    }
    let lines = expected_lines(&document);
    let extracted_lines: Vec<String> = extracted.lines().map(normalize).collect();
    let verified_lines = lines
        .iter()
        .filter(|line| {
            extracted_lines
                .iter()
                .any(|found| found.contains(line.as_str()))
        })
        .count();
    if verified_lines < lines.len() {
        bail!(
            "only {verified_lines} of {} converted Office lines extract from the output",
            lines.len()
        );
    }

    let report = OfficePdfReport {
        bundle: manifest.display().to_string(),
        out: out.display().to_string(),
        kind: kind.label().to_owned(),
        title: document.title.clone(),
        target: plan.target().label().to_owned(),
        output_name: plan.output_name().to_owned(),
        summary: plan.summary().to_owned(),
        model_pages: document.pages.len(),
        model_blocks: plan.block_count(),
        unsupported: plan
            .unsupported()
            .iter()
            .map(|item| item.kind().to_owned())
            .collect(),
        strict,
        pdf_pages,
        expected_lines: lines.len(),
        verified_lines,
        extracted_chars: extracted.chars().count(),
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("bundle: {}", report.bundle);
    println!("out:    {}", report.out);
    println!("kind: {}  title: {}", report.kind, report.title);
    println!(
        "plan: target={} output={} pages={} blocks={} unsupported={}",
        report.target,
        report.output_name,
        report.model_pages,
        report.model_blocks,
        if report.unsupported.is_empty() {
            "none".to_owned()
        } else {
            report.unsupported.join(",")
        }
    );
    println!("summary: {}", report.summary);
    println!(
        "pdf_pages: {}  verified_lines: {}/{}  extracted_chars: {}  bytes: {}",
        report.pdf_pages,
        report.verified_lines,
        report.expected_lines,
        report.extracted_chars,
        report.bytes
    );
    Ok(())
}
