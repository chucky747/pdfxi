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

/// Return the structure-tree root id and dictionary.
fn structure_root(document: &Document) -> anyhow::Result<(ObjectId, Dictionary)> {
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
    Ok((tree_id, tree))
}

/// Recursively append one element and its children in reading order.
fn walk_order(
    document: &Document,
    object: &Object,
    order: &mut Vec<(u64, String)>,
    visited: &mut HashSet<ObjectId>,
) {
    let Some(dictionary) = object.as_dictionary() else {
        return;
    };
    if let Some(id) = integer(dictionary, "XI_NodeId")
        && let Ok(id) = u64::try_from(id)
    {
        order.push((id, role_name(dictionary)));
    }
    if let Some(kids) = dictionary.get("K").and_then(Object::as_array) {
        for kid in kids {
            if let Some(reference) = kid.as_reference()
                && visited.insert(reference)
                && let Ok(object) = document.object(reference)
            {
                walk_order(document, &object, order, visited);
            }
        }
    }
}

/// Collect the pre-order reading order as `(node id, role)` pairs.
fn reading_order(document: &Document) -> anyhow::Result<Vec<(u64, String)>> {
    let (_, tree) = structure_root(document)?;
    let mut order = Vec::new();
    let mut visited = HashSet::new();
    if let Some(kids) = tree.get("K").and_then(Object::as_array) {
        for kid in kids {
            if let Some(reference) = kid.as_reference()
                && visited.insert(reference)
                && let Ok(object) = document.object(reference)
            {
                walk_order(document, &object, &mut order, &mut visited);
            }
        }
    }
    Ok(order)
}
/// Find the dictionary whose `/K` array directly references `child`.
fn find_owner(
    document: &Document,
    child: ObjectId,
) -> anyhow::Result<Option<(ObjectId, Dictionary)>> {
    let (tree_id, tree) = structure_root(document)?;
    if tree
        .get("K")
        .and_then(Object::as_array)
        .is_some_and(|kids| kids.iter().any(|kid| kid.as_reference() == Some(child)))
    {
        return Ok(Some((tree_id, tree.clone())));
    }
    let mut stack: Vec<ObjectId> = tree
        .get("K")
        .and_then(Object::as_array)
        .map(|kids| kids.iter().filter_map(Object::as_reference).collect())
        .unwrap_or_default();
    let mut visited = HashSet::new();
    while let Some(id) = stack.pop() {
        if !visited.insert(id) {
            continue;
        }
        let Ok(object) = document.object(id) else {
            continue;
        };
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        if let Some(kids) = dictionary.get("K").and_then(Object::as_array) {
            if kids.iter().any(|kid| kid.as_reference() == Some(child)) {
                return Ok(Some((id, dictionary.clone())));
            }
            for kid in kids {
                if let Some(reference) = kid.as_reference() {
                    stack.push(reference);
                }
            }
        }
    }
    Ok(None)
}

/// `pdfxi accessibility order`.
pub(crate) fn show_order(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let order = reading_order(&document)?;
    if json {
        let rows: Vec<serde_json::Value> = order
            .iter()
            .map(|(id, role)| serde_json::json!({ "node": id, "role": role }))
            .collect();
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "file": file.display().to_string(),
                "count": rows.len(),
                "order": rows,
            }))?
        );
        return Ok(());
    }
    println!("file: {}", file.display());
    for (index, (id, role)) in order.iter().enumerate() {
        println!("{index}\t{id}\t{role}");
    }
    println!("nodes: {}", order.len());
    Ok(())
}

/// `pdfxi accessibility move`.
pub(crate) fn move_node(
    file: &Path,
    out: &Path,
    node: u64,
    before: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let (node_id, _) = find_element(&document, node)?
        .with_context(|| format!("no structure element with /XI_NodeId {node}"))?;
    let before_id = match before {
        Some(before) => Some(
            find_element(&document, before)?
                .with_context(|| format!("no structure element with /XI_NodeId {before}"))?
                .0,
        ),
        None => None,
    };
    let (owner_id, mut owner) = find_owner(&document, node_id)?
        .with_context(|| format!("node {node} has no owning /K array"))?;
    let mut kids: Vec<Object> = owner
        .get("K")
        .and_then(Object::as_array)
        .map(<[Object]>::to_vec)
        .context("owner /K is not an array")?;
    let position = kids
        .iter()
        .position(|kid| kid.as_reference() == Some(node_id))
        .context("owner /K does not contain the node")?;
    let moved = kids.remove(position);
    match before_id {
        Some(before_id) => {
            let index = kids
                .iter()
                .position(|kid| kid.as_reference() == Some(before_id))
                .with_context(|| format!("node {before:?} is not a sibling"))?;
            kids.insert(index, moved);
        }
        None => kids.push(moved),
    }
    owner.insert("K".to_owned(), Object::Array(kids));
    document
        .set_object(owner_id, Object::Dictionary(owner))
        .context("write reordered /K")?;
    let saved = document
        .save()
        .map_err(|error| anyhow::anyhow!("save {}: {error}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved.clone())
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let order = reading_order(&reopened)?;
    let node_index = order
        .iter()
        .position(|(id, _)| *id == node)
        .with_context(|| format!("reopened order lost node {node}"))?;
    if let Some(before) = before {
        let before_index = order
            .iter()
            .position(|(id, _)| *id == before)
            .with_context(|| format!("reopened order lost node {before}"))?;
        if node_index >= before_index {
            bail!("node {node} is not before {before} in the reopened order");
        }
    }

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({
                "file": file.display().to_string(),
                "out": out.display().to_string(),
                "node": node,
                "before": before,
                "position": node_index,
                "order": order.iter().map(|(id, _)| *id).collect::<Vec<u64>>(),
                "bytes": saved.len(),
            }))?
        );
        return Ok(());
    }
    println!("out: {}", out.display());
    println!("node: {node}  before: {before:?}  position: {node_index}");
    println!(
        "order: {}",
        order
            .iter()
            .map(|(id, _)| id.to_string())
            .collect::<Vec<String>>()
            .join(",")
    );
    println!("bytes: {}", saved.len());
    Ok(())
}
