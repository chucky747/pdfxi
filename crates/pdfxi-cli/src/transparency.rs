//! `pdfxi transparency inspect`: read blend modes and transparency groups.
//!
//! [`pdfxi_core::transparency`] models the 16 standard blend modes, group
//! isolation/knockout and validated blend specs. This host reads the real
//! `/Group` and `/ExtGState` dictionaries of a PDF, validates every mode and
//! alpha with that model, and reports the deterministic descriptors. Pixel
//! compositing stays a render-backend responsibility; this command inspects and
//! plans.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::transparency::{
    BlendMode, BlendSpec, GroupIsolation, GroupKnockout, TransparencyGroup,
};
use pdfxi_core::{Dictionary, Document, Object};
use serde::Serialize;

/// One transparency finding.
#[derive(Serialize)]
struct TransparencyRow {
    /// One-based page.
    page: usize,
    /// `group` or `ExtGState /NAME`.
    source: String,
    /// Blend mode name.
    mode: String,
    /// Group isolation, for page groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    isolation: Option<String>,
    /// Group knockout, for page groups.
    #[serde(skip_serializing_if = "Option::is_none")]
    knockout: Option<String>,
    /// Source alpha step count, for graphics states.
    #[serde(skip_serializing_if = "Option::is_none")]
    source_alpha: Option<u16>,
    /// Backdrop alpha step count, for graphics states.
    #[serde(skip_serializing_if = "Option::is_none")]
    backdrop_alpha: Option<u16>,
    /// Canonical blend descriptor.
    descriptor: String,
}

/// JSON shape of `pdfxi transparency inspect`.
#[derive(Serialize)]
struct TransparencyReport {
    /// Inspected PDF.
    file: String,
    /// Pages inspected.
    pages: usize,
    /// Page groups.
    groups: usize,
    /// Graphics states with a blend mode or alpha.
    states: usize,
    /// Distinct blend mode names.
    modes: Vec<String>,
    /// Findings in page order.
    rows: Vec<TransparencyRow>,
}

/// Resolve one reference chain to an owned object.
fn resolve(document: &Document, object: &Object) -> anyhow::Result<Object> {
    let mut current = object.clone();
    for _ in 0..32 {
        match current {
            Object::Reference(id) => {
                current = document
                    .object(id)
                    .with_context(|| format!("read object {} {}", id.number, id.generation))?;
            }
            other => return Ok(other),
        }
    }
    bail!("reference chain exceeds 32 objects")
}

/// Read a dictionary entry after resolving references.
fn entry(
    document: &Document,
    dictionary: &Dictionary,
    key: &str,
) -> anyhow::Result<Option<Object>> {
    match dictionary.get(key) {
        Some(value) => Ok(Some(resolve(document, value)?)),
        None => Ok(None),
    }
}

/// Read a boolean entry with a default.
fn boolean(document: &Document, dictionary: &Dictionary, key: &str, default: bool) -> bool {
    match entry(document, dictionary, key) {
        Ok(Some(Object::Boolean(value))) => value,
        _ => default,
    }
}

/// Read a number entry with a default.
fn number(document: &Document, dictionary: &Dictionary, key: &str, default: f64) -> f64 {
    match entry(document, dictionary, key) {
        Ok(Some(Object::Integer(value))) => value as f64,
        Ok(Some(Object::Real(value))) => value,
        _ => default,
    }
}

/// Parse a `/BM` value: a name or the first name of an array.
fn blend_mode(document: &Document, value: &Object) -> anyhow::Result<BlendMode> {
    let value = resolve(document, value)?;
    let token = match &value {
        Object::Name(name) => name.as_str(),
        Object::Array(items) => items.first().and_then(Object::as_name).unwrap_or("Normal"),
        _ => "Normal",
    };
    BlendMode::parse(token).map_err(|error| anyhow::anyhow!("blend mode {token:?}: {error}"))
}
/// Convert an alpha in 0..=1 to the model's step count.
fn alpha(value: f64) -> u16 {
    let clamped = value.clamp(0.0, 1.0);
    (clamped * f64::from(pdfxi_core::transparency::MAX_ALPHA)).round() as u16
}

/// `pdfxi transparency inspect`.
pub(crate) fn inspect(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .pages()
        .map_err(|error| anyhow::anyhow!("read page tree: {error}"))?;
    let mut rows = Vec::new();
    let mut groups = 0usize;
    let mut states = 0usize;
    let mut modes: Vec<String> = Vec::new();
    for (index, page) in pages.iter().enumerate() {
        let page_object = document
            .object(page.id)
            .map_err(|error| anyhow::anyhow!("read page {}: {error}", index + 1))?;
        let page_dictionary = page_object
            .as_dictionary()
            .context("page is not a dictionary")?;

        let group = entry(&document, page_dictionary, "Group")?
            .and_then(|value| value.as_dictionary().cloned());
        if let Some(group_dictionary) = group {
            let subtype = group_dictionary.get("S").and_then(Object::as_name);
            if subtype == Some("Transparency") {
                let mode = match group_dictionary.get("BM") {
                    Some(value) => blend_mode(&document, value)?,
                    None => BlendMode::Normal,
                };
                let isolation = if boolean(&document, &group_dictionary, "I", false) {
                    GroupIsolation::Isolated
                } else {
                    GroupIsolation::NonIsolated
                };
                let knockout = if boolean(&document, &group_dictionary, "K", false) {
                    GroupKnockout::Knockout
                } else {
                    GroupKnockout::NonKnockout
                };
                let model = TransparencyGroup::new(isolation, knockout, mode);
                groups += 1;
                if !modes.contains(&mode.name().to_owned()) {
                    modes.push(mode.name().to_owned());
                }
                rows.push(TransparencyRow {
                    page: index + 1,
                    source: "group".to_owned(),
                    mode: mode.name().to_owned(),
                    isolation: Some(isolation.token().to_owned()),
                    knockout: Some(knockout.token().to_owned()),
                    source_alpha: None,
                    backdrop_alpha: None,
                    descriptor: model.to_token(),
                });
            }
        }

        let Some(resources) = entry(&document, page_dictionary, "Resources")?
            .and_then(|value| value.as_dictionary().cloned())
        else {
            continue;
        };
        let Some(states_dictionary) = entry(&document, &resources, "ExtGState")?
            .and_then(|value| value.as_dictionary().cloned())
        else {
            continue;
        };
        let mut names: Vec<&String> = states_dictionary.keys().collect();
        names.sort();
        for name in names {
            let Some(state) = entry(&document, &states_dictionary, name)?
                .and_then(|value| value.as_dictionary().cloned())
            else {
                continue;
            };
            let has_mode = state.contains_key("BM");
            let has_alpha = state.contains_key("ca") || state.contains_key("CA");
            if !has_mode && !has_alpha {
                continue;
            }
            let mode = match state.get("BM") {
                Some(value) => blend_mode(&document, value)?,
                None => BlendMode::Normal,
            };
            let source_alpha = alpha(number(&document, &state, "ca", 1.0));
            let backdrop_alpha = alpha(number(&document, &state, "CA", 1.0));
            let spec = BlendSpec::new(mode, source_alpha, backdrop_alpha)
                .map_err(|error| anyhow::anyhow!("ExtGState /{name}: {error}"))?;
            states += 1;
            if !modes.contains(&mode.name().to_owned()) {
                modes.push(mode.name().to_owned());
            }
            rows.push(TransparencyRow {
                page: index + 1,
                source: format!("ExtGState /{name}"),
                mode: mode.name().to_owned(),
                isolation: None,
                knockout: None,
                source_alpha: Some(source_alpha),
                backdrop_alpha: Some(backdrop_alpha),
                descriptor: spec.to_descriptor(),
            });
        }
    }
    modes.sort();
    let report = TransparencyReport {
        file: file.display().to_string(),
        pages: pages.len(),
        groups,
        states,
        modes,
        rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!(
        "pages: {}  groups: {}  states: {}  modes: {}",
        report.pages,
        report.groups,
        report.states,
        if report.modes.is_empty() {
            "none".to_owned()
        } else {
            report.modes.join(",")
        }
    );
    for row in &report.rows {
        println!(
            "page {}  {}  {}  {}",
            row.page, row.source, row.mode, row.descriptor
        );
    }
    Ok(())
}
