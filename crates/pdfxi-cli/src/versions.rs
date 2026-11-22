//! `pdfxi versions`: store page-text snapshots and diff revisions.
//!
//! A store is a small JSON document holding one text snapshot per revision. A
//! snapshot is the PDF path, an optional label and the normalised page text, so
//! a later revision can be compared without keeping the original PDF around.
//! Comparison reuses `pdfxi_edit::compare`, the same bounded line-hunk model as
//! `pdfxi compare --hunks`.
//!
//! Snapshots are identified by monotonically increasing ids. Removing a
//! snapshot never reuses its id, so a diff target cannot silently change
//! meaning. The store is written with `serde_json` and contains only text: no
//! document bytes, secrets or binary payloads.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{CompareOptions, TextDocument, TextPage as EditTextPage, compare_documents};
use serde::{Deserialize, Serialize};

/// Store format version. A different value is refused rather than guessed.
const STORE_VERSION: u32 = 1;
/// Maximum snapshots kept in one store.
const MAX_SNAPSHOTS: usize = 256;

/// One stored page of normalised text.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct SnapshotPage {
    /// Zero-based page index.
    page: usize,
    /// Trimmed, non-empty lines.
    lines: Vec<String>,
}

/// One stored revision.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Snapshot {
    /// Monotonic identifier.
    id: u64,
    /// Optional human label.
    label: String,
    /// Source PDF path at snapshot time.
    source: String,
    /// Page text in page order.
    pages: Vec<SnapshotPage>,
}

impl Snapshot {
    /// Total normalised lines.
    fn lines(&self) -> usize {
        self.pages.iter().map(|page| page.lines.len()).sum()
    }

    /// Convert to the comparison model.
    fn document(&self) -> anyhow::Result<TextDocument> {
        let pages = self
            .pages
            .iter()
            .map(|page| EditTextPage {
                page: page.page,
                lines: page.lines.clone(),
            })
            .collect();
        TextDocument::new(pages).map_err(|error| anyhow::anyhow!("snapshot {}: {error}", self.id))
    }
}

/// The JSON store.
#[derive(Clone, Debug, Deserialize, Serialize)]
struct Store {
    /// Format version.
    version: u32,
    /// Next id to assign.
    next_id: u64,
    /// Snapshots in insertion order.
    snapshots: Vec<Snapshot>,
}

impl Default for Store {
    fn default() -> Self {
        Self {
            version: STORE_VERSION,
            next_id: 1,
            snapshots: Vec::new(),
        }
    }
}

impl Store {
    /// Read a store, or an empty store when the file does not exist.
    fn load(path: &Path) -> anyhow::Result<Self> {
        if !path.exists() {
            return Ok(Self::default());
        }
        let bytes =
            std::fs::read(path).with_context(|| format!("read store {}", path.display()))?;
        let store: Self = serde_json::from_slice(&bytes)
            .with_context(|| format!("parse store {}", path.display()))?;
        if store.version != STORE_VERSION {
            bail!(
                "store {} uses format version {}; this build supports {STORE_VERSION}",
                path.display(),
                store.version
            );
        }
        Ok(store)
    }

    /// Write the store atomically through a sibling temporary file.
    fn save(&self, path: &Path) -> anyhow::Result<()> {
        if let Some(parent) = path.parent()
            && !parent.as_os_str().is_empty()
        {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("create store directory {}", parent.display()))?;
        }
        let bytes = serde_json::to_vec_pretty(self).context("serialize version store")?;
        let temporary = path.with_extension("tmp");
        std::fs::write(&temporary, &bytes)
            .with_context(|| format!("write {}", temporary.display()))?;
        std::fs::rename(&temporary, path)
            .with_context(|| format!("replace store {}", path.display()))?;
        Ok(())
    }

    /// Find a snapshot by id.
    fn get(&self, id: u64) -> Option<&Snapshot> {
        self.snapshots.iter().find(|snapshot| snapshot.id == id)
    }

    /// Find a snapshot by id or fail with the known ids.
    fn require(&self, id: u64, path: &Path) -> anyhow::Result<&Snapshot> {
        self.get(id).ok_or_else(|| {
            let ids: Vec<String> = self
                .snapshots
                .iter()
                .map(|snapshot| snapshot.id.to_string())
                .collect();
            anyhow::anyhow!(
                "no snapshot {id} in {} (known: {})",
                path.display(),
                if ids.is_empty() {
                    "none".to_owned()
                } else {
                    ids.join(", ")
                }
            )
        })
    }
}

/// Extract normalised page text from a PDF.
fn pages_of(file: &Path) -> anyhow::Result<Vec<SnapshotPage>> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let count = document
        .page_count()
        .with_context(|| format!("count pages of {}", file.display()))?;
    let options = CompareOptions::default();
    if count > options.max_pages {
        bail!(
            "{} has {count} pages; the snapshot limit is {}",
            file.display(),
            options.max_pages
        );
    }
    let mut pages = Vec::with_capacity(count);
    let mut total_lines = 0;
    for index in 0..count {
        let text = document.page_text(index).with_context(|| {
            format!("extract text from page {} of {}", index + 1, file.display())
        })?;
        let lines: Vec<String> = text
            .lines()
            .map(str::trim)
            .filter(|line| !line.is_empty())
            .map(str::to_owned)
            .collect();
        if lines.len() > options.max_lines_per_page {
            bail!(
                "page {} of {} has {} lines; the limit is {}",
                index + 1,
                file.display(),
                lines.len(),
                options.max_lines_per_page
            );
        }
        if let Some(line) = lines
            .iter()
            .find(|line| line.len() > options.max_line_bytes)
        {
            bail!(
                "page {} of {} has a {}-byte line; the limit is {}",
                index + 1,
                file.display(),
                line.len(),
                options.max_line_bytes
            );
        }
        total_lines += lines.len();
        pages.push(SnapshotPage { page: index, lines });
    }
    if total_lines > 2_000_000 {
        bail!("{} carries too much text for a snapshot", file.display());
    }
    Ok(pages)
}

/// Snapshot report.
#[derive(Serialize)]
struct SnapshotReport {
    /// Store path.
    store: String,
    /// Assigned id.
    id: u64,
    /// Label, empty when none was given.
    label: String,
    /// Source PDF path.
    source: String,
    /// Pages captured.
    pages: usize,
    /// Normalised lines captured.
    lines: usize,
}

/// `pdfxi versions snapshot`.
pub(crate) fn run_snapshot(
    file: &Path,
    store_path: &Path,
    label: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut store = Store::load(store_path)?;
    if store.snapshots.len() >= MAX_SNAPSHOTS {
        bail!(
            "store {} already holds {MAX_SNAPSHOTS} snapshots",
            store_path.display()
        );
    }
    let pages = pages_of(file)?;
    if pages.is_empty() {
        bail!("{} has no pages", file.display());
    }
    let id = store.next_id;
    let snapshot = Snapshot {
        id,
        label: label.to_owned(),
        source: file.display().to_string(),
        pages,
    };
    store.next_id = store
        .next_id
        .checked_add(1)
        .context("snapshot id space exhausted")?;
    let report = SnapshotReport {
        store: store_path.display().to_string(),
        id,
        label: snapshot.label.clone(),
        source: snapshot.source.clone(),
        pages: snapshot.pages.len(),
        lines: snapshot.lines(),
    };
    store.snapshots.push(snapshot);
    store.save(store_path)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("store: {}", report.store);
    println!("id: {}", report.id);
    if !report.label.is_empty() {
        println!("label: {}", report.label);
    }
    println!("source: {}", report.source);
    println!("pages: {}", report.pages);
    println!("lines: {}", report.lines);
    Ok(())
}

/// One row in `versions list`.
#[derive(Serialize)]
struct ListRow {
    /// Snapshot id.
    id: u64,
    /// Optional label.
    label: String,
    /// Source PDF path.
    source: String,
    /// Pages captured.
    pages: usize,
    /// Normalised lines captured.
    lines: usize,
}

/// `pdfxi versions list`.
pub(crate) fn run_list(store_path: &Path, json: bool) -> anyhow::Result<()> {
    let store = Store::load(store_path)?;
    let rows: Vec<ListRow> = store
        .snapshots
        .iter()
        .map(|snapshot| ListRow {
            id: snapshot.id,
            label: snapshot.label.clone(),
            source: snapshot.source.clone(),
            pages: snapshot.pages.len(),
            lines: snapshot.lines(),
        })
        .collect();
    if json {
        println!("{}", serde_json::to_string_pretty(&rows)?);
        return Ok(());
    }
    println!("store: {}", store_path.display());
    for row in &rows {
        let label = if row.label.is_empty() {
            String::new()
        } else {
            format!("  \"{}\"", row.label)
        };
        println!(
            "#{}  {}  {} pages  {} lines{}",
            row.id, row.source, row.pages, row.lines, label
        );
    }
    println!("snapshots: {}", rows.len());
    Ok(())
}

/// One hunk in the diff report.
#[derive(Serialize)]
struct HunkRow {
    /// `added`, `removed` or `changed`.
    kind: &'static str,
    /// One-based page.
    page: usize,
    /// One-based old line.
    #[serde(skip_serializing_if = "Option::is_none")]
    old_line: Option<usize>,
    /// One-based new line.
    #[serde(skip_serializing_if = "Option::is_none")]
    new_line: Option<usize>,
    /// Old text.
    #[serde(skip_serializing_if = "Option::is_none")]
    old_text: Option<String>,
    /// New text.
    #[serde(skip_serializing_if = "Option::is_none")]
    new_text: Option<String>,
}

/// Summary counts.
#[derive(Serialize)]
struct DiffSummary {
    /// Added hunks.
    added: usize,
    /// Removed hunks.
    removed: usize,
    /// Changed hunks.
    changed: usize,
    /// Pages examined.
    pages_examined: usize,
    /// Total hunks.
    hunks: usize,
}

/// One side of a diff.
#[derive(Serialize)]
struct DiffSide {
    /// `snapshot` or `file`.
    kind: &'static str,
    /// Snapshot id, when the side is a snapshot.
    #[serde(skip_serializing_if = "Option::is_none")]
    id: Option<u64>,
    /// Label or path.
    label: String,
    /// Pages.
    pages: usize,
    /// Lines.
    lines: usize,
}

/// JSON shape of `versions diff`.
#[derive(Serialize)]
struct DiffReport {
    /// Store path, when supplied.
    #[serde(skip_serializing_if = "Option::is_none")]
    store: Option<String>,
    /// Left side.
    left: DiffSide,
    /// Right side.
    right: DiffSide,
    /// Whether the text is identical.
    identical: bool,
    /// Counts.
    summary: DiffSummary,
    /// Hunks in deterministic order.
    hunks: Vec<HunkRow>,
}

/// Resolve one diff argument to a document and description.
fn resolve_side(
    argument: &str,
    store: Option<&Store>,
    store_path: Option<&Path>,
) -> anyhow::Result<(TextDocument, DiffSide)> {
    if let Some(store) = store
        && let Ok(id) = argument.parse::<u64>()
    {
        let path = store_path.unwrap_or(Path::new("<store>"));
        let snapshot = store.require(id, path)?;
        let document = snapshot.document()?;
        return Ok((
            document,
            DiffSide {
                kind: "snapshot",
                id: Some(snapshot.id),
                label: if snapshot.label.is_empty() {
                    snapshot.source.clone()
                } else {
                    snapshot.label.clone()
                },
                pages: snapshot.pages.len(),
                lines: snapshot.lines(),
            },
        ));
    }
    let path = Path::new(argument);
    let pages = pages_of(path)?;
    let entries: Vec<EditTextPage> = pages
        .iter()
        .map(|page| EditTextPage {
            page: page.page,
            lines: page.lines.clone(),
        })
        .collect();
    let document =
        TextDocument::new(entries).map_err(|error| anyhow::anyhow!("{argument}: {error}"))?;
    Ok((
        document,
        DiffSide {
            kind: "file",
            id: None,
            label: argument.to_owned(),
            pages: pages.len(),
            lines: pages.iter().map(|page| page.lines.len()).sum(),
        },
    ))
}

/// `pdfxi versions diff`.
pub(crate) fn run_diff(
    left: &str,
    right: &str,
    store_path: Option<&Path>,
    json: bool,
) -> anyhow::Result<()> {
    let store = match store_path {
        Some(path) => Some(Store::load(path)?),
        None => None,
    };
    let (left_document, left_side) = resolve_side(left, store.as_ref(), store_path)?;
    let (right_document, right_side) = resolve_side(right, store.as_ref(), store_path)?;
    let result = compare_documents(&left_document, &right_document, &CompareOptions::default())
        .map_err(|error| anyhow::anyhow!("compare snapshots: {error}"))?;
    let report = DiffReport {
        store: store_path.map(|path| path.display().to_string()),
        left: left_side,
        right: right_side,
        identical: result.hunks.is_empty(),
        summary: DiffSummary {
            added: result.summary.added,
            removed: result.summary.removed,
            changed: result.summary.changed,
            pages_examined: result.summary.pages_examined,
            hunks: result.summary.hunks,
        },
        hunks: result
            .hunks
            .iter()
            .map(|hunk| HunkRow {
                kind: hunk.kind.as_str(),
                page: hunk.page + 1,
                old_line: hunk.old_line,
                new_line: hunk.new_line,
                old_text: hunk.old_text.clone(),
                new_text: hunk.new_text.clone(),
            })
            .collect(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("left:  {} ({})", report.left.label, report.left.kind);
    println!("right: {} ({})", report.right.label, report.right.kind);
    for hunk in &report.hunks {
        println!(
            "{}  page {}  old {}  new {}",
            hunk.kind,
            hunk.page,
            hunk.old_line
                .map(|line| line.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            hunk.new_line
                .map(|line| line.to_string())
                .unwrap_or_else(|| "-".to_owned())
        );
        if let Some(text) = &hunk.old_text {
            println!("  - {text}");
        }
        if let Some(text) = &hunk.new_text {
            println!("  + {text}");
        }
    }
    println!(
        "summary: added {}, removed {}, changed {}, pages {}, hunks {}",
        report.summary.added,
        report.summary.removed,
        report.summary.changed,
        report.summary.pages_examined,
        report.summary.hunks
    );
    println!("identical: {}", report.identical);
    Ok(())
}

/// `pdfxi versions remove`.
pub(crate) fn run_remove(id: u64, store_path: &Path, json: bool) -> anyhow::Result<()> {
    let mut store = Store::load(store_path)?;
    let index = store
        .snapshots
        .iter()
        .position(|snapshot| snapshot.id == id)
        .ok_or_else(|| anyhow::anyhow!("no snapshot {id} in {}", store_path.display()))?;
    store.snapshots.remove(index);
    store.save(store_path)?;
    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "store": store_path.display().to_string(),
                "removed": id,
                "snapshots": store.snapshots.len(),
            }))?
        );
        return Ok(());
    }
    println!("store: {}", store_path.display());
    println!("removed: {id}");
    println!("snapshots: {}", store.snapshots.len());
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{STORE_VERSION, Snapshot, SnapshotPage, Store};

    #[test]
    fn store_defaults_and_validates_version() {
        let store = Store::default();
        assert_eq!(store.version, STORE_VERSION);
        assert_eq!(store.next_id, 1);
        assert!(store.snapshots.is_empty());
    }

    #[test]
    fn snapshot_line_count_sums_pages() {
        let snapshot = Snapshot {
            id: 1,
            label: String::new(),
            source: "memory".to_owned(),
            pages: vec![
                SnapshotPage {
                    page: 0,
                    lines: vec!["a".to_owned(), "b".to_owned()],
                },
                SnapshotPage {
                    page: 1,
                    lines: vec!["c".to_owned()],
                },
            ],
        };
        assert_eq!(snapshot.lines(), 3);
        assert_eq!(snapshot.document().expect("document").pages().len(), 2);
    }
}
