//! `pdfxi layers`: optional-content layers and their default visibility.

use std::path::Path;

use anyhow::Context;
use pdfxi_core::Document;
use serde::Serialize;

/// One layer in the JSON report.
#[derive(Serialize)]
struct LayerRow {
    /// One-based position in display order.
    index: usize,
    /// Layer name, when the OCG carries one.
    name: Option<String>,
    /// Whether the default configuration shows the layer.
    visible: bool,
    /// Nesting depth in the default `/Order` array.
    depth: usize,
}

/// JSON shape of `pdfxi layers --json`.
#[derive(Serialize)]
struct LayersReport {
    /// File that was inspected.
    file: String,
    /// Number of layers.
    count: usize,
    /// Layers in display order.
    layers: Vec<LayerRow>,
}

/// Print every optional-content layer in default display order.
pub(crate) fn run_layers(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let layers = document.layers().context("read optional-content layers")?;
    let rows = layers
        .into_iter()
        .enumerate()
        .map(|(index, layer)| LayerRow {
            index: index + 1,
            name: layer.name,
            visible: layer.visible,
            depth: layer.depth,
        })
        .collect::<Vec<_>>();
    let report = LayersReport {
        file: file.display().to_string(),
        count: rows.len(),
        layers: rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &report.layers {
            let indent = "  ".repeat(row.depth);
            let name = row.name.as_deref().unwrap_or("(unnamed)");
            println!(
                "layer: {}  {indent}{name}  visible: {}",
                row.index,
                if row.visible { "yes" } else { "no" }
            );
        }
        println!("layers: {}", report.count);
    }
    Ok(())
}
