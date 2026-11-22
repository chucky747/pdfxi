//! `pdfxi interactions`: audit `/AA` additional actions in a document.
//!
//! [`pdfxi_render::actions`] validates `/AA` trigger dictionaries against a
//! whitelist of safe navigation actions and rejects code-executing or
//! host-touching actions. This host reads the real catalog, page and annotation
//! `/AA` dictionaries, reports every action (including rejected ones) and can
//! fail closed with `--strict`. It never executes an action.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use pdfxi_render::actions::{AaActionSet, AdditionalActionInput};
use serde::Serialize;

/// One action row.
#[derive(Serialize)]
struct ActionRow {
    /// Where the `/AA` dictionary lives.
    site: String,
    /// Trigger event code.
    event: String,
    /// Action kind name.
    kind: String,
    /// Action target, when the kind takes one.
    #[serde(skip_serializing_if = "Option::is_none")]
    target: Option<String>,
    /// `ok` or `rejected`.
    status: &'static str,
    /// Stable action code, for accepted actions.
    #[serde(skip_serializing_if = "Option::is_none")]
    code: Option<String>,
    /// Typed rejection reason.
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<String>,
}

/// JSON shape of `pdfxi interactions`.
#[derive(Serialize)]
struct InteractionsReport {
    /// Source PDF.
    file: String,
    /// Number of `/AA` dictionaries found.
    sites: usize,
    /// Total actions.
    actions: usize,
    /// Accepted actions.
    accepted: usize,
    /// Rejected actions.
    rejected: usize,
    /// Rows in site/event order.
    rows: Vec<ActionRow>,
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

/// Map a page object id to its zero-based index.
fn page_index(pages: &[ObjectId], id: ObjectId) -> Option<usize> {
    pages.iter().position(|page| *page == id)
}

/// Extract the destination target for a `/GoTo` or named action.
fn destination(
    document: &Document,
    pages: &[ObjectId],
    action: &Dictionary,
) -> anyhow::Result<(String, String)> {
    let kind = action
        .get("S")
        .and_then(Object::as_name)
        .unwrap_or_default();
    let kind = match kind {
        "GoTo" | "GoToNamed" | "GoToR" | "GoToRemote" => "GoTo".to_owned(),
        "JavaScript" | "JS" => "JavaScript".to_owned(),
        "Launch" => "Launch".to_owned(),
        "NextPage" => "NextPage".to_owned(),
        "PrevPage" => "PrevPage".to_owned(),
        "FirstPage" => "FirstPage".to_owned(),
        "LastPage" => "LastPage".to_owned(),
        "FullScreen" => "ToggleFullScreen".to_owned(),
        "URI" => "URI".to_owned(),
        "SubmitForm" => "SubmitForm".to_owned(),
        "ResetForm" => "ResetForm".to_owned(),
        "ImportData" => "ImportData".to_owned(),
        "Rendition" => "Rendition".to_owned(),
        "Movie" => "Movie".to_owned(),
        "Sound" => "Sound".to_owned(),
        other => other.to_owned(),
    };
    if kind != "GoTo" {
        return Ok((kind, String::new()));
    }
    let Some(value) = action.get("D") else {
        return Ok((kind, String::new()));
    };
    let value = resolve(document, value)?;
    let target = match value {
        Object::Integer(page) if page >= 0 => page.to_string(),
        Object::Name(name) => {
            return Ok(("GoToNamed".to_owned(), name));
        }
        Object::String(bytes) => {
            return Ok((
                "GoToNamed".to_owned(),
                String::from_utf8_lossy(&bytes).into_owned(),
            ));
        }
        Object::Array(items) => match items.first() {
            Some(Object::Reference(id)) => page_index(pages, *id)
                .map_or_else(|| format!("{} 0 R", id.number), |index| index.to_string()),
            Some(Object::Integer(page)) if *page >= 0 => page.to_string(),
            _ => String::new(),
        },
        _ => String::new(),
    };
    Ok((kind, target))
}

/// Collect rows for one `/AA` dictionary.
fn collect_site(
    document: &Document,
    pages: &[ObjectId],
    site: &str,
    additional: &Dictionary,
    rows: &mut Vec<ActionRow>,
) -> anyhow::Result<()> {
    for (event, value) in additional.iter() {
        let Object::Dictionary(action) = resolve(document, value)? else {
            continue;
        };
        let (kind, target) = destination(document, pages, &action)?;
        let input = AdditionalActionInput {
            event: event.to_owned(),
            kind,
            target: target.clone(),
        };
        match AaActionSet::parse(vec![input]) {
            Ok(set) => {
                let planned = set.actions().first();
                rows.push(ActionRow {
                    site: site.to_owned(),
                    event: event.to_owned(),
                    kind: planned.map_or_else(String::new, |row| action_kind(row.action())),
                    target: (!target.is_empty()).then_some(target),
                    status: "ok",
                    code: planned.map(|row| row.action().code()),
                    error: None,
                });
            }
            Err(error) => {
                rows.push(ActionRow {
                    site: site.to_owned(),
                    event: event.to_owned(),
                    kind: action
                        .get("S")
                        .and_then(Object::as_name)
                        .unwrap_or_default()
                        .to_owned(),
                    target: (!target.is_empty()).then_some(target),
                    status: "rejected",
                    code: None,
                    error: Some(error.to_string()),
                });
            }
        }
    }
    Ok(())
}

/// A short label for one safe action.
fn action_kind(action: &pdfxi_render::actions::SafeAction) -> String {
    match action {
        pdfxi_render::actions::SafeAction::GoTo { .. } => "GoTo".to_owned(),
        pdfxi_render::actions::SafeAction::GoToNamed { .. } => "GoToNamed".to_owned(),
        pdfxi_render::actions::SafeAction::NextPage => "NextPage".to_owned(),
        pdfxi_render::actions::SafeAction::PrevPage => "PrevPage".to_owned(),
        pdfxi_render::actions::SafeAction::FirstPage => "FirstPage".to_owned(),
        pdfxi_render::actions::SafeAction::LastPage => "LastPage".to_owned(),
        pdfxi_render::actions::SafeAction::ToggleFullScreen => "ToggleFullScreen".to_owned(),
    }
}

/// `pdfxi interactions`.
pub(crate) fn interactions(file: &Path, strict: bool, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let page_list = document
        .pages()
        .map_err(|error| anyhow::anyhow!("read page tree: {error}"))?;
    let page_ids: Vec<ObjectId> = page_list.iter().map(|page| page.id).collect();
    let mut rows = Vec::new();
    let mut sites = 0;

    let root = document
        .root()
        .map_err(|error| anyhow::anyhow!("read catalog: {error}"))?;
    if let Object::Dictionary(catalog) = document
        .object(root)
        .map_err(|error| anyhow::anyhow!("read catalog object: {error}"))?
        && let Some(Object::Dictionary(additional)) = catalog
            .get("AA")
            .map(|value| resolve(&document, value))
            .transpose()?
    {
        sites += 1;
        collect_site(&document, &page_ids, "catalog", &additional, &mut rows)?;
    }

    for (index, page) in page_list.iter().enumerate() {
        let Object::Dictionary(page_dict) = document
            .object(page.id)
            .map_err(|error| anyhow::anyhow!("read page object: {error}"))?
        else {
            continue;
        };
        if let Some(Object::Dictionary(additional)) = page_dict
            .get("AA")
            .map(|value| resolve(&document, value))
            .transpose()?
        {
            sites += 1;
            collect_site(
                &document,
                &page_ids,
                &format!("page {}", index + 1),
                &additional,
                &mut rows,
            )?;
        }
        let Some(annots) = page_dict.get("Annots") else {
            continue;
        };
        let Object::Array(annotations) = resolve(&document, annots)? else {
            continue;
        };
        for annotation in &annotations {
            let number = annotation.as_reference().map_or(0, |id| id.number);
            let Object::Dictionary(annot) = resolve(&document, annotation)? else {
                continue;
            };
            if let Some(Object::Dictionary(additional)) = annot
                .get("AA")
                .map(|value| resolve(&document, value))
                .transpose()?
            {
                sites += 1;
                collect_site(
                    &document,
                    &page_ids,
                    &format!("page {} annotation {number}", index + 1),
                    &additional,
                    &mut rows,
                )?;
            }
        }
    }

    let accepted = rows.iter().filter(|row| row.status == "ok").count();
    let rejected = rows.len() - accepted;
    let report = InteractionsReport {
        file: file.display().to_string(),
        sites,
        actions: rows.len(),
        accepted,
        rejected,
        rows,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "file: {}  sites: {}  actions: {}  accepted: {}  rejected: {}",
            report.file, report.sites, report.actions, report.accepted, report.rejected
        );
        for row in &report.rows {
            let detail = row
                .code
                .clone()
                .or_else(|| row.error.clone())
                .unwrap_or_default();
            println!(
                "  {}  {}  {}  {}  {}{}",
                row.site,
                row.event,
                row.kind,
                row.status,
                detail,
                row.target
                    .as_ref()
                    .map_or_else(String::new, |target| format!(" -> {target}"))
            );
        }
    }
    if strict && rejected > 0 {
        bail!("{rejected} additional action(s) rejected by the safe-action whitelist");
    }
    Ok(())
}
