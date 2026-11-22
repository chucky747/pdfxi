//! `pdfxi portfolio`: list and extract a PDF portfolio collection.
//!
//! A portfolio is a PDF whose catalog carries a `/Collection` dictionary and
//! whose `/Names /EmbeddedFiles` name tree holds the collection files. This
//! command is the headless host for the portfolio model: it prints one row per
//! file with its `/UF` folder path, and `--extract` writes the embedded bytes
//! into a directory while recreating the folders.
//!
//! Every path component is validated before it is joined onto the target
//! directory. Hostile collection data (absolute paths, `..`, drive or device
//! prefixes, control characters, alternate-stream `:` markers) is reported and
//! skipped instead of written.

use std::path::{Path, PathBuf};

use anyhow::Context;
use pdfxi_core::Document;
use serde::Serialize;

/// Longest accepted folder or file name component, in bytes.
const MAX_COMPONENT_BYTES: usize = 255;
/// Deepest accepted folder nesting, in components.
const MAX_COMPONENT_DEPTH: usize = 32;

/// One portfolio file in the JSON report.
#[derive(Serialize)]
struct PortfolioRow {
    /// Name-tree key accepted by `Document::attachment_bytes`.
    key: String,
    /// `/UF` path with `/`-separated folders.
    path: String,
    /// File name (the last path component).
    name: String,
    /// Embedded stream size in bytes, when declared.
    size: Option<u64>,
    /// Media type declared by the embedded file stream.
    mime: Option<String>,
    /// Stored `/Params /ModDate`, exactly as written.
    modified: Option<String>,
    /// Optional description.
    description: Option<String>,
    /// Absolute path written by `--extract`.
    #[serde(skip_serializing_if = "Option::is_none")]
    extracted: Option<String>,
    /// Why the entry was not extracted, when it was refused.
    #[serde(skip_serializing_if = "Option::is_none")]
    refused: Option<String>,
}

/// JSON shape of `pdfxi portfolio --json`.
#[derive(Serialize)]
struct PortfolioReport {
    /// File that was inspected.
    file: String,
    /// Whether the catalog declares a `/Collection`.
    collection: bool,
    /// Number of files in the collection.
    count: usize,
    /// Files in name-tree order.
    files: Vec<PortfolioRow>,
    /// Number of files written by `--extract`.
    #[serde(skip_serializing_if = "Option::is_none")]
    extracted: Option<usize>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Print the portfolio collection and optionally extract every file.
pub(crate) fn run_portfolio(file: &Path, json: bool, extract: Option<&Path>) -> anyhow::Result<()> {
    let document = load(file)?;
    let collection = document.is_portfolio().context("read /Collection")?;
    let items = document.portfolio_items().context("read portfolio files")?;

    let mut rows = items
        .into_iter()
        .map(|item| PortfolioRow {
            key: item.key,
            path: item.path,
            name: item.name,
            size: item.size,
            mime: item.mime,
            modified: item.modified,
            description: item.description,
            extracted: None,
            refused: None,
        })
        .collect::<Vec<_>>();

    let mut extracted = 0_usize;
    if let Some(directory) = extract {
        std::fs::create_dir_all(directory)
            .with_context(|| format!("create extraction directory {}", directory.display()))?;
        for row in &mut rows {
            let relative = match safe_relative_path(&row.path) {
                Ok(relative) => relative,
                Err(reason) => {
                    row.refused = Some(reason);
                    continue;
                }
            };
            let target = directory.join(&relative);
            if !is_inside(directory, &target) {
                row.refused = Some("path leaves the extraction directory".to_owned());
                continue;
            }
            if target.exists() {
                row.refused = Some("a different file already uses this path".to_owned());
                continue;
            }
            let bytes = match document.attachment_bytes(&row.key) {
                Ok(bytes) => bytes,
                Err(error) => {
                    row.refused = Some(format!("stream unavailable: {error}"));
                    continue;
                }
            };
            if let Some(parent) = target.parent()
                && let Err(error) = std::fs::create_dir_all(parent)
            {
                row.refused = Some(format!("create {}: {error}", parent.display()));
                continue;
            }
            match std::fs::write(&target, &bytes) {
                Ok(()) => {
                    row.extracted = Some(target.display().to_string());
                    extracted += 1;
                }
                Err(error) => row.refused = Some(format!("write {}: {error}", target.display())),
            }
        }
    }

    let report = PortfolioReport {
        file: file.display().to_string(),
        collection,
        count: rows.len(),
        files: rows,
        extracted: extract.map(|_| extracted),
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }

    println!(
        "collection: {}",
        if report.collection { "yes" } else { "no" }
    );
    for row in &report.files {
        let depth = row.path.matches('/').count();
        let indent = "  ".repeat(depth);
        let size = row
            .size
            .map_or_else(|| "?".to_owned(), |size| size.to_string());
        let mime = row.mime.as_deref().unwrap_or("application/octet-stream");
        let description = row
            .description
            .as_deref()
            .map_or_else(String::new, |text| format!("  description: {text}"));
        println!(
            "{indent}{}  size: {size}  type: {mime}{description}",
            row.name
        );
        if let Some(refused) = &row.refused {
            println!("{indent}  refused: {refused}");
        }
    }
    if let Some(count) = report.extracted {
        println!("extracted: {count} files");
    }
    println!("files: {}", report.count);
    Ok(())
}

/// Reduce a `/UF` path to a relative path that stays inside the target.
fn safe_relative_path(path: &str) -> Result<PathBuf, String> {
    if path.is_empty() {
        return Err("empty path".to_owned());
    }
    let mut relative = PathBuf::new();
    let mut depth = 0_usize;
    for component in path.split('/') {
        validate_component(component)?;
        relative.push(component);
        depth += 1;
        if depth > MAX_COMPONENT_DEPTH {
            return Err("folder nesting is too deep".to_owned());
        }
    }
    Ok(relative)
}

/// Validate one `/UF` path component.
fn validate_component(component: &str) -> Result<(), String> {
    if component.is_empty() {
        return Err("empty path component".to_owned());
    }
    if component == "." || component == ".." {
        return Err(format!("refused relative component {component:?}"));
    }
    if component.len() > MAX_COMPONENT_BYTES {
        return Err("path component is too long".to_owned());
    }
    if component.contains('\\') {
        return Err("path component contains a backslash".to_owned());
    }
    if component.chars().any(|character| {
        matches!(character, ':' | '*' | '?' | '"' | '<' | '>' | '|' | '\u{0}')
            || character.is_control()
    }) {
        return Err("path component contains a reserved or control character".to_owned());
    }
    Ok(())
}

/// Whether `candidate` stays inside `directory` by component-wise comparison.
///
/// The validator already rejects absolute components and `..`, so this is a
/// second, cheap guard against a joined result escaping the target. Components
/// are compared case-insensitively (Windows) and a `..` in either path fails
/// closed.
fn is_inside(directory: &Path, candidate: &Path) -> bool {
    let (Some(base), Some(joined)) = (path_components(directory), path_components(candidate))
    else {
        return false;
    };
    joined.len() >= base.len() && joined[..base.len()] == base[..]
}

/// Lower-cased, non-empty path components, or `None` when `..` appears.
fn path_components(path: &Path) -> Option<Vec<String>> {
    let mut parts = Vec::new();
    for part in path.to_string_lossy().split(['/', '\\']) {
        if part.is_empty() || part == "." {
            continue;
        }
        if part == ".." {
            return None;
        }
        parts.push(part.to_ascii_lowercase());
    }
    Some(parts)
}

#[cfg(test)]
mod tests {
    use super::{is_inside, safe_relative_path};

    #[test]
    fn nested_portfolio_paths_become_relative_paths() {
        assert_eq!(
            safe_relative_path("Reports/2026/q1.pdf")
                .expect("valid path")
                .to_string_lossy()
                .replace('\\', "/"),
            "Reports/2026/q1.pdf"
        );
        assert_eq!(
            safe_relative_path("notes.txt")
                .expect("valid path")
                .to_string_lossy(),
            "notes.txt"
        );
    }

    #[test]
    fn hostile_portfolio_paths_are_refused() {
        assert!(safe_relative_path("../escape.txt").is_err());
        assert!(safe_relative_path("Reports/../../escape.txt").is_err());
        assert!(safe_relative_path("/etc/passwd").is_err());
        assert!(safe_relative_path("C:/Windows/x.txt").is_err());
        assert!(safe_relative_path("Reports//q1.pdf").is_err());
        assert!(safe_relative_path("Reports/q1.pdf:stream").is_err());
        assert!(safe_relative_path("Reports/q\u{1}1.pdf").is_err());
        assert!(safe_relative_path("Reports/q1.pdf/").is_err());
        assert!(safe_relative_path("").is_err());
    }

    #[test]
    fn containment_guard_accepts_children_and_rejects_siblings() {
        let base = std::path::Path::new("out");
        assert!(is_inside(base, &base.join("a").join("b.txt")));
        assert!(is_inside(base, base));
        assert!(
            is_inside(
                std::path::Path::new("C:/Temp/Out"),
                std::path::Path::new("c:/temp/out/a.txt")
            ),
            "comparison is case-insensitive"
        );
        assert!(!is_inside(base, std::path::Path::new("out-other/a.txt")));
        assert!(!is_inside(base, std::path::Path::new("out/../escape.txt")));
    }
}
