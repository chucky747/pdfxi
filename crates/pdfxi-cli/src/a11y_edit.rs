//! `pdfxi accessibility alt` and `accessibility table`: edit tagged structure.
//!
//! The `pdfxi structure apply` writer stores each structure element with an
//! `/XI_NodeId`, so a host can address one element without re-emitting the whole
//! tree. This module walks the real `/StructTreeRoot` from the catalog, finds
//! the element with the requested id, and updates `/Alt` or `/Scope` in place.
//! The written document is reopened and the change is read back.

use std::collections::HashSet;
use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::{Dictionary, Document, Object, ObjectId, encode_text_string};
use serde::Serialize;

/// Maximum structure elements visited while resolving one node id.
const MAX_VISITED: usize = 200_000;

/// JSON shape of the two accessibility edit commands.
#[derive(Serialize)]
struct EditReport {
    /// Source PDF.
    file: String,
    /// Written PDF.
    out: String,
    /// Target `/XI_NodeId`.
    node: u64,
    /// Structure role found on the element.
    role: String,
    /// Field that changed: `alt` or `scope`.
    field: &'static str,
    /// New value, or `null` when cleared.
    value: Option<String>,
    /// Bytes written.
    bytes: usize,
}

/// Return a dictionary's integer entry.
fn integer(dictionary: &Dictionary, key: &str) -> Option<i64> {
    match dictionary.get(key) {
        Some(Object::Integer(value)) => Some(*value),
        _ => None,
    }
}

/// Find the structure-element object carrying `/XI_NodeId == node`.
fn find_element(document: &Document, node: u64) -> anyhow::Result<Option<(ObjectId, Dictionary)>> {
    let root = document.root().context("read catalog")?;
    let catalog = document
        .object(root)
        .context("read catalog object")?
        .as_dictionary()
        .cloned()
        .context("catalog is not a dictionary")?;
    let tree_id = catalog
        .get("StructTreeRoot")
        .and_then(Object::as_reference)
        .context("the catalog has no /StructTreeRoot reference")?;
    let tree = document
        .object(tree_id)
        .context("read /StructTreeRoot")?
        .as_dictionary()
        .cloned()
        .context("/StructTreeRoot is not a dictionary")?;

    let mut stack: Vec<ObjectId> = Vec::new();
    if let Some(kids) = tree.get("K").and_then(Object::as_array) {
        for kid in kids {
            if let Some(reference) = kid.as_reference() {
                stack.push(reference);
            }
        }
    } else if let Some(reference) = tree.get("K").and_then(Object::as_reference) {
        stack.push(reference);
    }
    let mut visited = HashSet::new();
    while let Some(id) = stack.pop() {
        if visited.len() >= MAX_VISITED {
            bail!("structure tree exceeds {MAX_VISITED} elements");
        }
        if !visited.insert(id) {
            continue;
        }
        let Ok(object) = document.object(id) else {
            continue;
        };
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        if integer(dictionary, "XI_NodeId") == i64::try_from(node).ok() {
            return Ok(Some((id, dictionary.clone())));
        }
        if let Some(kids) = dictionary.get("K").and_then(Object::as_array) {
            for kid in kids {
                if let Some(reference) = kid.as_reference() {
                    stack.push(reference);
                }
            }
        } else if let Some(reference) = dictionary.get("K").and_then(Object::as_reference) {
            stack.push(reference);
        }
    }
    Ok(None)
}

/// Read the role name (`XI_Role` first, then `/S`).
fn role_name(dictionary: &Dictionary) -> String {
    dictionary
        .get("XI_Role")
        .and_then(Object::as_name)
        .or_else(|| dictionary.get("S").and_then(Object::as_name))
        .unwrap_or("Unknown")
        .to_owned()
}

/// Read a PDF string entry.
fn text_string(object: &Object) -> Option<String> {
    match object {
        Object::String(bytes) => Some(pdfxi_core::decode_text_string(bytes)),
        _ => None,
    }
}

/// `pdfxi accessibility alt`.
pub(crate) fn set_alt(
    file: &Path,
    out: &Path,
    node: u64,
    text: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let (id, mut dictionary) = find_element(&document, node)?
        .with_context(|| format!("no structure element with /XI_NodeId {node}"))?;
    let role = role_name(&dictionary);
    match text {
        Some(text) => {
            if text.trim().is_empty() {
                bail!("--text must not be empty; pass --clear to remove /Alt");
            }
            dictionary.insert("Alt".to_owned(), Object::String(encode_text_string(text)));
            dictionary.insert(
                "XI_Alt".to_owned(),
                Object::String(encode_text_string(text)),
            );
        }
        None => {
            dictionary.remove("Alt");
            dictionary.remove("XI_Alt");
        }
    }
    document
        .set_object(id, Object::Dictionary(dictionary))
        .context("write structure element")?;
    let saved = document
        .save()
        .map_err(|error| anyhow::anyhow!("save {}: {error}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let (_, check) = find_element(&reopened, node)?
        .with_context(|| format!("reopened document lost /XI_NodeId {node}"))?;
    let stored = check.get("Alt").and_then(text_string);
    if stored.as_deref() != text {
        bail!("reopened /Alt is {stored:?}, expected {text:?}");
    }

    let report = EditReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        node,
        role,
        field: "alt",
        value: text.map(str::to_owned),
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("out: {}", report.out);
        println!(
            "node: {}  role: {}  alt: {}",
            report.node,
            report.role,
            report.value.as_deref().unwrap_or("(cleared)")
        );
        println!("bytes: {}", report.bytes);
    }
    Ok(())
}

/// `pdfxi accessibility table`.
pub(crate) fn set_scope(
    file: &Path,
    out: &Path,
    node: u64,
    scope: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let (id, mut dictionary) = find_element(&document, node)?
        .with_context(|| format!("no structure element with /XI_NodeId {node}"))?;
    let role = role_name(&dictionary);
    if !matches!(
        role.to_ascii_lowercase().as_str(),
        "th" | "td" | "tableheader" | "tabledata"
    ) {
        bail!("node {node} has role {role}, not a table header or data cell");
    }
    let canonical = match scope.map(str::to_ascii_lowercase).as_deref() {
        None => None,
        Some("row") => Some("Row"),
        Some("column") | Some("col") => Some("Column"),
        Some("both") => Some("Both"),
        Some(other) => bail!("unknown scope {other:?}; expected row, column or both"),
    };
    match canonical {
        Some(name) => {
            dictionary.insert("Scope".to_owned(), Object::Name(name.to_owned()));
        }
        None => {
            dictionary.remove("Scope");
        }
    }
    document
        .set_object(id, Object::Dictionary(dictionary))
        .context("write table cell")?;
    let saved = document
        .save()
        .map_err(|error| anyhow::anyhow!("save {}: {error}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let (_, check) = find_element(&reopened, node)?
        .with_context(|| format!("reopened document lost /XI_NodeId {node}"))?;
    let stored = check.get("Scope").and_then(Object::as_name);
    if stored != canonical {
        bail!("reopened /Scope is {stored:?}, expected {canonical:?}");
    }

    let report = EditReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        node,
        role,
        field: "scope",
        value: canonical.map(str::to_owned),
        bytes: saved.len(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("out: {}", report.out);
        println!(
            "node: {}  role: {}  scope: {}",
            report.node,
            report.role,
            report.value.as_deref().unwrap_or("(cleared)")
        );
        println!("bytes: {}", report.bytes);
    }
    Ok(())
}
