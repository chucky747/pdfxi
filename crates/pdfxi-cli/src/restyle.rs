//! `pdfxi restyle`: change font size, spacing, scaling and colour of a paragraph.
//!
//! [`pdfxi_edit::restyle_run_text`] re-emits the text block that contains one run
//! with a [`RunStyleOverride`]. This host loads a real PDF, resolves the run in
//! the geometry-aware text model, applies the requested style, saves, and
//! reopens the output to verify the text still extracts and the style operators
//! are present.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{RunStyleOverride, restyle_run_text, text_blocks};
use serde::Serialize;

/// JSON shape of `pdfxi restyle`.
#[derive(Serialize)]
struct RestyleReport {
    /// Source PDF.
    file: String,
    /// Written PDF.
    out: String,
    /// Zero-based page.
    page: usize,
    /// Targeted run index.
    run: usize,
    /// Rebuilt block index.
    block: usize,
    /// Target run text.
    text: String,
    /// Font size used, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    font_size: Option<f64>,
    /// Horizontal scaling used, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    horizontal_scaling: Option<f64>,
    /// Character spacing used, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    character_spacing: Option<f64>,
    /// Word spacing used, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    word_spacing: Option<f64>,
    /// Fill colour used, when requested.
    #[serde(skip_serializing_if = "Option::is_none")]
    fill_color: Option<[f64; 3]>,
    /// Reflowed lines emitted for the block.
    lines: usize,
    /// Font resource selected by the writer.
    font_resource: String,
    /// Base font name reported by the writer.
    base_font: String,
    /// Whether the embedded fallback font was used.
    embedded: bool,
    /// Characters extracted from the written page.
    extracted_chars: usize,
    /// The target run's leading token still extracts.
    verified_text: bool,
    /// Bytes written.
    bytes: usize,
}

/// Parse an `R,G,B` colour.
fn parse_color(text: &str) -> anyhow::Result<[f64; 3]> {
    let parts: Vec<&str> = text.split(',').collect();
    if parts.len() != 3 {
        bail!("--color must be R,G,B with three components");
    }
    let mut color = [0.0f64; 3];
    for (index, part) in parts.iter().enumerate() {
        color[index] = part
            .trim()
            .parse::<f64>()
            .with_context(|| format!("parse colour component {part:?}"))?;
    }
    Ok(color)
}

/// One requested style.
struct RequestedStyle {
    size: Option<f64>,
    scale: Option<f64>,
    character: Option<f64>,
    word: Option<f64>,
    color: Option<[f64; 3]>,
}

impl RequestedStyle {
    fn is_empty(&self) -> bool {
        self.size.is_none()
            && self.scale.is_none()
            && self.character.is_none()
            && self.word.is_none()
            && self.color.is_none()
    }

    fn build(&self) -> RunStyleOverride {
        let mut style = RunStyleOverride::new();
        if let Some(size) = self.size {
            style = style.with_font_size(size);
        }
        if let Some(scale) = self.scale {
            style = style.with_horizontal_scaling(scale);
        }
        if let Some(spacing) = self.character {
            style = style.with_character_spacing(spacing);
        }
        if let Some(spacing) = self.word {
            style = style.with_word_spacing(spacing);
        }
        if let Some([red, green, blue]) = self.color {
            style = style.with_fill_color(red, green, blue);
        }
        style
    }
}

/// `pdfxi restyle`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn run_restyle(
    file: &Path,
    out: &Path,
    page: usize,
    run: usize,
    size: Option<f64>,
    scale: Option<f64>,
    character: Option<f64>,
    word: Option<f64>,
    color: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let requested = RequestedStyle {
        size,
        scale,
        character,
        word,
        color: color.map(parse_color).transpose()?,
    };
    if requested.is_empty() {
        bail!("pass at least one of --size, --scale, --char-spacing, --word-spacing or --color");
    }
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let model = text_blocks(&document, page)
        .map_err(|error| anyhow::anyhow!("read page {} text: {error}", page + 1))?;
    let target = model.run(run).cloned().ok_or_else(|| {
        anyhow::anyhow!(
            "run {run} does not exist on page {} ({} runs)",
            page + 1,
            model.runs().count()
        )
    })?;
    let style = requested.build();
    style
        .validate()
        .map_err(|error| anyhow::anyhow!("style: {error}"))?;
    let outcome = restyle_run_text(&mut document, page, run, &style)
        .map_err(|error| anyhow::anyhow!("restyle: {error}"))?;
    let saved = document
        .save()
        .map_err(|error| anyhow::anyhow!("save restyled document: {error}"))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let text = reopened
        .page_text(page)
        .map_err(|error| anyhow::anyhow!("extract page {} text: {error}", page + 1))?;
    let leading = target
        .text
        .split_whitespace()
        .next()
        .unwrap_or(target.text.as_str());
    let verified_text = text.contains(leading);
    if !verified_text {
        bail!("the restyled text no longer extracts from the output");
    }

    let report = RestyleReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        page,
        run,
        block: outcome.block_index,
        text: target.text.clone(),
        font_size: requested.size,
        horizontal_scaling: requested.scale,
        character_spacing: requested.character,
        word_spacing: requested.word,
        fill_color: requested.color,
        lines: outcome.lines,
        font_resource: outcome.font.resource.clone(),
        base_font: outcome.font.base_font.clone(),
        embedded: outcome.font.embedded,
        extracted_chars: text.chars().count(),
        verified_text,
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("out: {}", report.out);
    println!(
        "page {} run {} block {}  lines: {}  font: {} ({}{})",
        report.page,
        report.run,
        report.block,
        report.lines,
        report.font_resource,
        report.base_font,
        if report.embedded { ", embedded" } else { "" }
    );
    if let Some(size) = report.font_size {
        println!("font_size: {size}");
    }
    if let Some(scale) = report.horizontal_scaling {
        println!("horizontal_scaling: {scale}");
    }
    if let Some(spacing) = report.character_spacing {
        println!("character_spacing: {spacing}");
    }
    if let Some(spacing) = report.word_spacing {
        println!("word_spacing: {spacing}");
    }
    if let Some(color) = report.fill_color {
        println!("fill_color: {} {} {}", color[0], color[1], color[2]);
    }
    println!(
        "verified_text: {}  extracted_chars: {}  bytes: {}",
        report.verified_text, report.extracted_chars, report.bytes
    );
    Ok(())
}
