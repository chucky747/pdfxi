//! `pdfxi find-files`: search the text of a folder or file list.
//!
//! Reads every PDF under the supplied paths, extracts page text with
//! `pdfxi-core`, searches each page with the shared `pdfxi-text` matcher, and
//! groups matches by file and page. Files rank by total match count, with a
//! path tie-break, and global/per-file result caps report truncation.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_text::{SearchOptions, TextPage, TextSpan, search};
use serde::Serialize;

/// Maximum files scanned in one command.
const MAX_FILES: usize = 10_000;
/// Maximum directory depth.
const MAX_DEPTH: usize = 32;

/// One page match group.
#[derive(Serialize)]
struct PageRow {
    /// One-based page.
    page: usize,
    /// Matches on this page.
    matches: usize,
    /// Text around the first match.
    snippet: String,
}

/// One file result.
#[derive(Serialize)]
struct FileRow {
    /// File path.
    file: String,
    /// Total matches.
    matches: usize,
    /// Pages with matches.
    pages: Vec<PageRow>,
}

/// JSON shape of `pdfxi find-files`.
#[derive(Serialize)]
struct FindFilesReport {
    /// Search text.
    query: String,
    /// Inputs visited.
    inputs: Vec<String>,
    /// PDF files scanned.
    files_scanned: usize,
    /// Files with at least one match.
    files_matched: usize,
    /// Matches before caps.
    total_matches: usize,
    /// Matches reported after caps.
    reported_matches: usize,
    /// Whether a cap cut the result set.
    truncated: bool,
    /// Results ranked by match count then path.
    files: Vec<FileRow>,
}

/// Collect PDF candidates beneath one path.
fn collect(path: &Path, depth: usize, out: &mut Vec<PathBuf>) -> anyhow::Result<()> {
    if depth > MAX_DEPTH {
        bail!(
            "directory nesting exceeds {MAX_DEPTH} under {}",
            path.display()
        );
    }
    if out.len() >= MAX_FILES {
        bail!("more than {MAX_FILES} PDF files were found");
    }
    if path.is_file() {
        out.push(path.to_path_buf());
        return Ok(());
    }
    if !path.exists() {
        bail!("input {} does not exist", path.display());
    }
    if !path.is_dir() {
        bail!("input {} is neither a file nor a directory", path.display());
    }
    let mut entries: Vec<PathBuf> = std::fs::read_dir(path)
        .with_context(|| format!("read directory {}", path.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .collect();
    entries.sort();
    for entry in entries {
        if entry.is_dir() {
            collect(&entry, depth + 1, out)?;
        } else if entry
            .extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
        {
            out.push(entry);
        }
    }
    Ok(())
}

/// Search one file and return its page groups and total.
fn search_file(
    path: &Path,
    query: &str,
    options: &SearchOptions,
) -> anyhow::Result<Option<(usize, Vec<PageRow>)>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))?;
    let count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages of {}: {error}", path.display()))?;
    let mut total = 0usize;
    let mut pages = Vec::new();
    for index in 0..count {
        let text = document.page_text(index).map_err(|error| {
            anyhow::anyhow!("read page {} of {}: {error}", index + 1, path.display())
        })?;
        if text.is_empty() {
            continue;
        }
        let page = TextPage::new(index, vec![TextSpan::new(text.clone(), 0.0, 0.0)]);
        let found = search(&page, query, options);
        if found.is_empty() {
            continue;
        }
        let first = &found[0];
        let start = first.start().saturating_sub(40);
        let end = (first.end() + 40).min(text.len());
        let snippet = text
            .get(start..end)
            .unwrap_or_default()
            .replace(['\n', '\r'], " ");
        total += found.len();
        pages.push(PageRow {
            page: index + 1,
            matches: found.len(),
            snippet: snippet.trim().to_owned(),
        });
    }
    Ok((total > 0).then_some((total, pages)))
}

/// `pdfxi find-files`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn find_files(
    inputs: &[PathBuf],
    query: &str,
    case_sensitive: bool,
    whole_word: bool,
    max_results: Option<usize>,
    max_per_file: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    if query.trim().is_empty() {
        bail!("--query must not be empty");
    }
    if inputs.is_empty() {
        bail!("pass at least one file or directory");
    }
    let mut files = Vec::new();
    for input in inputs {
        collect(input, 0, &mut files)?;
    }
    files.sort();
    files.dedup();
    let options = SearchOptions::with_options(case_sensitive, whole_word);
    let mut rows: Vec<FileRow> = Vec::new();
    let mut files_scanned = 0usize;
    let mut total_matches = 0usize;
    for path in &files {
        files_scanned += 1;
        let Some((total, mut pages)) = search_file(path, query, &options)? else {
            continue;
        };
        if let Some(cap) = max_per_file {
            let mut kept = 0usize;
            pages.retain_mut(|page| {
                if kept >= cap {
                    return false;
                }
                let allowed = (cap - kept).min(page.matches);
                page.matches = allowed;
                kept += allowed;
                allowed > 0
            });
        }
        let reported: usize = pages.iter().map(|page| page.matches).sum();
        total_matches += total;
        rows.push(FileRow {
            file: path.display().to_string(),
            matches: reported,
            pages,
        });
    }
    rows.sort_by(|left, right| {
        right
            .matches
            .cmp(&left.matches)
            .then_with(|| left.file.cmp(&right.file))
    });
    let mut reported_matches: usize = rows.iter().map(|row| row.matches).sum();
    let mut truncated = max_per_file.is_some_and(|cap| {
        rows.iter().any(|row| row.matches == cap) && total_matches > reported_matches
    });
    if let Some(cap) = max_results
        && reported_matches > cap
    {
        let mut remaining = cap;
        for row in &mut rows {
            row.pages.retain_mut(|page| {
                if remaining == 0 {
                    return false;
                }
                let allowed = remaining.min(page.matches);
                page.matches = allowed;
                remaining -= allowed;
                allowed > 0
            });
            row.matches = row.pages.iter().map(|page| page.matches).sum();
        }
        rows.retain(|row| row.matches > 0);
        reported_matches = cap;
        truncated = true;
    }
    let files_matched = rows.len();
    let report = FindFilesReport {
        query: query.to_owned(),
        inputs: inputs
            .iter()
            .map(|path| path.display().to_string())
            .collect(),
        files_scanned,
        files_matched,
        total_matches,
        reported_matches,
        truncated,
        files: rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "query: {:?}  files_scanned: {}  files_matched: {}  matches: {}",
        report.query, report.files_scanned, report.files_matched, report.reported_matches
    );
    for row in &report.files {
        for page in &row.pages {
            println!(
                "{}\tpage {}\t{}\t{}",
                row.file, page.page, page.matches, page.snippet
            );
        }
    }
    if report.truncated {
        println!("truncated: true");
    }
    Ok(())
}
