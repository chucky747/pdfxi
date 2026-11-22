//! `create` command group: build new PDFs from images and text/Markdown.
//!
//! These commands are the CLI surface of `pdfxi-convert::create`. They share
//! the crate's deterministic writer, typed errors, and documented DPI/page-fit
//! rules. Every command writes the finished PDF atomically to the requested
//! output path and prints one deterministic summary line.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_convert::{
    CreateImageFormat, CreateOptions, Margins, PageSize, PsContent, PsPdfOptions,
    create_from_image, create_from_markdown, create_from_text, ingest_postscript,
    ingest_postscript_strict, write_postscript_pdf,
};
use pdfxi_core::Document;
use serde::Serialize;

/// Page-size choices accepted by the `create` commands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PageSizeChoice {
    /// US Letter, 612 by 792 points.
    Letter,
    /// ISO A4, approximately 595.28 by 841.89 points.
    A4,
}

/// Shared geometry and typography settings for a creation command.
#[derive(Debug, Clone, Copy)]
pub struct CreateSettings {
    /// Selected page size.
    pub page_size: PageSizeChoice,
    /// Uniform margin on every side, in points.
    pub margin_pt: f64,
    /// Body font size in points.
    pub font_size_pt: f64,
    /// Line-height multiplier.
    pub line_height: f64,
}

impl CreateSettings {
    /// Convert CLI settings into a [`CreateOptions`] value.
    ///
    /// Margin geometry is validated here with context. Semantic validation of
    /// the content area and typography happens inside each creation function.
    pub fn to_options(self) -> anyhow::Result<CreateOptions> {
        let page_size = match self.page_size {
            PageSizeChoice::Letter => PageSize::LETTER,
            PageSizeChoice::A4 => PageSize::A4,
        };
        let margins = Margins::uniform(self.margin_pt).context("invalid --margin")?;
        Ok(CreateOptions::new()
            .with_page_size(page_size)
            .with_margins(margins)
            .with_font_size(self.font_size_pt)
            .with_line_height(self.line_height))
    }
}

/// Create a one-page PDF that embeds a PNG or JPEG image.
///
/// The input extension selects the embedded format. PNG must be non-interlaced
/// 8-bit grayscale or RGB (no alpha or palette); JPEG must be 8-bit with one or
/// three components. Unsupported or malformed images return a typed error from
/// the library, surfaced here with path context.
pub fn create_image(input: &Path, output: &Path, settings: CreateSettings) -> anyhow::Result<()> {
    let options = settings.to_options()?;
    let extension = input
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let format = CreateImageFormat::from_extension(&extension).ok_or_else(|| {
        anyhow::anyhow!(
            "unsupported image extension '{extension}' for 'create image': expected png, jpg or jpeg"
        )
    })?;
    let bytes = std::fs::read(input)
        .with_context(|| format!("failed to read image '{}'", input.display()))?;
    let pdf = create_from_image(&bytes, format, &options)
        .with_context(|| format!("failed to create a PDF from image '{}'", input.display()))?;
    write_pdf(output, &pdf, &format!("image ({})", format.label()))
}

/// Create a paginated PDF from plain text or the documented Markdown subset.
///
/// `.md` and `.markdown` inputs (or `--markdown`) use the small Markdown parser;
/// every other extension is treated as plain UTF-8 text. The supported Markdown
/// constructs are ATX headings, paragraphs, bullet lists, and fenced code
/// blocks; other markup is emitted literally.
pub fn create_text(
    input: &Path,
    output: &Path,
    settings: CreateSettings,
    markdown: bool,
) -> anyhow::Result<()> {
    let options = settings.to_options()?;
    let bytes = std::fs::read(input)
        .with_context(|| format!("failed to read text '{}'", input.display()))?;
    let text = std::str::from_utf8(&bytes)
        .with_context(|| format!("'{}' is not valid UTF-8 text", input.display()))?;
    let extension = input
        .extension()
        .and_then(|ext| ext.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let use_markdown = markdown || matches!(extension.as_str(), "md" | "markdown");
    let pdf = if use_markdown {
        create_from_markdown(text, &options)
    } else {
        create_from_text(text, &options)
    }
    .with_context(|| format!("failed to create a PDF from text '{}'", input.display()))?;
    let kind = if use_markdown { "Markdown" } else { "text" };
    write_pdf(output, &pdf, kind)
}

/// One unsupported PostScript construct.
#[derive(Serialize)]
struct UnsupportedRow {
    /// One-based page.
    page: usize,
    /// Stable kind.
    kind: String,
    /// Token that triggered it.
    token: String,
    /// Stable explanation.
    reason: String,
}

/// JSON shape of `pdfxi create postscript`.
#[derive(Serialize)]
struct PostscriptReport {
    /// Input file.
    file: String,
    /// Written PDF.
    out: String,
    /// `PS` or `EPS`.
    kind: String,
    /// Pages written.
    pages: usize,
    /// `%%Pages`, when declared.
    declared_pages: Option<usize>,
    /// `%%BoundingBox`, when declared.
    bounding_box: Option<[i64; 4]>,
    /// Text runs captured.
    text_runs: usize,
    /// Lines captured.
    lines: usize,
    /// Rectangles captured.
    rectangles: usize,
    /// Unsupported findings.
    unsupported: Vec<UnsupportedRow>,
    /// Whether nothing was skipped.
    clean: bool,
    /// Whether strict mode was requested.
    strict: bool,
    /// Pages reopened and verified.
    verified_pages: usize,
    /// Bytes written.
    bytes: usize,
}

/// Create a PDF from the supported PostScript/EPS subset.
pub fn create_postscript(
    input: &Path,
    output: &Path,
    strict: bool,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(input)
        .with_context(|| format!("failed to read PostScript '{}'", input.display()))?;
    let ingested = if strict {
        ingest_postscript_strict(&bytes)
    } else {
        ingest_postscript(&bytes)
    }
    .with_context(|| format!("failed to ingest PostScript '{}'", input.display()))?;
    if ingested.pages.is_empty() {
        bail!("'{}' produced no pages", input.display());
    }
    let pdf = write_postscript_pdf(&ingested, &PsPdfOptions::default())
        .with_context(|| format!("failed to write PostScript '{}' as PDF", input.display()))?;
    std::fs::write(output, &pdf)
        .with_context(|| format!("failed to write '{}'", output.display()))?;

    let reopened = Document::from_bytes(pdf.clone())
        .map_err(|error| anyhow::anyhow!("reopen '{}': {error}", output.display()))?;
    let page_count = reopened
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if page_count != ingested.pages.len() {
        bail!(
            "wrote {page_count} page(s) but ingested {}",
            ingested.pages.len()
        );
    }
    for (page_index, page) in ingested.pages.iter().enumerate() {
        let page_text = reopened
            .page_text(page_index)
            .map_err(|error| anyhow::anyhow!("extract page text: {error}"))?;
        for content in &page.content {
            let PsContent::Text(text) = content else {
                continue;
            };
            if text.text.chars().any(char::is_alphanumeric)
                && !page_text.contains(text.text.as_str())
            {
                bail!(
                    "page {} text does not contain the captured run {:?}",
                    page_index + 1,
                    text.text
                );
            }
        }
    }

    let mut text_runs = 0;
    let mut lines = 0;
    let mut rectangles = 0;
    for page in &ingested.pages {
        for content in &page.content {
            match content {
                PsContent::Text(_) => text_runs += 1,
                PsContent::Line(_) => lines += 1,
                PsContent::Rect(_) => rectangles += 1,
                PsContent::Image(_) => {}
            }
        }
    }
    let report = PostscriptReport {
        file: input.display().to_string(),
        out: output.display().to_string(),
        kind: ingested.report.kind.label().to_owned(),
        pages: ingested.pages.len(),
        declared_pages: ingested.report.declared_pages,
        bounding_box: ingested
            .report
            .bounding_box
            .map(|bbox| [bbox.llx, bbox.lly, bbox.urx, bbox.ury]),
        text_runs,
        lines,
        rectangles,
        unsupported: ingested
            .report
            .unsupported
            .iter()
            .map(|finding| UnsupportedRow {
                page: finding.page,
                kind: finding.kind.to_owned(),
                token: finding.token.clone(),
                reason: finding.reason.to_owned(),
            })
            .collect(),
        clean: ingested.report.is_clean(),
        strict,
        verified_pages: page_count,
        bytes: pdf.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("kind: {}", report.kind);
    println!("pages: {}", report.pages);
    if let Some(declared) = report.declared_pages {
        println!("declared_pages: {declared}");
    }
    if let Some(bbox) = report.bounding_box {
        println!(
            "bounding_box: {} {} {} {}",
            bbox[0], bbox[1], bbox[2], bbox[3]
        );
    }
    println!(
        "content: {} text, {} lines, {} rectangles",
        report.text_runs, report.lines, report.rectangles
    );
    println!("unsupported: {}", report.unsupported.len());
    for finding in &report.unsupported {
        println!(
            "  page {}  {}  {}  {}",
            finding.page, finding.kind, finding.token, finding.reason
        );
    }
    println!("clean: {}", report.clean);
    println!("verified_pages: {}", report.verified_pages);
    println!("bytes: {}", report.bytes);
    Ok(())
}

/// Write `pdf` to `output` and print a deterministic one-line summary.
fn write_pdf(output: &Path, pdf: &[u8], kind: &str) -> anyhow::Result<()> {
    std::fs::write(output, pdf)
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    println!("wrote {} ({kind}, {} bytes)", output.display(), pdf.len());
    Ok(())
}
