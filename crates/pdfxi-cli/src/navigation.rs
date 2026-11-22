//! `pdfxi outline`, `pdfxi link` and `pdfxi attach`: edit the interactive layer.
//!
//! The readers (`pdfxi links`, `pdfxi attachments`) report what a file already
//! contains. This module is the write side: it stages bookmarks through
//! `pdfxi_edit::write_outline`, links through `pdfxi_edit::add_link` and
//! embedded files through `pdfxi_edit::add_attachment`, then saves a standalone
//! document and reads it back so the printed report is evidence from the file,
//! not from the command's own state.
//!
//! Bookmark paths are dotted child indices (`0.1` is the second child of the
//! first root); the empty path addresses the root list. Page numbers are
//! one-based on the command line and in the reports.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_edit::{
    Bookmark, BookmarkPath, Link, LinkHighlight, LinkTarget, add_attachment, add_link, link_rect,
    page_destination, read_outline, remove_attachment, remove_link, write_outline,
};
use serde::Serialize;

/// One bookmark row in the JSON report.
#[derive(Serialize)]
struct BookmarkRow {
    /// Dotted child-index path from the root.
    path: String,
    /// `/Title` text.
    title: String,
    /// One-based destination page, when the bookmark jumps somewhere.
    page: Option<usize>,
    /// Number of direct children.
    children: usize,
}

/// JSON shape of the outline commands.
#[derive(Serialize)]
struct OutlineReport {
    /// Input file.
    file: String,
    /// Output file for edits; omitted when listing.
    #[serde(skip_serializing_if = "Option::is_none")]
    out: Option<String>,
    /// Whether the catalog has an `/Outlines` entry after the operation.
    present: bool,
    /// Total bookmark count.
    count: usize,
    /// Bookmarks in display order.
    bookmarks: Vec<BookmarkRow>,
    /// Items kept without a readable destination, with the reason.
    issues: Vec<String>,
}

/// One link row in the JSON report.
#[derive(Serialize)]
struct LinkRow {
    /// One-based page that carries the annotation.
    page: usize,
    /// `/Rect` in points.
    rect: [f64; 4],
    /// `uri` or `page`.
    kind: &'static str,
    /// External URI, when the target is a URI.
    #[serde(skip_serializing_if = "Option::is_none")]
    uri: Option<String>,
    /// One-based destination page, when the target is a page.
    #[serde(skip_serializing_if = "Option::is_none")]
    dest_page: Option<usize>,
}

/// JSON shape of the `pdfxi link` edits.
#[derive(Serialize)]
struct LinkReport {
    /// Input file.
    file: String,
    /// Output file.
    out: String,
    /// One-based page the edit applied to.
    page: usize,
    /// Links left on that page.
    links: Vec<LinkRow>,
}

/// One attachment in the JSON report.
#[derive(Serialize)]
struct AttachmentRow {
    /// File name.
    name: String,
    /// Optional description.
    #[serde(skip_serializing_if = "Option::is_none")]
    description: Option<String>,
    /// Embedded size in bytes.
    size: Option<u64>,
}

/// JSON shape of the `pdfxi attach` edits.
#[derive(Serialize)]
struct AttachReport {
    /// Input file.
    file: String,
    /// Output file.
    out: String,
    /// Attachment that was added or removed.
    name: String,
    /// Embedded size for an add.
    #[serde(skip_serializing_if = "Option::is_none")]
    size: Option<u64>,
    /// Attachments in the saved document.
    attachments: Vec<AttachmentRow>,
}

/// `pdfxi outline add` request.
pub(crate) struct OutlineAddRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// Bookmark title.
    pub title: &'a str,
    /// One-based destination page.
    pub page: usize,
    /// Parent path; the root list when `None` or empty.
    pub parent: Option<&'a str>,
    /// Insert position; appends when `None`.
    pub index: Option<usize>,
    /// Emit JSON.
    pub json: bool,
}

/// `pdfxi link add` request.
pub(crate) struct LinkAddRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// One-based page to place the link on.
    pub page: usize,
    /// `x0,y0,x1,y1` in points.
    pub rect: &'a str,
    /// External URI target.
    pub uri: Option<&'a str>,
    /// One-based destination page target.
    pub to_page: Option<usize>,
    /// `none`, `invert`, `outline` or `push`.
    pub highlight: &'a str,
    /// Emit JSON.
    pub json: bool,
}

/// `pdfxi attach add` request.
pub(crate) struct AttachAddRequest<'a> {
    /// Input file.
    pub file: &'a Path,
    /// Output file.
    pub out: &'a Path,
    /// File whose bytes are embedded.
    pub attachment: &'a Path,
    /// Name-tree key; defaults to the attachment file name.
    pub name: Option<&'a str>,
    /// Optional description.
    pub description: &'a str,
    /// MIME type; defaults to `application/octet-stream`.
    pub mime: &'a str,
    /// Emit JSON.
    pub json: bool,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

fn save(document: &Document, out: &Path) -> anyhow::Result<Vec<u8>> {
    let bytes = document.save().context("save the edited document")?;
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    Ok(bytes)
}

fn reopen(bytes: Vec<u8>) -> anyhow::Result<Document> {
    Document::from_bytes(bytes).context("reopen the saved document")
}

/// Parse a dotted bookmark path; an empty string addresses the root list.
fn parse_path(text: &str) -> anyhow::Result<BookmarkPath> {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return Ok(BookmarkPath::root());
    }
    let mut path = BookmarkPath::root();
    for part in trimmed.split('.') {
        let index: usize = part
            .trim()
            .parse()
            .with_context(|| format!("bookmark path part {part:?} is not a number"))?;
        path = path.child(index);
    }
    Ok(path)
}

/// Render a path as dotted child indices.
fn render_path(indices: &[usize]) -> String {
    if indices.is_empty() {
        return "".to_owned();
    }
    indices
        .iter()
        .map(usize::to_string)
        .collect::<Vec<_>>()
        .join(".")
}

fn rows(outline: &pdfxi_edit::Outline) -> Vec<BookmarkRow> {
    fn walk(nodes: &[Bookmark], prefix: &mut Vec<usize>, out: &mut Vec<BookmarkRow>) {
        for (index, node) in nodes.iter().enumerate() {
            prefix.push(index);
            out.push(BookmarkRow {
                path: render_path(prefix),
                title: node.title.clone(),
                page: node.destination.as_ref().map(|dest| dest.page.get() + 1),
                children: node.children.len(),
            });
            walk(&node.children, prefix, out);
            prefix.pop();
        }
    }
    let mut out = Vec::new();
    let mut prefix = Vec::new();
    walk(&outline.roots, &mut prefix, &mut out);
    out
}

fn outline_report(
    file: &Path,
    out: Option<&Path>,
    document: &Document,
) -> anyhow::Result<OutlineReport> {
    let read = read_outline(document).map_err(|error| anyhow::anyhow!("read outline: {error}"))?;
    Ok(OutlineReport {
        file: file.display().to_string(),
        out: out.map(|path| path.display().to_string()),
        present: read.present,
        count: rows(&read.outline).len(),
        bookmarks: rows(&read.outline),
        issues: read
            .issues
            .iter()
            .map(|issue| format!("{}: {:?}", issue.title, issue.reason))
            .collect(),
    })
}

fn print_outline(report: &OutlineReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    if let Some(out) = &report.out {
        println!("out:  {out}");
    }
    println!("present: {}", report.present);
    for row in &report.bookmarks {
        let depth = row.path.matches('.').count();
        let indent = "  ".repeat(depth);
        match row.page {
            Some(page) => println!("{indent}{}  {}  page {page}", row.path, row.title),
            None => println!("{indent}{}  {}  (no destination)", row.path, row.title),
        }
    }
    for issue in &report.issues {
        println!("issue: {issue}");
    }
    println!("bookmarks: {}", report.count);
    Ok(())
}

/// `pdfxi outline list`.
pub(crate) fn run_outline_list(file: &Path, json: bool) -> anyhow::Result<()> {
    let document = load(file)?;
    let report = outline_report(file, None, &document)?;
    print_outline(&report, json)
}

/// `pdfxi outline add`.
pub(crate) fn run_outline_add(request: OutlineAddRequest<'_>) -> anyhow::Result<()> {
    if request.page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let mut document = load(request.file)?;
    let read = read_outline(&document).map_err(|error| anyhow::anyhow!("read outline: {error}"))?;
    let mut outline = read.outline;
    let parent = match request.parent {
        Some(text) => parse_path(text)?,
        None => BookmarkPath::root(),
    };
    let index = match request.index {
        Some(index) => index,
        None => child_count(&outline, &parent)?,
    };
    let bookmark = Bookmark::new(request.title)
        .map_err(|error| anyhow::anyhow!("invalid bookmark title: {error}"))?
        .with_destination(page_destination(request.page - 1));
    outline
        .insert_bookmark(&parent, index, bookmark)
        .map_err(|error| anyhow::anyhow!("insert bookmark: {error}"))?;
    write_outline(&mut document, &outline)
        .map_err(|error| anyhow::anyhow!("write outline: {error}"))?;
    let bytes = save(&document, request.out)?;
    let reopened = reopen(bytes)?;
    let report = outline_report(request.file, Some(request.out), &reopened)?;
    print_outline(&report, request.json)
}

/// `pdfxi outline rename`.
pub(crate) fn run_outline_rename(
    file: &Path,
    out: &Path,
    path: &str,
    title: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut document = load(file)?;
    let read = read_outline(&document).map_err(|error| anyhow::anyhow!("read outline: {error}"))?;
    let mut outline = read.outline;
    let path = parse_path(path)?;
    outline
        .rename_bookmark(&path, title)
        .map_err(|error| anyhow::anyhow!("rename bookmark: {error}"))?;
    write_outline(&mut document, &outline)
        .map_err(|error| anyhow::anyhow!("write outline: {error}"))?;
    let bytes = save(&document, out)?;
    let reopened = reopen(bytes)?;
    let report = outline_report(file, Some(out), &reopened)?;
    print_outline(&report, json)
}

/// `pdfxi outline move`.
pub(crate) fn run_outline_move(
    file: &Path,
    out: &Path,
    path: &str,
    parent: Option<&str>,
    index: Option<usize>,
    json: bool,
) -> anyhow::Result<()> {
    let mut document = load(file)?;
    let read = read_outline(&document).map_err(|error| anyhow::anyhow!("read outline: {error}"))?;
    let mut outline = read.outline;
    let path = parse_path(path)?;
    let parent = match parent {
        Some(text) => parse_path(text)?,
        None => BookmarkPath::root(),
    };
    let index = match index {
        Some(index) => index,
        None => child_count(&outline, &parent)?,
    };
    outline
        .move_bookmark(&path, &parent, index)
        .map_err(|error| anyhow::anyhow!("move bookmark: {error}"))?;
    write_outline(&mut document, &outline)
        .map_err(|error| anyhow::anyhow!("write outline: {error}"))?;
    let bytes = save(&document, out)?;
    let reopened = reopen(bytes)?;
    let report = outline_report(file, Some(out), &reopened)?;
    print_outline(&report, json)
}

/// `pdfxi outline remove`.
pub(crate) fn run_outline_remove(
    file: &Path,
    out: &Path,
    path: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut document = load(file)?;
    let read = read_outline(&document).map_err(|error| anyhow::anyhow!("read outline: {error}"))?;
    let mut outline = read.outline;
    let path = parse_path(path)?;
    outline
        .remove_bookmark(&path)
        .map_err(|error| anyhow::anyhow!("remove bookmark: {error}"))?;
    write_outline(&mut document, &outline)
        .map_err(|error| anyhow::anyhow!("write outline: {error}"))?;
    let bytes = save(&document, out)?;
    let reopened = reopen(bytes)?;
    let report = outline_report(file, Some(out), &reopened)?;
    print_outline(&report, json)
}

/// Number of children under a parent path, used when appending.
fn child_count(outline: &pdfxi_edit::Outline, parent: &BookmarkPath) -> anyhow::Result<usize> {
    if parent.is_root() {
        return Ok(outline.roots.len());
    }
    let bookmark = outline
        .bookmark_at(parent)
        .map_err(|error| anyhow::anyhow!("resolve parent: {error}"))?;
    Ok(bookmark.children.len())
}

/// Parse `none`, `invert`, `outline` or `push`.
fn parse_highlight(text: &str) -> anyhow::Result<LinkHighlight> {
    match text.to_ascii_lowercase().as_str() {
        "none" => Ok(LinkHighlight::None),
        "invert" => Ok(LinkHighlight::Invert),
        "outline" => Ok(LinkHighlight::Outline),
        "push" => Ok(LinkHighlight::Push),
        other => bail!("unknown highlight {other:?}; expected none, invert, outline or push"),
    }
}

fn link_rows(document: &Document, page: usize) -> anyhow::Result<Vec<LinkRow>> {
    let links = document
        .links()
        .map_err(|error| anyhow::anyhow!("read links: {error}"))?;
    let mut rows = Vec::new();
    for link in links.iter().filter(|link| link.page == page) {
        let (kind, uri, dest_page) = match &link.target {
            pdfxi_core::LinkTarget::Uri { uri } => ("uri", Some(uri.clone()), None),
            pdfxi_core::LinkTarget::Page { page, .. } => ("page", None, Some(page + 1)),
            pdfxi_core::LinkTarget::Unresolved { detail } => {
                bail!("saved link is unresolved: {detail}")
            }
        };
        rows.push(LinkRow {
            page: link.page + 1,
            rect: link.rect,
            kind,
            uri,
            dest_page,
        });
    }
    Ok(rows)
}

fn print_links(report: &LinkReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("page: {}", report.page);
    for row in &report.links {
        let rect = format!(
            "{} {} {} {}",
            row.rect[0], row.rect[1], row.rect[2], row.rect[3]
        );
        match row.kind {
            "uri" => println!(
                "uri  {}  rect: {rect}",
                row.uri.as_deref().unwrap_or_default()
            ),
            _ => println!("page {}  rect: {rect}", row.dest_page.unwrap_or_default()),
        }
    }
    println!("links: {}", report.links.len());
    Ok(())
}

/// `pdfxi link add`.
pub(crate) fn run_link_add(request: LinkAddRequest<'_>) -> anyhow::Result<()> {
    if request.page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let target = match (request.uri, request.to_page) {
        (Some(_), Some(_)) => bail!("use either --uri or --to-page, not both"),
        (None, None) => bail!("a link needs --uri URL or --to-page N"),
        (Some(uri), None) => LinkTarget::Uri(uri.to_owned()),
        (None, Some(page)) => {
            if page == 0 {
                bail!("--to-page is one-based and must be at least 1");
            }
            LinkTarget::Destination(page_destination(page - 1))
        }
    };
    let rect = crate::annotate::parse_rect(request.rect)?;
    let rect = link_rect(rect.x0(), rect.y0(), rect.x1(), rect.y1())
        .map_err(|error| anyhow::anyhow!("invalid link rectangle: {error}"))?;
    let link = Link::new(rect, target).with_highlight(parse_highlight(request.highlight)?);

    let mut document = load(request.file)?;
    add_link(&mut document, request.page - 1, &link)
        .map_err(|error| anyhow::anyhow!("add link: {error}"))?;
    let bytes = save(&document, request.out)?;
    let reopened = reopen(bytes)?;
    let report = LinkReport {
        file: request.file.display().to_string(),
        out: request.out.display().to_string(),
        page: request.page,
        links: link_rows(&reopened, request.page - 1)?,
    };
    print_links(&report, request.json)
}

/// `pdfxi link remove`.
pub(crate) fn run_link_remove(
    file: &Path,
    out: &Path,
    page: usize,
    index: usize,
    json: bool,
) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let mut document = load(file)?;
    remove_link(&mut document, page - 1, index)
        .map_err(|error| anyhow::anyhow!("remove link: {error}"))?;
    let bytes = save(&document, out)?;
    let reopened = reopen(bytes)?;
    let report = LinkReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        page,
        links: link_rows(&reopened, page - 1)?,
    };
    print_links(&report, json)
}

fn attachment_rows(document: &Document) -> anyhow::Result<Vec<AttachmentRow>> {
    let attachments = document
        .attachments()
        .map_err(|error| anyhow::anyhow!("read attachments: {error}"))?;
    Ok(attachments
        .into_iter()
        .map(|attachment| AttachmentRow {
            name: attachment.name,
            description: attachment.description,
            size: attachment.size,
        })
        .collect())
}

fn print_attachments(report: &AttachReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("name: {}", report.name);
    if let Some(size) = report.size {
        println!("size: {size}");
    }
    for row in &report.attachments {
        match row.size {
            Some(size) => println!("{}  {size} bytes", row.name),
            None => println!("{}", row.name),
        }
    }
    println!("attachments: {}", report.attachments.len());
    Ok(())
}

/// `pdfxi attach add`.
pub(crate) fn run_attach_add(request: AttachAddRequest<'_>) -> anyhow::Result<()> {
    let bytes = std::fs::read(request.attachment)
        .with_context(|| format!("read attachment payload {}", request.attachment.display()))?;
    let default_name = request
        .attachment
        .file_name()
        .map(|name| name.to_string_lossy().into_owned())
        .context("the attachment path has no file name; pass --name")?;
    let name = request.name.unwrap_or(&default_name);
    let mut document = load(request.file)?;
    add_attachment(
        &mut document,
        name,
        Some(name),
        request.description,
        request.mime,
        &bytes,
    )
    .map_err(|error| anyhow::anyhow!("add attachment: {error}"))?;
    let saved = save(&document, request.out)?;
    let reopened = reopen(saved)?;
    let report = AttachReport {
        file: request.file.display().to_string(),
        out: request.out.display().to_string(),
        name: name.to_owned(),
        size: Some(u64::try_from(bytes.len()).unwrap_or(u64::MAX)),
        attachments: attachment_rows(&reopened)?,
    };
    print_attachments(&report, request.json)
}

/// `pdfxi attach remove`.
pub(crate) fn run_attach_remove(
    file: &Path,
    out: &Path,
    name: &str,
    json: bool,
) -> anyhow::Result<()> {
    let mut document = load(file)?;
    remove_attachment(&mut document, name)
        .map_err(|error| anyhow::anyhow!("remove attachment: {error}"))?;
    let saved = save(&document, out)?;
    let reopened = reopen(saved)?;
    let report = AttachReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        name: name.to_owned(),
        size: None,
        attachments: attachment_rows(&reopened)?,
    };
    print_attachments(&report, json)
}

#[cfg(test)]
mod tests {
    use super::{parse_highlight, parse_path, render_path};

    #[test]
    fn bookmark_paths_parse_and_render() {
        assert!(parse_path("").expect("root").is_root());
        let path = parse_path("0.2.1").expect("path");
        assert_eq!(path.indices(), &[0, 2, 1]);
        assert_eq!(render_path(path.indices()), "0.2.1");
        assert!(parse_path("0.x").is_err());
        assert!(parse_path("0..1").is_err());
    }

    #[test]
    fn highlights_parse_case_insensitively_and_reject_unknown_names() {
        assert_eq!(
            parse_highlight("Outline").expect("outline"),
            pdfxi_edit::LinkHighlight::Outline
        );
        assert_eq!(
            parse_highlight("none").expect("none"),
            pdfxi_edit::LinkHighlight::None
        );
        assert!(parse_highlight("glow").is_err());
    }
}
