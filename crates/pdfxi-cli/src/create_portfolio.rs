//! `pdfxi create portfolio`: assemble a PDF portfolio from real files.
//!
//! [`pdfxi_convert::portfolio_package`] turns a validated manifest and
//! caller-supplied bytes into a `/Collection` dictionary plus an
//! `/EmbeddedFiles` name tree. This host reads a JSON manifest, loads each file
//! from disk, builds the package, and reopens the output to prove every embedded
//! stream round-trips byte-for-byte.
//!
//! Entry names are the in-portfolio paths. They must be relative, traversal-free
//! and use forward slashes; `file` points at the bytes on disk and may be
//! relative to the manifest's directory.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use pdfxi_convert::{
    PACKAGE_MAX_FILE_BYTES, PACKAGE_MAX_TOTAL_BYTES, PortfolioEntry, PortfolioManifest,
    build_portfolio_package,
};
use pdfxi_core::Document;
use serde::{Deserialize, Serialize};

/// One manifest entry.
#[derive(Debug, Deserialize)]
struct EntryInput {
    /// In-portfolio path (`docs/brief.pdf`).
    name: String,
    /// Optional description shown by the portfolio.
    #[serde(default)]
    description: Option<String>,
    /// Display order; defaults to the entry's manifest index.
    #[serde(default)]
    order: Option<usize>,
    /// File on disk that supplies the bytes.
    file: String,
}

/// The portfolio manifest JSON.
#[derive(Debug, Deserialize)]
struct ManifestFile {
    /// Portfolio title.
    title: String,
    /// Files in the collection.
    entries: Vec<EntryInput>,
}

/// One verified entry in the report.
#[derive(Serialize)]
struct PortfolioBuildRow {
    /// In-portfolio path.
    name: String,
    /// Source file read from disk.
    file: String,
    /// Bytes embedded.
    bytes: usize,
    /// Description stored in the file specification.
    description: Option<String>,
}

/// JSON shape of `pdfxi create portfolio`.
#[derive(Serialize)]
struct PortfolioBuildReport {
    /// Source manifest.
    manifest: String,
    /// Written PDF.
    out: String,
    /// Portfolio title.
    title: String,
    /// Declared entries.
    entries: usize,
    /// Total embedded bytes.
    embedded_bytes: usize,
    /// Bytes written.
    pdf_bytes: usize,
    /// Whether the reopened catalog declares `/Collection`.
    collection: bool,
    /// Entries found by `portfolio_items`.
    items: usize,
    /// Entries whose embedded bytes matched the source.
    verified_files: usize,
    /// Written entries.
    files: Vec<PortfolioBuildRow>,
}

/// Resolve a manifest-relative file path.
fn resolve_file(manifest: &Path, file: &str) -> PathBuf {
    let path = Path::new(file);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        manifest
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

/// `pdfxi create portfolio`.
pub(crate) fn create_portfolio(manifest: &Path, out: &Path, json: bool) -> anyhow::Result<()> {
    let manifest_text = std::fs::read_to_string(manifest)
        .with_context(|| format!("read manifest {}", manifest.display()))?;
    let file: ManifestFile = serde_json::from_str(&manifest_text)
        .with_context(|| format!("parse manifest {}", manifest.display()))?;

    let mut entries = Vec::with_capacity(file.entries.len());
    let mut sources = Vec::with_capacity(file.entries.len());
    let mut rows = Vec::with_capacity(file.entries.len());
    let mut total = 0usize;
    for (index, entry) in file.entries.iter().enumerate() {
        let path = resolve_file(manifest, &entry.file);
        let bytes = std::fs::read(&path)
            .with_context(|| format!("read portfolio file {}", path.display()))?;
        if bytes.len() > PACKAGE_MAX_FILE_BYTES {
            bail!(
                "portfolio file {} is {} bytes; the per-file limit is {PACKAGE_MAX_FILE_BYTES}",
                path.display(),
                bytes.len()
            );
        }
        total = total
            .checked_add(bytes.len())
            .context("portfolio byte total overflows")?;
        if total > PACKAGE_MAX_TOTAL_BYTES {
            bail!("portfolio files total {total} bytes; the limit is {PACKAGE_MAX_TOTAL_BYTES}");
        }
        let description = entry.description.clone().unwrap_or_default();
        entries.push(PortfolioEntry::new(
            entry.name.clone(),
            description.clone(),
            entry.order.unwrap_or(index),
        ));
        rows.push(PortfolioBuildRow {
            name: entry.name.clone(),
            file: path.display().to_string(),
            bytes: bytes.len(),
            description: entry.description.clone(),
        });
        sources.push((entry.name.clone(), bytes));
    }

    let manifest_model = PortfolioManifest::new(file.title.clone(), entries);
    manifest_model
        .validate()
        .map_err(|error| anyhow::anyhow!("portfolio manifest: {error}"))?;
    let sources: Vec<(&str, Vec<u8>)> = sources
        .iter()
        .map(|(name, bytes)| (name.as_str(), bytes.clone()))
        .collect();
    let package = build_portfolio_package(&manifest_model, &sources)
        .map_err(|error| anyhow::anyhow!("build portfolio: {error}"))?;
    let bytes = package.to_bytes();
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(bytes.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let collection = reopened
        .is_portfolio()
        .map_err(|error| anyhow::anyhow!("read /Collection: {error}"))?;
    if !collection {
        bail!("the written portfolio has no /Collection dictionary");
    }
    let items = reopened
        .portfolio_items()
        .map_err(|error| anyhow::anyhow!("read portfolio files: {error}"))?;
    if items.len() != rows.len() {
        bail!(
            "wrote {} portfolio item(s) but the manifest declares {}",
            items.len(),
            rows.len()
        );
    }
    let mut verified_files = 0usize;
    for row in &rows {
        let found = items
            .iter()
            .find(|item| item.path == row.name || item.name == row.name)
            .with_context(|| format!("portfolio item {} is missing", row.name))?;
        let embedded = reopened
            .attachment_bytes(&found.key)
            .map_err(|error| anyhow::anyhow!("read embedded {}: {error}", row.name))?;
        let source = sources
            .iter()
            .find(|(name, _)| *name == row.name)
            .map(|(_, bytes)| bytes)
            .context("manifest source is missing")?;
        if embedded != *source {
            bail!(
                "embedded bytes for {} do not match the source file",
                row.name
            );
        }
        verified_files += 1;
    }

    let report = PortfolioBuildReport {
        manifest: manifest.display().to_string(),
        out: out.display().to_string(),
        title: manifest_model.title().to_owned(),
        entries: rows.len(),
        embedded_bytes: total,
        pdf_bytes: bytes.len(),
        collection,
        items: items.len(),
        verified_files,
        files: rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("manifest: {}", report.manifest);
    println!("out:      {}", report.out);
    println!("title: {}  entries: {}", report.title, report.entries);
    for row in &report.files {
        println!("{}  {} bytes  <- {}", row.name, row.bytes, row.file);
    }
    println!(
        "collection: {}  items: {}  verified_files: {}  embedded_bytes: {}  pdf_bytes: {}",
        report.collection,
        report.items,
        report.verified_files,
        report.embedded_bytes,
        report.pdf_bytes
    );
    Ok(())
}
