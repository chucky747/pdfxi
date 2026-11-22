//! `pdfxi export postscript`: write a positioned text subset as PostScript.
//!
//! [`pdfxi_convert::postscript`] renders a caller-supplied [`PsDocument`](pdfxi_convert::PsDocument) as
//! deterministic DSC PostScript. This host maps every text run from
//! [`pdfxi_edit::text_blocks`] to a `PsText` at its real page coordinates and
//! font size, so the output preserves placement and page geometry. Vector art,
//! images and non-text content are not exported; the command reports that the
//! output is the text subset rather than pretending to be a full converter.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{PsContent, PsDocument, PsFont, PsPage, PsText, export_postscript_with_report};
use pdfxi_core::Document;
use pdfxi_edit::text_blocks;
use serde::Serialize;

/// JSON shape of `pdfxi export postscript`.
#[derive(Serialize)]
struct PostscriptReport {
    /// Source PDF.
    input: String,
    /// Written PostScript.
    out: String,
    /// Document title written in the DSC comment.
    title: String,
    /// Pages exported.
    pages: usize,
    /// Text runs exported.
    runs: usize,
    /// Bytes written.
    bytes: usize,
    /// Unsigned content classes not exported.
    omitted: Vec<String>,
}

/// `pdfxi export postscript`.
pub(crate) fn export_postscript_file(input: &Path, out: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", input.display()))?;
    let count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if count == 0 {
        bail!("{} has no pages", input.display());
    }
    let title = input
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("PDFXI PostScript export")
        .to_owned();
    let mut pages = Vec::with_capacity(count);
    let mut runs = 0usize;
    for index in 0..count {
        let frame = document
            .page_frame(index)
            .with_context(|| format!("read page {} geometry", index + 1))?;
        let width = (frame.media[2] - frame.media[0]) as f32;
        let height = (frame.media[3] - frame.media[1]) as f32;
        let model = text_blocks(&document, index)
            .map_err(|error| anyhow::anyhow!("read page {} text: {error}", index + 1))?;
        let mut content = Vec::new();
        for run in model.runs() {
            if run.text.trim().is_empty() {
                continue;
            }
            let font_size = run.font_size as f32;
            if !font_size.is_finite() || font_size <= 0.0 {
                continue;
            }
            content.push(PsContent::Text(
                PsText::new(run.text.clone(), run.start.x, run.start.y, font_size)
                    .with_font(PsFont::Helvetica),
            ));
            runs += 1;
        }
        pages.push(PsPage::new(index, width, height).with_content(content));
    }
    let model = PsDocument::new(title.clone(), pages);
    let (program, report) = export_postscript_with_report(&model)
        .map_err(|error| anyhow::anyhow!("export PostScript: {error}"))?;
    if !program.starts_with(b"%!PS-Adobe") {
        bail!("the exporter did not produce a DSC PostScript header");
    }
    std::fs::write(out, &program).with_context(|| format!("write {}", out.display()))?;

    let output = PostscriptReport {
        input: input.display().to_string(),
        out: out.display().to_string(),
        title,
        pages: report.pages,
        runs,
        bytes: program.len(),
        omitted: vec![
            "vector art".to_owned(),
            "images".to_owned(),
            "annotations".to_owned(),
        ],
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&output)?);
        return Ok(());
    }
    println!("input: {}", output.input);
    println!("out:   {}", output.out);
    println!("title: {}", output.title);
    println!(
        "pages: {}  runs: {}  bytes: {}",
        output.pages, output.runs, output.bytes
    );
    println!("omitted: {}", output.omitted.join(", "));
    Ok(())
}
