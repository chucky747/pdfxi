//! `export` command group: export structured, accessible text from a PDF.
//!
//! The command derives a deterministic accessibility tree from the PDF's
//! extracted page text and serializes it through `pdfxi-convert`'s tagged-text
//! writer. The result is a **structured text representation** (roles, reading
//! order, text and optional alt text). It is not a tagged-PDF file and makes no
//! PDF/UA certification claim.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_accessibility::{BBox, ReadingOrderBuilder, TextBlock};
use pdfxi_convert::{TaggedExport, TaggedTextOptions, export_tagged_text};
use pdfxi_core::Document;

/// Which tagged-text representation to write.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaggedFormat {
    /// Nested, indented outline with `@role` markers.
    Structured,
    /// Flat text with `[role]` markers and optional `[alt: ...]` suffixes.
    Plain,
}

impl TaggedFormat {
    /// Return the stable lowercase name used in messages.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Structured => "structured",
            Self::Plain => "plain",
        }
    }
}

/// Synthetic page box for derived text blocks.
///
/// The core text API reports one string per page without glyph boxes, so every
/// block receives the same deterministic US-Letter box. Reading order is then
/// defined purely by ascending page index.
fn synthetic_page_box() -> anyhow::Result<BBox> {
    BBox::new(0.0, 0.0, 612.0, 792.0).context("invalid synthetic page box")
}

/// Derive deterministic text blocks from a document's per-page text.
fn text_blocks(document: &Document) -> anyhow::Result<Vec<TextBlock>> {
    let count = document
        .page_count()
        .context("failed to read the PDF page count")?;
    let page_box = synthetic_page_box()?;
    let mut blocks = Vec::with_capacity(count);
    for page in 0..count {
        let text = document
            .page_text(page)
            .with_context(|| format!("failed to extract text from page {}", page + 1))?;
        if text.trim().is_empty() {
            continue;
        }
        blocks.push(
            TextBlock::new(page, text, page_box, None)
                .with_context(|| format!("invalid text block for page {}", page + 1))?,
        );
    }
    Ok(blocks)
}

/// Export a PDF's accessible text as a structured representation.
///
/// An accessibility tree is built from extracted page text with
/// `ReadingOrderBuilder`, then serialized by `export_tagged_text`. The selected
/// representation is written to `output`. An empty result (no extractable text)
/// is a typed error rather than an empty success.
pub fn export_tagged(input: &Path, output: &Path, format: TaggedFormat) -> anyhow::Result<()> {
    let bytes =
        std::fs::read(input).with_context(|| format!("failed to read '{}'", input.display()))?;
    let document = Document::from_bytes(bytes)
        .with_context(|| format!("'{}' is not a parseable PDF", input.display()))?;
    let blocks = text_blocks(&document)?;
    let tree = ReadingOrderBuilder::new()
        .build(blocks)
        .context("failed to build an accessibility tree")?;
    let export: TaggedExport = export_tagged_text(&tree, &TaggedTextOptions::new())
        .context("tagged text export failed")?;
    let text = match format {
        TaggedFormat::Structured => &export.structured,
        TaggedFormat::Plain => &export.plain,
    };
    if text.is_empty() {
        bail!("'{}' has no extractable text to export", input.display());
    }
    std::fs::write(output, text.as_bytes())
        .with_context(|| format!("failed to write '{}'", output.display()))?;
    println!(
        "wrote {} (tagged {} text, {} bytes)",
        output.display(),
        format.label(),
        text.len()
    );
    Ok(())
}
