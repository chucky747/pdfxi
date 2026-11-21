//! Page-level editing: extract, delete, move, reorder, rotate, insert, replace,
//! merge and split.
//!
//! Every command builds a [`pdfxi_core::PagePlan`], applies it to real document
//! bytes and writes a standalone PDF. Nothing here keeps the source file open or
//! mutates it; the plan model validates indices and rejects empty selections.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use pdfxi_core::{Document, PageOperation, PagePlan, RotationTarget};

/// Parse a page selection: `all`, a comma list of one-based numbers, or ranges
/// such as `2-4`, all one-based and inclusive.
pub(crate) fn parse_selection(spec: &str, page_count: usize) -> anyhow::Result<Vec<usize>> {
    let spec = spec.trim();
    if spec.eq_ignore_ascii_case("all") {
        return Ok((0..page_count).collect());
    }
    if spec.is_empty() {
        bail!("page selection must not be empty");
    }
    let mut indices = Vec::new();
    for part in spec.split(',') {
        let part = part.trim();
        if let Some((start, end)) = part.split_once('-') {
            let start: usize = start
                .trim()
                .parse()
                .with_context(|| format!("invalid page number {start:?}"))?;
            let end: usize = end
                .trim()
                .parse()
                .with_context(|| format!("invalid page number {end:?}"))?;
            if start == 0 || end < start || end > page_count {
                bail!("page range {part} is outside 1..={page_count}");
            }
            indices.extend(start - 1..end);
        } else {
            let page: usize = part
                .parse()
                .with_context(|| format!("invalid page number {part:?}"))?;
            if page == 0 || page > page_count {
                bail!("page {page} is outside 1..={page_count}");
            }
            indices.push(page - 1);
        }
    }
    Ok(indices)
}

fn load(path: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    Document::from_bytes(bytes).with_context(|| format!("parse {}", path.display()))
}

fn write(path: &Path, bytes: &[u8]) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent)
            .with_context(|| format!("create output directory {}", parent.display()))?;
    }
    std::fs::write(path, bytes).with_context(|| format!("write {}", path.display()))
}

fn apply(document: &Document, plan: &PagePlan, out: &Path) -> anyhow::Result<()> {
    let bytes = document
        .apply_page_plan(plan)
        .context("apply the page plan")?;
    write(out, &bytes)
}

/// Keep the selected pages, in the order given.
pub(crate) fn extract(input: &Path, spec: &str, out: &Path) -> anyhow::Result<()> {
    let document = load(input)?;
    let indices = parse_selection(spec, document.page_count()?)?;
    apply(&document, &PagePlan::extract(&indices)?, out)?;
    report(out)
}

/// Remove the selected pages.
pub(crate) fn delete(input: &Path, spec: &str, out: &Path) -> anyhow::Result<()> {
    let document = load(input)?;
    let count = document.page_count()?;
    let removed = parse_selection(spec, count)?;
    let keep: Vec<usize> = (0..count)
        .filter(|index| !removed.contains(index))
        .collect();
    apply(&document, &PagePlan::extract(&keep)?, out)?;
    report(out)
}

/// Reorder with an explicit permutation, one-based.
pub(crate) fn reorder(input: &Path, order: &str, out: &Path) -> anyhow::Result<()> {
    let document = load(input)?;
    let indices = parse_selection(order, document.page_count()?)?;
    apply(&document, &PagePlan::reorder(&indices)?, out)?;
    report(out)
}

/// Move one page so it lands at a new one-based position.
pub(crate) fn move_page(input: &Path, from: usize, to: usize, out: &Path) -> anyhow::Result<()> {
    let document = load(input)?;
    let count = document.page_count()?;
    if from == 0 || from > count || to == 0 || to > count {
        bail!("page positions must be inside 1..={count}");
    }
    let mut order: Vec<usize> = (0..count).collect();
    let page = order.remove(from - 1);
    order.insert(to - 1, page);
    apply(&document, &PagePlan::reorder(&order)?, out)?;
    report(out)
}

/// Rotate the selected pages by clockwise quarter turns.
pub(crate) fn rotate(
    input: &Path,
    spec: &str,
    quarter_turns: i32,
    out: &Path,
) -> anyhow::Result<()> {
    let document = load(input)?;
    let count = document.page_count()?;
    let indices = parse_selection(spec, count)?;
    let target = if indices.len() == count {
        RotationTarget::All
    } else {
        RotationTarget::Indices(indices)
    };
    apply(&document, &PagePlan::rotate(target, quarter_turns)?, out)?;
    report(out)
}

/// Concatenate documents in the order given.
pub(crate) fn merge(inputs: &[PathBuf], out: &Path) -> anyhow::Result<()> {
    let (first, rest) = inputs
        .split_first()
        .context("merge needs at least one input")?;
    if rest.is_empty() {
        bail!("merge needs at least two inputs");
    }
    let primary = load(first)?;
    let mut extras = Vec::with_capacity(rest.len());
    for path in rest {
        extras.push(load(path)?);
    }
    apply(&primary, &PagePlan::merge(extras), out)?;
    report(out)
}

/// Split into chunks of `every` pages, numbered in output order.
pub(crate) fn split(input: &Path, every: usize, out_dir: &Path) -> anyhow::Result<()> {
    let document = load(input)?;
    let count = document.page_count()?;
    if every == 0 {
        bail!("--every must be at least 1");
    }
    let chunks: Vec<Vec<usize>> = (0..count)
        .collect::<Vec<_>>()
        .chunks(every)
        .map(<[usize]>::to_vec)
        .collect();
    let plans = PagePlan::split(&chunks)?;
    let stem = input
        .file_stem()
        .and_then(|name| name.to_str())
        .unwrap_or("document");
    std::fs::create_dir_all(out_dir)
        .with_context(|| format!("create output directory {}", out_dir.display()))?;
    for (index, plan) in plans.iter().enumerate() {
        let path = out_dir.join(format!("{stem}-{:02}.pdf", index + 1));
        apply(&document, plan, &path)?;
    }
    println!("parts: {}", plans.len());
    println!("out_dir: {}", out_dir.display());
    Ok(())
}

/// Insert every page of `other` after one-based position `after` (0 = first).
pub(crate) fn insert(input: &Path, after: usize, other: &Path, out: &Path) -> anyhow::Result<()> {
    let primary = load(input)?;
    let base = primary.page_count()?;
    if after > base {
        bail!("--after must be inside 0..={base}");
    }
    let extra = load(other)?;
    let added = extra.page_count()?;
    let mut order: Vec<usize> = (0..after).collect();
    order.extend(base..base + added);
    order.extend(after..base);
    let plan = PagePlan::merge(vec![extra]).then(PageOperation::Extract(order))?;
    apply(&primary, &plan, out)?;
    report(out)
}

/// Replace one-based page `page` with one-based `source_page` of `other`.
pub(crate) fn replace(
    input: &Path,
    page: usize,
    other: &Path,
    source_page: usize,
    out: &Path,
) -> anyhow::Result<()> {
    let primary = load(input)?;
    let base = primary.page_count()?;
    if page == 0 || page > base {
        bail!("page {page} is outside 1..={base}");
    }
    let extra = load(other)?;
    let extra_count = extra.page_count()?;
    if source_page == 0 || source_page > extra_count {
        bail!("source page {source_page} is outside 1..={extra_count}");
    }
    let mut order: Vec<usize> = (0..page - 1).collect();
    order.push(base + source_page - 1);
    order.extend(page..base);
    let plan = PagePlan::merge(vec![extra]).then(PageOperation::Extract(order))?;
    apply(&primary, &plan, out)?;
    report(out)
}

fn report(out: &Path) -> anyhow::Result<()> {
    let bytes = std::fs::read(out).with_context(|| format!("read back {}", out.display()))?;
    let length = bytes.len();
    let document =
        Document::from_bytes(bytes).with_context(|| format!("parse {}", out.display()))?;
    println!("file: {}", out.display());
    println!("pages: {}", document.page_count()?);
    println!("bytes: {length}");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn selection_parses_all_lists_and_ranges() {
        assert_eq!(parse_selection("all", 4).expect("all"), vec![0, 1, 2, 3]);
        assert_eq!(parse_selection("1,3", 4).expect("list"), vec![0, 2]);
        assert_eq!(parse_selection("2-4", 4).expect("range"), vec![1, 2, 3]);
        assert_eq!(parse_selection("3,1", 3).expect("order"), vec![2, 0]);
    }

    #[test]
    fn selection_rejects_zero_and_out_of_range_pages() {
        assert!(parse_selection("0", 3).is_err());
        assert!(parse_selection("4", 3).is_err());
        assert!(parse_selection("3-2", 3).is_err());
        assert!(parse_selection("", 3).is_err());
    }
}
