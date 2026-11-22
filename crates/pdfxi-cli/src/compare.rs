//! `pdfxi compare`: side-by-side revision compare of two PDFs.
//!
//! Pages are aligned by their extracted text, not by page number, so a page
//! inserted or deleted anywhere in the document is reported as such instead of
//! shifting every later page into a false "changed" pair. Aligned pairs whose
//! normalised text differs are reported as `changed` with removed/added line
//! counts, and `--lines` prints the differing lines as two columns.
//!
//! The comparison is deterministic: pages and lines are compared by normalised
//! text (trimmed, empty lines dropped), the alignment is a longest-common-
//! subsequence with ties resolved towards deletions, and no hashing or locale
//! ordering is involved.
//!
//! # Limits
//!
//! Alignment is quadratic. More than [`MAX_ALIGNMENT_CELLS`] page or line pairs
//! fail with a clear error instead of allocating unbounded memory. Displayed
//! differences are capped at [`MAX_DISPLAY_LINES`] rows per page.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use serde::Serialize;

/// Largest `(left + 1) * (right + 1)` alignment matrix the command will build.
const MAX_ALIGNMENT_CELLS: usize = 4_000_000;
/// Most differing line rows printed for one changed page.
const MAX_DISPLAY_LINES: usize = 20;

/// One page of comparable content.
struct Page {
    /// One-based page number in its document.
    number: usize,
    /// Normalised lines; also the page signature when joined with `\n`.
    lines: Vec<String>,
}

impl Page {
    /// Signature used for page alignment.
    fn signature(&self) -> String {
        self.lines.join("\n")
    }
}

/// How one row of the comparison relates the two documents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
enum RowKind {
    /// Both documents have this page.
    Aligned,
    /// Only the right document has this page.
    Inserted,
    /// Only the left document has this page.
    Deleted,
}

/// One page row in the JSON report.
#[derive(Serialize)]
struct ComparePageRow {
    /// `aligned`, `inserted` or `deleted`.
    kind: RowKind,
    /// One-based page in the left document.
    #[serde(skip_serializing_if = "Option::is_none")]
    left: Option<usize>,
    /// One-based page in the right document.
    #[serde(skip_serializing_if = "Option::is_none")]
    right: Option<usize>,
    /// Whether an aligned pair's text differs.
    #[serde(skip_serializing_if = "Option::is_none")]
    changed: Option<bool>,
    /// Lines present only in the left page.
    #[serde(skip_serializing_if = "Option::is_none")]
    removed_lines: Option<usize>,
    /// Lines present only in the right page.
    #[serde(skip_serializing_if = "Option::is_none")]
    added_lines: Option<usize>,
}

/// Counts of each row kind.
#[derive(Serialize)]
struct CompareSummary {
    /// Pairs present in both documents.
    aligned: usize,
    /// Aligned pairs whose text differs.
    changed: usize,
    /// Pages only in the right document.
    inserted: usize,
    /// Pages only in the left document.
    deleted: usize,
}

/// JSON shape of `pdfxi compare --json`.
#[derive(Serialize)]
struct CompareReport {
    /// Left document path.
    left: String,
    /// Right document path.
    right: String,
    /// Left document page count.
    left_pages: usize,
    /// Right document page count.
    right_pages: usize,
    /// Whether the two documents have identical page text.
    identical: bool,
    /// Row counts.
    summary: CompareSummary,
    /// One row per page or page pair, in left-document order.
    pages: Vec<ComparePageRow>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Compare two PDFs page by page and report the differences.
///
/// `--json` emits stable fields, `--lines` prints the differing lines of each
/// changed page as two columns, and `--fail-on-difference` turns a non-identical
/// result into a non-zero exit status for scripts.
pub(crate) fn run_compare(
    left: &Path,
    right: &Path,
    json: bool,
    lines: bool,
    fail_on_difference: bool,
) -> anyhow::Result<()> {
    let left_doc = load(left)?;
    let right_doc = load(right)?;
    let left_pages = pages_of(&left_doc, left)?;
    let right_pages = pages_of(&right_doc, right)?;

    let left_signatures = left_pages.iter().map(Page::signature).collect::<Vec<_>>();
    let right_signatures = right_pages.iter().map(Page::signature).collect::<Vec<_>>();
    let alignment = align(&left_signatures, &right_signatures)?;

    let mut summary = CompareSummary {
        aligned: 0,
        changed: 0,
        inserted: 0,
        deleted: 0,
    };
    let mut rows = Vec::with_capacity(alignment.len());
    let mut detail: Vec<String> = Vec::new();
    for (left_index, right_index) in alignment {
        match (left_index, right_index) {
            (Some(l), Some(r)) => {
                summary.aligned += 1;
                let (removed, added) = line_changes(&left_pages[l].lines, &right_pages[r].lines);
                let changed = left_signatures[l] != right_signatures[r];
                if changed {
                    summary.changed += 1;
                    if lines {
                        detail.push(format!(
                            "page {} | page {}  changed  -{removed} +{added}",
                            left_pages[l].number, right_pages[r].number
                        ));
                        differing_lines(&left_pages[l].lines, &right_pages[r].lines, &mut detail);
                    }
                }
                rows.push(ComparePageRow {
                    kind: RowKind::Aligned,
                    left: Some(left_pages[l].number),
                    right: Some(right_pages[r].number),
                    changed: Some(changed),
                    removed_lines: changed.then_some(removed),
                    added_lines: changed.then_some(added),
                });
            }
            (Some(l), None) => {
                summary.deleted += 1;
                rows.push(ComparePageRow {
                    kind: RowKind::Deleted,
                    left: Some(left_pages[l].number),
                    right: None,
                    changed: None,
                    removed_lines: None,
                    added_lines: None,
                });
            }
            (None, Some(r)) => {
                summary.inserted += 1;
                rows.push(ComparePageRow {
                    kind: RowKind::Inserted,
                    left: None,
                    right: Some(right_pages[r].number),
                    changed: None,
                    removed_lines: None,
                    added_lines: None,
                });
            }
            (None, None) => {}
        }
    }
    let identical = summary.changed == 0
        && summary.inserted == 0
        && summary.deleted == 0
        && left_pages.len() == right_pages.len();

    let report = CompareReport {
        left: left.display().to_string(),
        right: right.display().to_string(),
        left_pages: left_pages.len(),
        right_pages: right_pages.len(),
        identical,
        summary,
        pages: rows,
    };

    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("left:  {} ({} pages)", report.left, report.left_pages);
        println!("right: {} ({} pages)", report.right, report.right_pages);
        for row in &report.pages {
            match row.kind {
                RowKind::Aligned => {
                    let state = match row.changed {
                        Some(true) => format!(
                            "changed  -{} +{}",
                            row.removed_lines.unwrap_or(0),
                            row.added_lines.unwrap_or(0)
                        ),
                        _ => "same".to_owned(),
                    };
                    println!(
                        "aligned  left {}  right {}  {state}",
                        row.left.unwrap_or(0),
                        row.right.unwrap_or(0)
                    );
                }
                RowKind::Inserted => println!("inserted right {}", row.right.unwrap_or(0)),
                RowKind::Deleted => println!("deleted  left {}", row.left.unwrap_or(0)),
            }
        }
        for line in &detail {
            println!("{line}");
        }
        println!(
            "summary: aligned {}, changed {}, inserted {}, deleted {}",
            report.summary.aligned,
            report.summary.changed,
            report.summary.inserted,
            report.summary.deleted
        );
        println!("identical: {}", report.identical);
    }

    if fail_on_difference && !report.identical {
        bail!("the documents differ");
    }
    Ok(())
}

/// Extract the normalised lines of every page in order.
fn pages_of(document: &Document, file: &Path) -> anyhow::Result<Vec<Page>> {
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    let mut pages = Vec::with_capacity(count);
    for index in 0..count {
        let text = document.page_text(index).with_context(|| {
            format!("extract text from page {} of {}", index + 1, file.display())
        })?;
        pages.push(Page {
            number: index + 1,
            lines: normalised_lines(&text),
        });
    }
    Ok(pages)
}

/// Trimmed, non-empty lines from extracted page text.
///
/// Empty lines and trailing whitespace carry no content, so they are dropped
/// before comparison; this keeps two pages equal when only the text layout
/// changed.
fn normalised_lines(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(str::to_owned)
        .collect()
}

/// Align two signature lists by longest common subsequence.
///
/// Returns `(left, right)` index pairs for aligned items plus `(Some, None)`
/// for deletions and `(None, Some)` for insertions, in document order. Ties in
/// the DP are resolved towards deletions first, so the output is deterministic.
///
/// A block of deletions immediately followed by a block of insertions is zipped
/// positionally into aligned pairs: two pages (or lines) that replaced each
/// other are a *changed* pair, not one deletion plus one insertion. Unpaired
/// leftovers stay deletions or insertions.
fn align(left: &[String], right: &[String]) -> anyhow::Result<Vec<(Option<usize>, Option<usize>)>> {
    let n = left.len();
    let m = right.len();
    if (n + 1).saturating_mul(m + 1) > MAX_ALIGNMENT_CELLS {
        bail!("documents are too large to align ({n} x {m} pages)");
    }
    // `lcs[i][j]` is the LCS length of `left[i..]` and `right[j..]`.
    let mut lcs = vec![vec![0usize; m + 1]; n + 1];
    for i in (0..n).rev() {
        for j in (0..m).rev() {
            lcs[i][j] = if left[i] == right[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    let mut ops = Vec::with_capacity(n.max(m));
    let (mut i, mut j) = (0, 0);
    while i < n && j < m {
        if left[i] == right[j] {
            ops.push((Some(i), Some(j)));
            i += 1;
            j += 1;
        } else if lcs[i + 1][j] >= lcs[i][j + 1] {
            ops.push((Some(i), None));
            i += 1;
        } else {
            ops.push((None, Some(j)));
            j += 1;
        }
    }
    while i < n {
        ops.push((Some(i), None));
        i += 1;
    }
    while j < m {
        ops.push((None, Some(j)));
        j += 1;
    }
    Ok(refine_replacement_blocks(ops))
}

/// Zip each deletion run followed by an insertion run into changed pairs.
fn refine_replacement_blocks(
    ops: Vec<(Option<usize>, Option<usize>)>,
) -> Vec<(Option<usize>, Option<usize>)> {
    let mut out = Vec::with_capacity(ops.len());
    let mut index = 0_usize;
    while index < ops.len() {
        match ops[index] {
            (Some(_), Some(_)) => {
                out.push(ops[index]);
                index += 1;
            }
            (Some(_), None) => {
                let deletions_start = index;
                while index < ops.len() && ops[index].0.is_some() && ops[index].1.is_none() {
                    index += 1;
                }
                let deletions_end = index;
                let insertions_start = index;
                while index < ops.len() && ops[index].0.is_none() && ops[index].1.is_some() {
                    index += 1;
                }
                let deletions = &ops[deletions_start..deletions_end];
                let insertions = &ops[insertions_start..index];
                let paired = deletions.len().min(insertions.len());
                for step in 0..paired {
                    out.push((deletions[step].0, insertions[step].1));
                }
                out.extend_from_slice(&deletions[paired..]);
                out.extend_from_slice(&insertions[paired..]);
            }
            (None, Some(_)) => {
                out.push(ops[index]);
                index += 1;
            }
            (None, None) => index += 1,
        }
    }
    out
}

/// Removed and added line counts between one aligned page pair.
///
/// Uses the same LCS as page alignment. When the matrix would exceed the cap,
/// every line on each side counts as changed rather than reporting a false
/// "same".
fn line_changes(left: &[String], right: &[String]) -> (usize, usize) {
    if (left.len() + 1).saturating_mul(right.len() + 1) > MAX_ALIGNMENT_CELLS {
        return (left.len(), right.len());
    }
    let mut lcs = vec![vec![0usize; right.len() + 1]; left.len() + 1];
    for i in (0..left.len()).rev() {
        for j in (0..right.len()).rev() {
            lcs[i][j] = if left[i] == right[j] {
                lcs[i + 1][j + 1] + 1
            } else {
                lcs[i + 1][j].max(lcs[i][j + 1])
            };
        }
    }
    (left.len() - lcs[0][0], right.len() - lcs[0][0])
}

/// Append the differing lines of a changed page pair as two columns.
///
/// Aligned-but-different lines print as `- left | + right`; deleted and added
/// lines print alone. Output is capped at [`MAX_DISPLAY_LINES`] rows.
fn differing_lines(left: &[String], right: &[String], out: &mut Vec<String>) {
    let Ok(ops) = align(left, right) else {
        out.push("  diff unavailable: page is too large to align".to_owned());
        return;
    };
    let mut printed = 0_usize;
    for (l, r) in ops {
        if printed >= MAX_DISPLAY_LINES {
            out.push("  ...".to_owned());
            break;
        }
        match (l, r) {
            (Some(l), Some(r)) => {
                if left[l] != right[r] {
                    out.push(format!("  - {} | + {}", left[l], right[r]));
                    printed += 1;
                }
            }
            (Some(l), None) => {
                out.push(format!("  - {}", left[l]));
                printed += 1;
            }
            (None, Some(r)) => {
                out.push(format!("  + {}", right[r]));
                printed += 1;
            }
            (None, None) => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn strings(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn normalised_lines_drop_blank_and_padded_lines() {
        assert_eq!(
            normalised_lines("  alpha  \n\n beta\n   \n"),
            strings(&["alpha", "beta"])
        );
        assert!(normalised_lines("\n \n").is_empty());
    }

    #[test]
    fn alignment_reports_inserted_and_deleted_pages_in_order() {
        let left = strings(&["a", "b", "c"]);
        let right = strings(&["a", "c", "d"]);
        let ops = align(&left, &right).expect("small pages align");
        assert_eq!(
            ops,
            vec![
                (Some(0), Some(0)),
                (Some(1), None),
                (Some(2), Some(1)),
                (None, Some(2)),
            ]
        );
    }

    #[test]
    fn alignment_pairs_a_replaced_page_at_the_same_position() {
        let left = strings(&["title", "old body", "tail"]);
        let right = strings(&["title", "new body", "tail"]);
        assert_eq!(
            align(&left, &right).expect("small pages align"),
            vec![(Some(0), Some(0)), (Some(1), Some(1)), (Some(2), Some(2)),]
        );
    }

    #[test]
    fn alignment_keeps_unpaired_leftovers_when_block_sizes_differ() {
        let left = strings(&["a", "b", "c", "d"]);
        let right = strings(&["a", "x"]);
        assert_eq!(
            align(&left, &right).expect("small pages align"),
            vec![
                (Some(0), Some(0)),
                (Some(1), Some(1)),
                (Some(2), None),
                (Some(3), None),
            ]
        );
    }

    #[test]
    fn alignment_is_stable_for_identical_and_empty_inputs() {
        let pages = strings(&["a", "b"]);
        assert_eq!(
            align(&pages, &pages).expect("identical"),
            vec![(Some(0), Some(0)), (Some(1), Some(1))]
        );
        assert!(align(&[], &[]).expect("empty").is_empty());
        assert_eq!(
            align(&[], &pages).expect("insertions"),
            vec![(None, Some(0)), (None, Some(1))]
        );
    }

    #[test]
    fn line_changes_count_only_new_lines() {
        let left = strings(&["a", "b", "c"]);
        let right = strings(&["a", "c"]);
        assert_eq!(line_changes(&left, &right), (1, 0));
        let left = strings(&["a"]);
        let right = strings(&["a", "b", "c"]);
        assert_eq!(line_changes(&left, &right), (0, 2));
    }

    #[test]
    fn differing_lines_print_two_columns_with_a_cap() {
        let left = strings(&["same", "old"]);
        let right = strings(&["same", "new"]);
        let mut out = Vec::new();
        differing_lines(&left, &right, &mut out);
        assert_eq!(out, vec!["  - old | + new".to_owned()]);

        let many = (0..40)
            .map(|index| format!("line {index}"))
            .collect::<Vec<_>>();
        let mut out = Vec::new();
        differing_lines(&many, &[], &mut out);
        assert_eq!(out.len(), MAX_DISPLAY_LINES + 1);
        assert_eq!(out.last().map(String::as_str), Some("  ..."));
    }
}
