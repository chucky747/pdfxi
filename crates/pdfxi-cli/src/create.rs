//! `create` command group: build new PDFs from images and text/Markdown.
//!
//! These commands are the CLI surface of `pdfxi-convert::create`. They share
//! the crate's deterministic writer, typed errors, and documented DPI/page-fit
//! rules. Every command writes the finished PDF atomically to the requested
//! output path and prints one deterministic summary line.

use std::path::Path;

use anyhow::Context;
use pdfxi_convert::{
    CreateImageFormat, CreateOptions, Margins, PageSize, create_from_image, create_from_markdown,
    create_from_text,
};

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

/// Write `pdf` to `output` and print a deterministic one-line summary.
fn write_pdf(output: &Path, pdf: &[u8], kind: &str) -> anyhow::Result<()> {
    std::fs::write(output, pdf)
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    println!("wrote {} ({kind}, {} bytes)", output.display(), pdf.len());
    Ok(())
}
