//! `pdfxi links` and `pdfxi attachments`: read the interactive layer.
//!
//! Both commands report zero-based page indices in JSON and one-based page
//! numbers in text, matching `pdfxi info`'s "pages: N" and the rest of the CLI.

use std::path::Path;

use anyhow::Context;
use pdfxi_core::{Document, LinkTarget};
use serde::Serialize;

/// One link in the JSON report.
#[derive(Serialize)]
struct LinkRow {
    /// One-based page number that carries the annotation.
    page: usize,
    /// Annotation rectangle in points.
    rect: [f64; 4],
    /// `uri`, `page` or `unresolved`.
    kind: &'static str,
    /// Destination page (one-based) for page targets.
    dest_page: Option<usize>,
    /// Destination name when the link used a named destination.
    dest_name: Option<String>,
    /// External URI for URI targets.
    uri: Option<String>,
    /// Reason or name when the target could not be resolved.
    detail: Option<String>,
}

/// JSON shape of `pdfxi links --json`.
#[derive(Serialize)]
struct LinksReport {
    /// File that was inspected.
    file: String,
    /// Number of link annotations.
    count: usize,
    /// One row per link, in page order.
    links: Vec<LinkRow>,
}

/// One attachment in the JSON report.
#[derive(Serialize)]
struct AttachmentRow {
    /// File name shown to the user.
    name: String,
    /// Optional description.
    description: Option<String>,
    /// Embedded stream size in bytes, when readable.
    size: Option<u64>,
    /// One-based page of a `/FileAttachment` annotation.
    page: Option<usize>,
}

/// JSON shape of `pdfxi attachments --json`.
#[derive(Serialize)]
struct AttachmentsReport {
    /// File that was inspected.
    file: String,
    /// Number of attachments.
    count: usize,
    /// One row per attachment.
    attachments: Vec<AttachmentRow>,
}

fn load(file: &Path) -> anyhow::Result<Document> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))
}

/// Print every link annotation in the document.
pub(crate) fn run_links(file: &Path, json: bool) -> anyhow::Result<()> {
    let document = load(file)?;
    let links = document.links().context("read link annotations")?;
    let rows = links
        .iter()
        .map(|link| {
            let (kind, dest_page, dest_name, uri, detail) = match &link.target {
                LinkTarget::Uri { uri } => ("uri", None, None, Some(uri.clone()), None),
                LinkTarget::Page { page, name } => {
                    ("page", Some(page + 1), name.clone(), None, None)
                }
                LinkTarget::Unresolved { detail } => {
                    ("unresolved", None, None, None, Some(detail.clone()))
                }
            };
            LinkRow {
                page: link.page + 1,
                rect: link.rect,
                kind,
                dest_page,
                dest_name,
                uri,
                detail,
            }
        })
        .collect::<Vec<_>>();
    let report = LinksReport {
        file: file.display().to_string(),
        count: rows.len(),
        links: rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &report.links {
            let target = match row.kind {
                "uri" => format!("uri {}", row.uri.as_deref().unwrap_or_default()),
                "page" => match &row.dest_name {
                    Some(name) => format!("page {} (named {name})", row.dest_page.unwrap_or(0)),
                    None => format!("page {}", row.dest_page.unwrap_or(0)),
                },
                _ => format!("unresolved {}", row.detail.as_deref().unwrap_or("unknown")),
            };
            println!(
                "page: {}  rect: {} {} {} {}  target: {target}",
                row.page, row.rect[0], row.rect[1], row.rect[2], row.rect[3]
            );
        }
        println!("links: {}", report.count);
    }
    Ok(())
}

/// Print every embedded-file attachment in the document.
pub(crate) fn run_attachments(file: &Path, json: bool) -> anyhow::Result<()> {
    let document = load(file)?;
    let attachments = document.attachments().context("read attachments")?;
    let rows = attachments
        .into_iter()
        .map(|attachment| AttachmentRow {
            name: attachment.name,
            description: attachment.description,
            size: attachment.size,
            page: attachment.page.map(|page| page + 1),
        })
        .collect::<Vec<_>>();
    let report = AttachmentsReport {
        file: file.display().to_string(),
        count: rows.len(),
        attachments: rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        for row in &report.attachments {
            let size = row
                .size
                .map_or_else(|| "?".to_owned(), |size| size.to_string());
            let page = row
                .page
                .map_or_else(String::new, |page| format!("  page: {page}"));
            let description = row
                .description
                .as_deref()
                .map_or_else(String::new, |text| format!("  description: {text}"));
            println!("name: {}  size: {size}{page}{description}", row.name);
        }
        println!("attachments: {}", report.count);
    }
    Ok(())
}
