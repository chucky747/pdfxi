//! `pdfxi unicode analyze`: classify scripts, bidi runs and font fallback.
//!
//! [`pdfxi_core::unicode`] classifies scripts, resolves base direction, detects
//! logical-order bidi runs and builds an ordered font fallback chain from
//! caller candidates. This host runs that analysis over real PDF page text (or
//! `--text`) and prints the deterministic report. No shaping, rasterization or
//! external font backend is involved.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_core::unicode::{
    FontCandidate, Script, TextAnalysis, analyze_text, fallback_plan_from_text,
};
use serde::Serialize;

/// One bidi run row.
#[derive(Serialize)]
struct RunRow {
    /// Start byte.
    start: usize,
    /// End byte.
    end: usize,
    /// Run text.
    text: String,
    /// Script family name.
    script: String,
    /// Run direction.
    direction: String,
}

/// One fallback step row.
#[derive(Serialize)]
struct FallbackRow {
    /// Start byte.
    start: usize,
    /// End byte.
    end: usize,
    /// Script family name.
    script: String,
    /// Run direction.
    direction: String,
    /// Candidate family chain in order.
    chain: Vec<String>,
}

/// JSON shape of `pdfxi unicode analyze`.
#[derive(Serialize)]
struct UnicodeReport {
    /// Source label.
    source: String,
    /// Text bytes.
    bytes: usize,
    /// Text characters.
    characters: usize,
    /// Dominant script.
    dominant: String,
    /// Base direction.
    base_direction: String,
    /// Bidi runs in logical order.
    runs: Vec<RunRow>,
    /// Fallback plan summary, when candidates were supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback_summary: Option<String>,
    /// Fallback steps, when candidates were supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    fallback: Option<Vec<FallbackRow>>,
}

/// Parse a script family name.
fn parse_script(text: &str) -> anyhow::Result<Script> {
    match text.trim().to_ascii_lowercase().as_str() {
        "latin" => Ok(Script::Latin),
        "cjk" => Ok(Script::Cjk),
        "hebrew" => Ok(Script::Hebrew),
        "arabic" => Ok(Script::Arabic),
        "other" => Ok(Script::Other),
        other => bail!("unknown script {other:?}; expected latin, cjk, hebrew, arabic or other"),
    }
}

/// Parse `FAMILY=script1,script2` candidates.
fn parse_fonts(values: &[String]) -> anyhow::Result<Vec<FontCandidate>> {
    let mut candidates = Vec::with_capacity(values.len());
    for value in values {
        let (family, scripts) = value
            .split_once('=')
            .with_context(|| format!("font {value:?} must be FAMILY=script1,script2"))?;
        let scripts = scripts
            .split(',')
            .map(parse_script)
            .collect::<anyhow::Result<Vec<Script>>>()?;
        candidates.push(
            FontCandidate::new(family, scripts)
                .map_err(|error| anyhow::anyhow!("font {family:?}: {error}"))?,
        );
    }
    Ok(candidates)
}

/// Extract all page text, joined with newlines.
fn pdf_text(path: &Path) -> anyhow::Result<String> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))?;
    let count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let mut text = String::new();
    for index in 0..count {
        text.push_str(
            &document
                .page_text(index)
                .map_err(|error| anyhow::anyhow!("read page {}: {error}", index + 1))?,
        );
        text.push('\n');
    }
    Ok(text)
}

/// Build the report from one text.
fn report(source: String, text: &str, fonts: &[FontCandidate]) -> anyhow::Result<UnicodeReport> {
    let analysis: TextAnalysis =
        analyze_text(text, &[]).map_err(|error| anyhow::anyhow!("text analysis: {error}"))?;
    let runs = analysis
        .runs()
        .iter()
        .map(|run| RunRow {
            start: run.start(),
            end: run.end(),
            text: run.text().to_owned(),
            script: run.script().as_str().to_owned(),
            direction: run.direction().as_str().to_owned(),
        })
        .collect();
    let (fallback_summary, fallback) = if fonts.is_empty() {
        (None, None)
    } else {
        let plan = fallback_plan_from_text(text, fonts)
            .map_err(|error| anyhow::anyhow!("fallback plan: {error}"))?;
        let rows = plan
            .steps()
            .iter()
            .map(|step| FallbackRow {
                start: step.start(),
                end: step.end(),
                script: step.script().as_str().to_owned(),
                direction: step.direction().as_str().to_owned(),
                chain: step.font_chain().to_vec(),
            })
            .collect();
        (Some(plan.summary().to_owned()), Some(rows))
    };
    Ok(UnicodeReport {
        source,
        bytes: analysis.text_len(),
        characters: text.chars().count(),
        dominant: analysis.dominant_script().as_str().to_owned(),
        base_direction: analysis.base_direction().as_str().to_owned(),
        runs,
        fallback_summary,
        fallback,
    })
}

/// `pdfxi unicode analyze`.
pub(crate) fn analyze(
    file: Option<&Path>,
    text: Option<&str>,
    fonts: &[String],
    json: bool,
) -> anyhow::Result<()> {
    let candidates = parse_fonts(fonts)?;
    let (source, text) = match (file, text) {
        (Some(path), None) => (path.display().to_string(), pdf_text(path)?),
        (None, Some(text)) => ("--text".to_owned(), text.to_owned()),
        (Some(_), Some(_)) => bail!("pass either a PDF file or --text, not both"),
        (None, None) => bail!("pass a PDF file or --text"),
    };
    if text.trim().is_empty() {
        bail!("the analyzed text is empty");
    }
    let report = report(source, &text, &candidates)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("source: {}", report.source);
    println!(
        "bytes: {}  characters: {}  dominant: {}  base: {}",
        report.bytes, report.characters, report.dominant, report.base_direction
    );
    for run in &report.runs {
        println!(
            "run: {}..{}  {}  {}  {:?}",
            run.start, run.end, run.script, run.direction, run.text
        );
    }
    if let Some(summary) = &report.fallback_summary {
        println!("fallback: {summary}");
    }
    Ok(())
}
