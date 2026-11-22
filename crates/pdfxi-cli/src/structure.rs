//! `pdfxi structure apply`: write a tagged-PDF structure tree into a document.
//!
//! [`pdfxi_accessibility::write_structure_tree`] turns a validated
//! [`A11yTree`] into a structure intermediate: a `/StructTreeRoot`, a
//! `/ParentTree`, structure elements and per-page marked-content overlays. This
//! host merges that intermediate into a real document: it remaps the generated
//! object numbers, merges the catalog entries (`/StructTreeRoot`, `/MarkInfo`,
//! `/Lang`, `/ViewerPreferences`), appends each page's overlay as an extra
//! content stream and writes `/StructParents` on every page.
//!
//! The tree is supplied as JSON: role names, page indices, optional boxes, text,
//! `/Alt`, `/ActualText`, `/E` and children. The writer and this host validate
//! the tree before touching the document, so an invalid tree leaves it
//! unchanged. No conformance certification is claimed.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_accessibility::{
    A11yNode, A11yTree, BBox, NodeId, ObjRef, PdfValue as StructValue, StructOptions, TagRole,
    ViewerPreferences, write_structure_tree,
};
use pdfxi_core::{Dictionary, Document, Object, ObjectId, encode_text_string};
use serde::{Deserialize, Serialize};

/// First object number used for placeholder page references while the structure
/// tree is written. Real page numbers are well below this bound.
const PLACEHOLDER_BASE: u32 = 1_000_000;

/// One node in the JSON tree input.
#[derive(Clone, Debug, Deserialize)]
struct NodeInput {
    /// Stable node id.
    id: u64,
    /// Role name (`Document`, `P`, `H1`, `Figure`, ...).
    role: String,
    /// Zero-based page index.
    page: usize,
    /// `[x0, y0, x1, y1]`; defaults to a unit box.
    #[serde(default)]
    bbox: Option<[f32; 4]>,
    /// Visible text carried by the node.
    #[serde(default)]
    text: String,
    /// `/Alt` text.
    #[serde(default)]
    alt: Option<String>,
    /// `/ActualText` replacement.
    #[serde(default)]
    actual_text: Option<String>,
    /// `/E` expansion.
    #[serde(default)]
    expansion: Option<String>,
    /// Whether this is a decorative artifact.
    #[serde(default)]
    artifact: bool,
    /// Child node ids in presentation order.
    #[serde(default)]
    children: Vec<u64>,
}

/// JSON tree input.
#[derive(Clone, Debug, Deserialize)]
struct TreeInput {
    /// Catalog `/Lang`; defaults to `en-US`.
    #[serde(default = "default_lang")]
    lang: String,
    /// Nodes in any order.
    nodes: Vec<NodeInput>,
    /// Root node ids in presentation order.
    roots: Vec<u64>,
}

/// Default natural language.
fn default_lang() -> String {
    "en-US".to_owned()
}

/// Map a role name (model or PDF name) to [`TagRole`].
fn parse_role(text: &str) -> anyhow::Result<TagRole> {
    let role = match text.trim() {
        "Document" => TagRole::Document,
        "Page" => TagRole::Page,
        "Part" => TagRole::Part,
        "Art" => TagRole::Art,
        "Sect" => TagRole::Sect,
        "Div" => TagRole::Div,
        "P" | "Paragraph" => TagRole::Paragraph,
        "Span" => TagRole::Span,
        "H1" | "Heading1" => TagRole::Heading1,
        "H2" | "Heading2" => TagRole::Heading2,
        "H3" | "Heading3" => TagRole::Heading3,
        "H4" | "Heading4" => TagRole::Heading4,
        "H5" | "Heading5" => TagRole::Heading5,
        "H6" | "Heading6" => TagRole::Heading6,
        "L" | "List" => TagRole::List,
        "LI" | "ListItem" => TagRole::ListItem,
        "LBody" | "ListBody" => TagRole::ListBody,
        "Table" => TagRole::Table,
        "TR" | "TableRow" => TagRole::TableRow,
        "TH" | "TableHeader" => TagRole::TableHeader,
        "TD" | "TableData" => TagRole::TableData,
        "Figure" => TagRole::Figure,
        "Image" => TagRole::Image,
        "Form" => TagRole::Form,
        "FormField" => TagRole::FormField,
        "Link" => TagRole::Link,
        "Artifact" => TagRole::Artifact,
        "Quote" => TagRole::Quote,
        "Note" => TagRole::Note,
        "BibEntry" => TagRole::BibEntry,
        "Code" => TagRole::Code,
        "Caption" => TagRole::Caption,
        other => bail!("unknown structure role {other:?}"),
    };
    Ok(role)
}

/// Build a validated accessibility tree from the JSON input.
fn build_tree(input: &TreeInput) -> anyhow::Result<A11yTree> {
    let mut nodes = Vec::with_capacity(input.nodes.len());
    for node in &input.nodes {
        let role = parse_role(&node.role)?;
        let bbox = match node.bbox {
            Some([x0, y0, x1, y1]) => BBox::new(x0, y0, x1, y1)
                .map_err(|error| anyhow::anyhow!("node {} bbox: {error}", node.id))?,
            None => BBox::new(0.0, 0.0, 1.0, 1.0)
                .map_err(|error| anyhow::anyhow!("node {} bbox: {error}", node.id))?,
        };
        let mut built = A11yNode::new(
            NodeId::new(node.id),
            role,
            node.page,
            bbox,
            node.text.clone(),
            node.children.iter().copied().map(NodeId::new).collect(),
        );
        if let Some(alt) = &node.alt {
            built = built.with_alt_text(alt);
        }
        if let Some(actual) = &node.actual_text {
            built = built.with_actual_text(actual);
        }
        if let Some(expansion) = &node.expansion {
            built = built.with_expansion(expansion);
        }
        if node.artifact {
            built = built.with_artifact(true);
        }
        nodes.push(built);
    }
    let roots: Vec<NodeId> = input.roots.iter().copied().map(NodeId::new).collect();
    A11yTree::new(nodes, roots).map_err(|error| anyhow::anyhow!("invalid structure tree: {error}"))
}

/// Remap a generated structure value into a core object.
fn remap_value(
    value: &StructValue,
    base: u32,
    generated_max: u32,
    pages: &[ObjectId],
) -> anyhow::Result<Object> {
    Ok(match value {
        StructValue::Null => Object::Null,
        StructValue::Bool(flag) => Object::Boolean(*flag),
        StructValue::Int(number) => Object::Integer(*number),
        StructValue::Real(number) => Object::Real(*number),
        StructValue::Name(name) => Object::Name(name.clone()),
        StructValue::Text(text) => Object::String(encode_text_string(text)),
        StructValue::Array(items) => Object::Array(
            items
                .iter()
                .map(|item| remap_value(item, base, generated_max, pages))
                .collect::<anyhow::Result<Vec<_>>>()?,
        ),
        StructValue::Dict(entries) => Object::Dictionary(
            entries
                .iter()
                .map(|(key, item)| {
                    Ok((key.clone(), remap_value(item, base, generated_max, pages)?))
                })
                .collect::<anyhow::Result<Dictionary>>()?,
        ),
        StructValue::Ref(reference) => {
            if reference.number >= PLACEHOLDER_BASE {
                let index = (reference.number - PLACEHOLDER_BASE) as usize;
                let page = pages.get(index).with_context(|| {
                    format!("placeholder page reference {index} is out of range")
                })?;
                Object::Reference(*page)
            } else {
                let number = if reference.number == 0 {
                    reference.number
                } else if reference.number <= generated_max {
                    base.checked_add(reference.number - 1)
                        .context("structure object number overflow")?
                } else {
                    reference.number
                };
                Object::Reference(ObjectId::new(number, reference.generation))
            }
        }
    })
}

/// Page dictionary together with the shape of its `/Contents`.
struct PageState {
    /// Page object id.
    id: ObjectId,
    /// Page dictionary.
    dictionary: Dictionary,
    /// Original stream data for a stream page object.
    stream_data: Option<Vec<u8>>,
}

/// Read one page's dictionary.
fn read_page(document: &Document, index: usize) -> anyhow::Result<PageState> {
    let page = document
        .page(index)
        .map_err(|error| anyhow::anyhow!("read page {}: {error}", index + 1))?;
    match document.object(page.id)? {
        Object::Dictionary(dictionary) => Ok(PageState {
            id: page.id,
            dictionary,
            stream_data: None,
        }),
        Object::Stream { dictionary, data } => Ok(PageState {
            id: page.id,
            dictionary,
            stream_data: Some(data),
        }),
        _ => bail!("page {} object is not a dictionary", index + 1),
    }
}

/// Report of a successful apply.
#[derive(Serialize)]
struct ApplyReport {
    /// Input file.
    file: String,
    /// Written file.
    out: String,
    /// Catalog `/Lang`.
    lang: String,
    /// Pages in the document.
    pages: usize,
    /// Nodes accepted from JSON.
    nodes: usize,
    /// Structure elements emitted (artifacts excluded).
    struct_elements: usize,
    /// Generated objects staged.
    generated_objects: usize,
    /// `/StructTreeRoot` object number.
    struct_tree_root: u32,
    /// Pages carrying `/StructParents`.
    pages_tagged: usize,
    /// Sorted roles of the saved structure elements.
    roles: Vec<String>,
    /// Always `false`: no conformance certification.
    certification_claimed: bool,
}

/// `pdfxi structure apply`.
pub(crate) fn run_apply(
    file: &Path,
    out: &Path,
    tree_path: &Path,
    json: bool,
) -> anyhow::Result<()> {
    let tree_bytes =
        std::fs::read(tree_path).with_context(|| format!("read tree {}", tree_path.display()))?;
    let input: TreeInput = serde_json::from_slice(&tree_bytes)
        .with_context(|| format!("parse tree {}", tree_path.display()))?;
    if input.nodes.is_empty() {
        bail!("tree has no nodes");
    }
    if input.roots.is_empty() {
        bail!("tree has no roots");
    }
    let tree = build_tree(&input)?;

    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let page_count = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    if page_count == 0 {
        bail!("{} has no pages", file.display());
    }
    let page_ids: Vec<ObjectId> = (0..page_count)
        .map(|index| document.page(index).map(|page| page.id))
        .collect::<Result<_, _>>()
        .map_err(|error| anyhow::anyhow!("read page tree: {error}"))?;

    let placeholders: Vec<ObjRef> = (0..page_count)
        .map(|index| ObjRef::new(PLACEHOLDER_BASE + index as u32))
        .collect();
    let options = StructOptions {
        lang: input.lang.clone(),
        page_count,
        page_objects: placeholders,
        viewer_preferences: ViewerPreferences::default(),
    };
    let structure = write_structure_tree(&tree, &options)
        .map_err(|error| anyhow::anyhow!("write structure tree: {error}"))?;
    let generated_max = structure
        .objects
        .iter()
        .map(|(number, _)| *number)
        .max()
        .unwrap_or(0);
    let base = document
        .allocate_object_number()
        .map_err(|error| anyhow::anyhow!("allocate structure objects: {error}"))?;
    if base == 0 {
        bail!("object number space is exhausted");
    }
    base.checked_add(generated_max)
        .context("structure object numbers overflow")?;

    for (number, value) in &structure.objects {
        let remapped = remap_value(value, base, generated_max, &page_ids)?;
        let id = ObjectId::new(base + number.saturating_sub(1), 0);
        document
            .set_object(id, remapped)
            .map_err(|error| anyhow::anyhow!("stage structure object {number}: {error}"))?;
    }

    let root_id = document
        .root()
        .map_err(|error| anyhow::anyhow!("read catalog: {error}"))?;
    let mut catalog = document
        .object(root_id)
        .map_err(|error| anyhow::anyhow!("load catalog: {error}"))?
        .as_dictionary()
        .context("catalog is not a dictionary")?
        .clone();
    for (key, value) in &structure.catalog_entries {
        catalog.insert(
            key.clone(),
            remap_value(value, base, generated_max, &page_ids)?,
        );
    }
    document
        .set_object(root_id, Object::Dictionary(catalog))
        .map_err(|error| anyhow::anyhow!("stage catalog: {error}"))?;

    let struct_root = ObjectId::new(base, 0);
    let mut pages_tagged = 0;
    for (index, info) in structure.pages.iter().enumerate() {
        let mut page = read_page(&document, index)?;
        page.dictionary.insert(
            "StructParents".to_owned(),
            Object::Integer(i64::from(info.struct_parents)),
        );
        if !info.overlay_content.is_empty() {
            let overlay_number = document
                .allocate_object_number()
                .map_err(|error| anyhow::anyhow!("allocate overlay stream: {error}"))?;
            let overlay_id = ObjectId::new(overlay_number, 0);
            let mut stream = Dictionary::new();
            stream.insert(
                "Length".to_owned(),
                Object::Integer(i64::try_from(info.overlay_content.len()).unwrap_or(i64::MAX)),
            );
            document
                .set_object(
                    overlay_id,
                    Object::Stream {
                        dictionary: stream,
                        data: info.overlay_content.clone(),
                    },
                )
                .map_err(|error| anyhow::anyhow!("stage overlay stream: {error}"))?;
            let contents = match page.dictionary.get("Contents") {
                None | Some(Object::Null) => Object::Reference(overlay_id),
                Some(Object::Reference(id)) => {
                    Object::Array(vec![Object::Reference(*id), Object::Reference(overlay_id)])
                }
                Some(Object::Array(items)) => {
                    let mut items = items.clone();
                    items.push(Object::Reference(overlay_id));
                    Object::Array(items)
                }
                Some(_) => bail!("/Contents on page {} is not a stream or array", index + 1),
            };
            page.dictionary.insert("Contents".to_owned(), contents);
        }
        let object = match page.stream_data {
            Some(data) => Object::Stream {
                dictionary: page.dictionary,
                data,
            },
            None => Object::Dictionary(page.dictionary),
        };
        document
            .set_object(page.id, object)
            .map_err(|error| anyhow::anyhow!("stage page {}: {error}", index + 1))?;
        pages_tagged += 1;
    }

    let source_texts: Vec<String> = (0..page_count)
        .map(|index| document.page_text(index))
        .collect::<Result<_, _>>()
        .map_err(|error| anyhow::anyhow!("read page text: {error}"))?;

    let saved = document
        .save()
        .with_context(|| format!("save {}", out.display()))?;
    std::fs::write(out, &saved).with_context(|| format!("write {}", out.display()))?;

    let reopened = Document::from_bytes(saved)
        .map_err(|error| anyhow::anyhow!("reopen {}: {error}", out.display()))?;
    let catalog = reopened
        .object(
            reopened
                .root()
                .map_err(|error| anyhow::anyhow!("read saved catalog: {error}"))?,
        )
        .map_err(|error| anyhow::anyhow!("load saved catalog: {error}"))?
        .as_dictionary()
        .context("saved catalog is not a dictionary")?
        .clone();
    let struct_tree_root = catalog
        .get("StructTreeRoot")
        .and_then(Object::as_reference)
        .context("saved catalog has no /StructTreeRoot reference")?;
    let mark_info = catalog
        .get("MarkInfo")
        .and_then(|value| value.as_dictionary())
        .context("saved catalog has no /MarkInfo dictionary")?;
    /// Walk the saved structure tree and return the roles of its elements.
    ///
    /// Every `/StructElem` must carry `/S`, `/P` and `/Pg`; the walk is bounded by a
    /// visited set so a malformed tree cannot loop forever.
    fn saved_structure_roles(
        document: &Document,
        root: ObjectId,
        expected: usize,
    ) -> anyhow::Result<Vec<String>> {
        let root_object = document.object(root)?;
        let root_dictionary = root_object
            .as_dictionary()
            .context("saved /StructTreeRoot is not a dictionary")?;
        if root_dictionary.get("Type").and_then(Object::as_name) != Some("StructTreeRoot") {
            bail!("saved /StructTreeRoot has no /Type /StructTreeRoot");
        }
        let parent_tree = root_dictionary
            .get("ParentTree")
            .and_then(Object::as_reference)
            .context("saved /StructTreeRoot has no /ParentTree reference")?;
        let parent_dictionary = document.object(parent_tree)?;
        let parent_dictionary = parent_dictionary
            .as_dictionary()
            .context("saved /ParentTree is not a dictionary")?;
        let nums = parent_dictionary
            .get("Nums")
            .and_then(|value| match value {
                Object::Array(items) => Some(items),
                _ => None,
            })
            .context("saved /ParentTree has no /Nums array")?;
        if nums.is_empty() {
            bail!("saved /ParentTree /Nums is empty");
        }
        let kids = root_dictionary
            .get("K")
            .context("saved /StructTreeRoot has no /K")?
            .clone();
        let mut roles = Vec::new();
        let mut visited = std::collections::BTreeSet::new();
        collect_roles(document, &kids, &mut roles, &mut visited, 0)?;
        if roles.len() != expected {
            bail!(
                "saved tree holds {} element(s) but the writer staged {expected}",
                roles.len()
            );
        }
        for role in &roles {
            if !is_known_role(role) {
                bail!("saved structure element has unknown role {role:?}");
            }
        }
        roles.sort();
        Ok(roles)
    }

    /// Recursively collect `/S` names from structure elements.
    fn collect_roles(
        document: &Document,
        value: &Object,
        roles: &mut Vec<String>,
        visited: &mut std::collections::BTreeSet<ObjectId>,
        depth: usize,
    ) -> anyhow::Result<()> {
        if depth > 64 {
            bail!("structure tree is deeper than 64 levels");
        }
        match value {
            Object::Array(items) => {
                for item in items {
                    collect_roles(document, item, roles, visited, depth + 1)?;
                }
                Ok(())
            }
            Object::Reference(id) => {
                if !visited.insert(*id) {
                    return Ok(());
                }
                let object = document.object(*id)?;
                collect_roles(document, &object, roles, visited, depth + 1)
            }
            Object::Dictionary(dictionary) => {
                match dictionary.get("Type").and_then(Object::as_name) {
                    Some("StructElem") => {
                        let role = dictionary
                            .get("S")
                            .and_then(Object::as_name)
                            .with_context(|| "structure element has no /S role")?;
                        if dictionary.get("P").is_none() {
                            bail!("structure element {role} has no /P parent");
                        }
                        let page = dictionary
                            .get("Pg")
                            .and_then(Object::as_reference)
                            .with_context(|| format!("structure element {role} has no /Pg page"))?;
                        if document.page(0).map(|page| page.id).ok() != Some(page)
                            && (0..document.page_count().unwrap_or(0))
                                .filter_map(|index| document.page(index).ok())
                                .all(|candidate| candidate.id != page)
                        {
                            bail!("structure element {role} points at a non-page object");
                        }
                        roles.push(role.to_owned());
                        if let Some(kids) = dictionary.get("K") {
                            collect_roles(document, kids, roles, visited, depth + 1)?;
                        }
                        Ok(())
                    }
                    Some("MCR") | Some("OBJR") => Ok(()),
                    _ => {
                        if let Some(kids) = dictionary.get("K") {
                            collect_roles(document, kids, roles, visited, depth + 1)?;
                        }
                        Ok(())
                    }
                }
            }
            _ => Ok(()),
        }
    }

    /// Whether `role` is a structure role this host accepts.
    fn is_known_role(role: &str) -> bool {
        matches!(
            role,
            "Document"
                | "Part"
                | "Art"
                | "Sect"
                | "Div"
                | "P"
                | "Span"
                | "H1"
                | "H2"
                | "H3"
                | "H4"
                | "H5"
                | "H6"
                | "L"
                | "LI"
                | "LBody"
                | "Table"
                | "TR"
                | "TH"
                | "TD"
                | "Figure"
                | "Formula"
                | "Form"
                | "Link"
                | "Quote"
                | "Note"
                | "BibEntry"
                | "Code"
                | "Caption"
        )
    }
    if !matches!(mark_info.get("Marked"), Some(Object::Boolean(true))) {
        bail!("saved /MarkInfo does not mark the document as tagged");
    }
    if !catalog.contains_key("Lang") {
        bail!("saved catalog has no /Lang");
    }
    if struct_tree_root != struct_root {
        bail!(
            "saved /StructTreeRoot is {} but the staged root is {}",
            struct_tree_root.number,
            struct_root.number
        );
    }
    let roles = saved_structure_roles(&reopened, struct_tree_root, structure.struct_elem_count())?;
    for (index, source_text) in source_texts.iter().enumerate() {
        let page = reopened
            .page(index)
            .map_err(|error| anyhow::anyhow!("read saved page {}: {error}", index + 1))?;
        let dictionary = reopened
            .object(page.id)
            .map_err(|error| anyhow::anyhow!("load saved page {}: {error}", index + 1))?;
        let dictionary = dictionary
            .as_dictionary()
            .context("saved page is not a dictionary")?;
        if dictionary
            .get("StructParents")
            .and_then(Object::as_i64)
            .is_none()
        {
            bail!("saved page {} has no /StructParents", index + 1);
        }
        if reopened.page_text(index).ok().as_deref() != Some(source_text.as_str()) {
            bail!("page {} text changed while tagging", index + 1);
        }
    }

    let report = ApplyReport {
        file: file.display().to_string(),
        out: out.display().to_string(),
        lang: input.lang.clone(),
        pages: page_count,
        nodes: input.nodes.len(),
        struct_elements: structure.struct_elem_count(),
        generated_objects: structure.objects.len(),
        struct_tree_root: struct_tree_root.number,
        pages_tagged,
        roles,
        certification_claimed: false,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!("file: {}", report.file);
    println!("out:  {}", report.out);
    println!("lang: {}", report.lang);
    println!("pages: {}", report.pages);
    println!("nodes: {}", report.nodes);
    println!("struct_elements: {}", report.struct_elements);
    println!("generated_objects: {}", report.generated_objects);
    println!("struct_tree_root: {}", report.struct_tree_root);
    println!("pages_tagged: {}", report.pages_tagged);
    println!("roles: {}", report.roles.join(","));
    println!("certification_claimed: false");
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::parse_role;
    use pdfxi_accessibility::TagRole;

    #[test]
    fn roles_parse_model_and_pdf_names() {
        assert_eq!(parse_role("P").expect("P"), TagRole::Paragraph);
        assert_eq!(parse_role("H1").expect("H1"), TagRole::Heading1);
        assert_eq!(parse_role("TD").expect("TD"), TagRole::TableData);
        assert_eq!(parse_role("Figure").expect("fig"), TagRole::Figure);
        assert!(parse_role("Widget").is_err());
    }
}
