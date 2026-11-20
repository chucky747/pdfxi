//! Writer and reader for tagged-PDF structure trees derived from an [`A11yTree`].
//!
//! [`write_structure_tree`] maps a validated [`A11yTree`] into the indirect
//! objects of a tagged PDF: a `/StructTreeRoot` with ordered `/K` kids, one
//! `/StructElem` per non-artifact accessibility node, a `/ParentTree` number
//! tree plus `/ParentTreeNextKey`, a `/RoleMap` for non-standard roles, `/Alt`
//! for figures and images, `/ActualText` replacement text, `/E` abbreviation
//! expansions, per-page `/StructParents` keys, and marked-content identifiers
//! (`/MCID`) embedded in additive page-content overlays. Artifact nodes are
//! *not* written as structure elements; their marked-content sequences are
//! emitted with the `/Artifact` tag so assistive technology skips them.
//!
//! The writer is deliberately *intermediate*: it never opens or rewrites a
//! concrete PDF file. It returns a [`StructDocument`] holding numbered PDF
//! objects, catalog-level entries (`/Lang`, `/MarkInfo`,
//! `/ViewerPreferences`, `/StructTreeRoot`), and one [`PageTagInfo`] per page
//! with the overlay bytes a core writer should append. Existing page content
//! is preserved verbatim by [`compose_page_content`], which only appends.
//!
//! [`read_structure_tree`] is the inverse: it reconstructs the exact
//! [`A11yTree`] (ids, roles, pages, boxes, text, alt text, and children) from
//! the generated objects, which makes write/read round-trips verifiable.
//!
//! The module is dependency-free: it defines its own small [`PdfValue`]
//! intermediate instead of depending on another crate. All public items are
//! rustdoc'd, the code denies missing docs, forbids unsafe, and uses no
//! `unwrap`/`expect`/`panic` outside tests.

use core::fmt;
use std::collections::{BTreeMap, HashMap, HashSet};

use crate::{A11yNode, A11yTree, BBox, NodeId, TagRole, TreeError};

/// A stable reference to an indirect PDF object (`number generation R`).
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct ObjRef {
    /// Object number, always positive for a real indirect object.
    pub number: u32,
    /// Generation number; generated objects use generation zero.
    pub generation: u16,
}

impl ObjRef {
    /// Creates a generation-zero reference to `number`.
    pub const fn new(number: u32) -> Self {
        Self {
            number,
            generation: 0,
        }
    }

    /// Creates a reference with an explicit generation number.
    pub const fn with_generation(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }

    /// Returns `true` when this reference is null (object number zero).
    pub const fn is_direct(&self) -> bool {
        self.number == 0
    }

    fn write(&self, out: &mut String) {
        out.push_str(&self.number.to_string());
        out.push(' ');
        out.push_str(&self.generation.to_string());
        out.push_str(" R");
    }
}

impl fmt::Display for ObjRef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

/// A dependency-free PDF value used as the structure-tree serialization model.
///
/// Names and dictionary keys are stored **without** the leading slash. The
/// model supports exactly the object kinds a structure tree needs.
#[derive(Clone, Debug, PartialEq)]
pub enum PdfValue {
    /// The `null` object.
    Null,
    /// A Boolean object.
    Bool(bool),
    /// An integer object.
    Int(i64),
    /// A real object.
    Real(f64),
    /// A name object (without the leading slash).
    Name(String),
    /// A literal string object.
    Text(String),
    /// An ordered array of values.
    Array(Vec<PdfValue>),
    /// A dictionary with deterministic key order.
    Dict(PdfDict),
    /// An indirect reference.
    Ref(ObjRef),
}

/// A PDF dictionary: names to values with deterministic, sorted key order.
pub type PdfDict = BTreeMap<String, PdfValue>;

impl PdfValue {
    /// Returns the dictionary when this value is a [`Self::Dict`].
    pub fn as_dict(&self) -> Option<&PdfDict> {
        match self {
            Self::Dict(dict) => Some(dict),
            _ => None,
        }
    }

    /// Returns the items when this value is an [`Self::Array`].
    pub fn as_array(&self) -> Option<&[PdfValue]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Returns the name when this value is a [`Self::Name`].
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    /// Returns the text when this value is a [`Self::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Returns the integer when this value is a [`Self::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the Boolean when this value is a [`Self::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the number when this value is a [`Self::Int`] or [`Self::Real`].
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Int(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the reference when this value is a [`Self::Ref`].
    pub fn as_ref(&self) -> Option<ObjRef> {
        match self {
            Self::Ref(reference) => Some(*reference),
            _ => None,
        }
    }

    /// Serializes this value into deterministic PDF object syntax.
    pub fn to_pdf_bytes(&self) -> Vec<u8> {
        let mut out = String::new();
        self.write(&mut out);
        out.into_bytes()
    }

    fn write(&self, out: &mut String) {
        match self {
            Self::Null => out.push_str("null"),
            Self::Bool(value) => out.push_str(if *value { "true" } else { "false" }),
            Self::Int(value) => out.push_str(&value.to_string()),
            Self::Real(value) => {
                // Validated trees carry finite coordinates; the fallback keeps
                // serialization total even if a caller builds a value directly.
                if value.is_finite() {
                    out.push_str(&value.to_string());
                } else {
                    out.push('0');
                }
            }
            Self::Name(name) => write_name(name, out),
            Self::Text(text) => write_literal(text, out),
            Self::Array(items) => {
                out.push('[');
                for item in items {
                    item.write(out);
                    out.push(' ');
                }
                out.push(']');
            }
            Self::Dict(dict) => {
                out.push_str("<<");
                for (key, value) in dict {
                    write_name(key, out);
                    out.push(' ');
                    value.write(out);
                    out.push(' ');
                }
                out.push_str(">>");
            }
            Self::Ref(reference) => reference.write(out),
        }
    }
}

fn write_name(name: &str, out: &mut String) {
    out.push('/');
    for &byte in name.as_bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.') {
            out.push(byte as char);
        } else {
            out.push_str(&format!("#{byte:02X}"));
        }
    }
}

fn write_literal(text: &str, out: &mut String) {
    out.push('(');
    for &byte in text.as_bytes() {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push('\\');
                out.push(byte as char);
            }
            0x20..=0x7e => out.push(byte as char),
            _ => out.push_str(&format!("\\{:03o}", byte)),
        }
    }
    out.push(')');
}

/// Viewer-preference flags written to the catalog `/ViewerPreferences` entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ViewerPreferences {
    /// Display the document title instead of the file name (`/DisplayDocTitle`).
    pub display_doc_title: bool,
    /// Hide the viewer toolbar (`/HideToolbar`).
    pub hide_toolbar: bool,
    /// Hide the viewer menu bar (`/HideMenubar`).
    pub hide_menubar: bool,
    /// Resize the window to the first page (`/FitWindow`).
    pub fit_window: bool,
}

impl Default for ViewerPreferences {
    fn default() -> Self {
        Self {
            display_doc_title: true,
            hide_toolbar: false,
            hide_menubar: false,
            fit_window: false,
        }
    }
}

impl ViewerPreferences {
    /// Converts these flags into a `/ViewerPreferences` dictionary value.
    pub fn to_pdf_value(self) -> PdfValue {
        let mut dict = PdfDict::new();
        if self.display_doc_title {
            dict.insert("DisplayDocTitle".into(), PdfValue::Bool(true));
        }
        if self.hide_toolbar {
            dict.insert("HideToolbar".into(), PdfValue::Bool(true));
        }
        if self.hide_menubar {
            dict.insert("HideMenubar".into(), PdfValue::Bool(true));
        }
        if self.fit_window {
            dict.insert("FitWindow".into(), PdfValue::Bool(true));
        }
        PdfValue::Dict(dict)
    }
}

/// Options controlling structure-tree generation.
#[derive(Clone, Debug, PartialEq)]
pub struct StructOptions {
    /// Natural language written as the catalog `/Lang` entry (for example `en-US`).
    pub lang: String,
    /// Number of pages in the target document.
    pub page_count: usize,
    /// Indirect references to the page objects, one per page in document order.
    pub page_objects: Vec<ObjRef>,
    /// Viewer-preference flags written to the catalog.
    pub viewer_preferences: ViewerPreferences,
}

impl Default for StructOptions {
    fn default() -> Self {
        Self {
            lang: "en-US".to_string(),
            page_count: 0,
            page_objects: Vec::new(),
            viewer_preferences: ViewerPreferences::default(),
        }
    }
}

impl StructOptions {
    /// Validates that language and page metadata are internally consistent.
    pub fn validate(&self) -> Result<(), StructError> {
        if self.lang.trim().is_empty() {
            return Err(StructError::InvalidOptions(
                "language (/Lang) must not be empty".to_string(),
            ));
        }
        if self.page_objects.len() != self.page_count {
            return Err(StructError::InvalidOptions(format!(
                "page_objects has {} entries but page_count is {}",
                self.page_objects.len(),
                self.page_count
            )));
        }
        for reference in &self.page_objects {
            if reference.is_direct() {
                return Err(StructError::InvalidOptions(
                    "page object numbers must be positive".to_string(),
                ));
            }
        }
        Ok(())
    }
}

/// Typed errors produced while writing or reading a tagged structure tree.
#[derive(Debug, Clone, PartialEq)]
pub enum StructError {
    /// The supplied accessibility tree failed validation.
    InvalidTree(TreeError),
    /// The writer options were inconsistent.
    InvalidOptions(String),
    /// A node referenced a page outside the supplied page range.
    PageOutOfRange {
        /// Zero-based page index of the offending node.
        page: usize,
        /// Total number of pages supplied in the options.
        page_count: usize,
    },
    /// Two generated objects shared an object number.
    DuplicateObject(u32),
    /// A referenced indirect object was not present in the document.
    MissingObject(u32),
    /// A `/StructElem` dictionary was missing or had a malformed field.
    BadStructElem(String),
    /// A role name could not be mapped back to a [`TagRole`].
    UnknownRole(String),
    /// `/ActualText` was attached to a node that has children.
    ActualTextOnNonLeaf(NodeId),
    /// `/ActualText` was an empty or whitespace-only string.
    EmptyActualText(NodeId),
    /// An artifact node contained a non-artifact (structure) child.
    ArtifactHasStructureKids(NodeId),
    /// An abbreviation expansion was attached to a role that forbids it.
    ExpansionForbiddenRole(NodeId, TagRole),
    /// Two marked-content sequences were assigned the same `/MCID`.
    DuplicateMcid(u32),
}

impl fmt::Display for StructError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidTree(error) => write!(f, "invalid accessibility tree: {error}"),
            Self::InvalidOptions(message) => write!(f, "invalid structure options: {message}"),
            Self::PageOutOfRange { page, page_count } => {
                write!(
                    f,
                    "node page {page} is outside the document ({page_count} pages)"
                )
            }
            Self::DuplicateObject(number) => write!(f, "duplicate object number {number}"),
            Self::MissingObject(number) => write!(f, "missing object number {number}"),
            Self::BadStructElem(message) => write!(f, "malformed structure element: {message}"),
            Self::UnknownRole(name) => write!(f, "unknown structure role /{name}"),
            Self::ActualTextOnNonLeaf(id) => {
                write!(f, "node {id} has /ActualText but is not a leaf")
            }
            Self::EmptyActualText(id) => write!(f, "node {id} has empty /ActualText"),
            Self::ArtifactHasStructureKids(id) => {
                write!(f, "artifact node {id} contains structure children")
            }
            Self::ExpansionForbiddenRole(id, role) => {
                write!(f, "node {id} has /E but role {role} forbids expansions")
            }
            Self::DuplicateMcid(mcid) => write!(f, "duplicate marked-content identifier {mcid}"),
        }
    }
}

impl std::error::Error for StructError {}

/// Per-page tagging information produced by the writer.
#[derive(Clone, Debug, PartialEq)]
pub struct PageTagInfo {
    /// Zero-based page index this information belongs to.
    pub page: usize,
    /// Indirect reference to the page object.
    pub page_object: ObjRef,
    /// Value the page dictionary should carry as `/StructParents`.
    pub struct_parents: u32,
    /// Additive content-stream bytes containing the page's marked-content markers.
    pub overlay_content: Vec<u8>,
    /// Marked-content identifiers allocated on this page, in order.
    pub mcid_ids: Vec<u32>,
    /// Subset of [`Self::mcid_ids`] that were emitted as `/Artifact` markers.
    pub artifact_mcids: Vec<u32>,
}

/// Complete tagged-structure intermediate produced by [`write_structure_tree`].
#[derive(Clone, Debug, PartialEq)]
pub struct StructDocument {
    /// All generated indirect objects, sorted by object number.
    pub objects: Vec<(u32, PdfValue)>,
    /// Reference to the generated `/StructTreeRoot`.
    pub struct_tree_root: ObjRef,
    /// Reference to the generated `/ParentTree` number tree.
    pub parent_tree: ObjRef,
    /// Value of `/ParentTreeNextKey`.
    pub parent_tree_next_key: u32,
    /// One tagging record per page, in document order.
    pub pages: Vec<PageTagInfo>,
    /// Catalog-level entries to merge into the document catalog.
    pub catalog_entries: PdfDict,
}

impl StructDocument {
    /// Returns the generated object with the given reference, when present.
    pub fn object(&self, reference: ObjRef) -> Option<&PdfValue> {
        self.objects
            .iter()
            .find(|(number, _)| *number == reference.number)
            .map(|(_, value)| value)
    }

    /// Returns the number of generated `/StructElem` objects.
    pub fn struct_elem_count(&self) -> usize {
        self.objects
            .iter()
            .filter(|(_, value)| {
                value
                    .as_dict()
                    .and_then(|dict| dict.get("Type"))
                    .and_then(PdfValue::as_name)
                    == Some("StructElem")
            })
            .count()
    }

    /// Returns the overlay bytes for `page`, when that page exists.
    pub fn overlay_for_page(&self, page: usize) -> Option<&[u8]> {
        self.pages
            .get(page)
            .map(|info| info.overlay_content.as_slice())
    }

    /// Serializes every generated object into deterministic PDF object syntax.
    pub fn to_pdf_bytes(&self) -> Vec<u8> {
        let mut out = String::from("%PDFXI tagged structure\n");
        for (number, value) in &self.objects {
            out.push_str(&number.to_string());
            out.push_str(" 0 obj\n");
            value.write(&mut out);
            out.push_str("\nendobj\n");
        }
        out.into_bytes()
    }
}

/// The [`TagRole`] variants, in a stable order used for reverse lookups.
const ALL_ROLES: &[TagRole] = &[
    TagRole::Document,
    TagRole::Page,
    TagRole::Part,
    TagRole::Art,
    TagRole::Sect,
    TagRole::Div,
    TagRole::Paragraph,
    TagRole::Span,
    TagRole::Heading1,
    TagRole::Heading2,
    TagRole::Heading3,
    TagRole::Heading4,
    TagRole::Heading5,
    TagRole::Heading6,
    TagRole::List,
    TagRole::ListItem,
    TagRole::ListBody,
    TagRole::Table,
    TagRole::TableRow,
    TagRole::TableHeader,
    TagRole::TableData,
    TagRole::Figure,
    TagRole::Image,
    TagRole::Form,
    TagRole::FormField,
    TagRole::Link,
    TagRole::Artifact,
    TagRole::Quote,
    TagRole::Note,
    TagRole::BibEntry,
    TagRole::Code,
    TagRole::Caption,
];

/// Standard ISO 32000 structure-tag names that need no `/RoleMap` entry.
const STANDARD_ROLE_NAMES: &[&str] = &[
    "Document", "Part", "Art", "Sect", "Div", "P", "Span", "H1", "H2", "H3", "H4", "H5", "H6", "L",
    "LI", "LBody", "Table", "TR", "TH", "TD", "Figure", "Form", "Link", "Quote", "Note",
    "BibEntry", "Code",
];

/// Maps a [`TagRole`] to the PDF name used in the `/S` entry of a structure element.
///
/// Every role maps to a distinct name, so the mapping is reversible.
pub fn role_pdf_name(role: TagRole) -> &'static str {
    match role {
        TagRole::Document => "Document",
        TagRole::Page => "Page",
        TagRole::Part => "Part",
        TagRole::Art => "Art",
        TagRole::Sect => "Sect",
        TagRole::Div => "Div",
        TagRole::Paragraph => "P",
        TagRole::Span => "Span",
        TagRole::Heading1 => "H1",
        TagRole::Heading2 => "H2",
        TagRole::Heading3 => "H3",
        TagRole::Heading4 => "H4",
        TagRole::Heading5 => "H5",
        TagRole::Heading6 => "H6",
        TagRole::List => "L",
        TagRole::ListItem => "LI",
        TagRole::ListBody => "LBody",
        TagRole::Table => "Table",
        TagRole::TableRow => "TR",
        TagRole::TableHeader => "TH",
        TagRole::TableData => "TD",
        TagRole::Figure => "Figure",
        TagRole::Image => "Image",
        TagRole::Form => "Form",
        TagRole::FormField => "FormField",
        TagRole::Link => "Link",
        TagRole::Artifact => "Artifact",
        TagRole::Quote => "Quote",
        TagRole::Note => "Note",
        TagRole::BibEntry => "BibEntry",
        TagRole::Code => "Code",
        TagRole::Caption => "Caption",
    }
}

fn role_from_pdf_name(name: &str) -> Option<TagRole> {
    ALL_ROLES
        .iter()
        .copied()
        .find(|role| role_pdf_name(*role) == name)
}

fn role_from_model_name(name: &str) -> Option<TagRole> {
    ALL_ROLES.iter().copied().find(|role| role.as_str() == name)
}

fn is_standard_role(name: &str) -> bool {
    STANDARD_ROLE_NAMES.contains(&name)
}

/// Returns `true` when `name` is a standard ISO 32000 structure-tag name that
/// needs no `/RoleMap` entry (for example `P`, `H1`, `L`, `Table`, or `Link`).
///
/// Non-standard names such as `Page`, `Image`, `FormField`, `Artifact`, and
/// `Caption` are written through the role map generated by [`role_map`]. The
/// accessibility checker uses this predicate to report unknown structure roles.
pub fn is_standard_pdf_role(name: &str) -> bool {
    is_standard_role(name)
}

fn custom_role_ancestor(name: &str) -> &'static str {
    match name {
        "Page" => "Sect",
        "Image" => "Figure",
        "FormField" => "Form",
        "Artifact" => "Div",
        "Caption" => "P",
        _ => "Div",
    }
}

/// Builds the `/RoleMap` dictionary for the non-standard roles present in `tree`.
pub fn role_map(tree: &A11yTree) -> PdfDict {
    let mut map = PdfDict::new();
    for node in tree.nodes() {
        let name = role_pdf_name(node.role);
        if !is_standard_role(name) {
            map.insert(
                name.to_string(),
                PdfValue::Name(custom_role_ancestor(name).to_string()),
            );
        }
    }
    map
}

fn require_node(tree: &A11yTree, id: NodeId) -> Result<&A11yNode, StructError> {
    tree.node(id)
        .ok_or_else(|| StructError::BadStructElem(format!("node {id} is not in the tree")))
}

fn require_ref(map: &HashMap<NodeId, ObjRef>, id: NodeId) -> Result<ObjRef, StructError> {
    map.get(&id).copied().ok_or_else(|| {
        StructError::BadStructElem(format!("no structure element allocated for node {id}"))
    })
}

/// Returns `true` when `role` is a pure structural container that never
/// carries text and therefore forbids an abbreviation expansion (`/E`).
///
/// Extensions such as `Paragraph`, `Span`, `Code`, `BibEntry`, and table cells
/// legitimately expand abbreviations; container roles do not.
pub fn role_forbids_expansion(role: TagRole) -> bool {
    matches!(
        role,
        TagRole::Document
            | TagRole::Page
            | TagRole::Part
            | TagRole::Sect
            | TagRole::Div
            | TagRole::List
            | TagRole::Table
            | TagRole::TableRow
            | TagRole::Form
    )
}

/// Validates the tagged-PDF-specific semantics of `tree`.
///
/// This is stricter than [`A11yTree::validate`]: in addition to the structural
/// invariants it enforces the writer's rules for `/ActualText`, `/E`, and
/// artifacts. It is called automatically by [`write_structure_tree`] but is
/// public so a host can reject a tree before allocating any object numbers.
pub fn validate_structure_semantics(tree: &A11yTree) -> Result<(), StructError> {
    for node in tree.nodes() {
        if let Some(actual) = &node.actual_text {
            if actual.trim().is_empty() {
                return Err(StructError::EmptyActualText(node.id));
            }
            if !node.children.is_empty() {
                return Err(StructError::ActualTextOnNonLeaf(node.id));
            }
        }
        if node.expansion.is_some() && role_forbids_expansion(node.role) {
            return Err(StructError::ExpansionForbiddenRole(node.id, node.role));
        }
    }

    let index: HashMap<NodeId, usize> = tree
        .nodes()
        .iter()
        .enumerate()
        .map(|(index, node)| (node.id, index))
        .collect();
    for artifact in tree.nodes().iter().filter(|node| node.artifact) {
        let mut seen: HashSet<NodeId> = HashSet::new();
        let mut stack: Vec<NodeId> = artifact.children.clone();
        while let Some(id) = stack.pop() {
            if !seen.insert(id) {
                continue;
            }
            let Some(&position) = index.get(&id) else {
                continue;
            };
            let child = &tree.nodes()[position];
            if !child.artifact {
                return Err(StructError::ArtifactHasStructureKids(artifact.id));
            }
            stack.extend(child.children.iter().copied());
        }
    }
    Ok(())
}

fn bbox_values(box_: BBox) -> PdfValue {
    PdfValue::Array(vec![
        PdfValue::Real(f64::from(box_.x0())),
        PdfValue::Real(f64::from(box_.y0())),
        PdfValue::Real(f64::from(box_.x1())),
        PdfValue::Real(f64::from(box_.y1())),
    ])
}

fn mcr_value(page: ObjRef, mcid: u32) -> PdfValue {
    let mut dict = PdfDict::new();
    dict.insert("Type".into(), PdfValue::Name("MCR".into()));
    dict.insert("Pg".into(), PdfValue::Ref(page));
    dict.insert("MCID".into(), PdfValue::Int(i64::from(mcid)));
    PdfValue::Dict(dict)
}

fn build_overlay(mcids: &[(u32, bool)]) -> Vec<u8> {
    let mut out = String::new();
    for &(mcid, artifact) in mcids {
        let tag = if artifact { "Artifact" } else { "Span" };
        out.push('/');
        out.push_str(tag);
        out.push_str(" << /MCID ");
        out.push_str(&mcid.to_string());
        out.push_str(" >> BDC\nEMC\n");
    }
    out.into_bytes()
}

/// Appends `overlay` to existing page content without modifying any existing byte.
///
/// When `overlay` is empty the original bytes are returned unchanged. Otherwise
/// a single newline separator is inserted only when the existing stream does not
/// already end with one, so the original content is always a byte-for-byte prefix
/// of the result.
pub fn compose_page_content(existing: &[u8], overlay: &[u8]) -> Vec<u8> {
    if overlay.is_empty() {
        return existing.to_vec();
    }
    let mut out = Vec::with_capacity(existing.len() + overlay.len() + 1);
    out.extend_from_slice(existing);
    if !existing.ends_with(b"\n") {
        out.push(b'\n');
    }
    out.extend_from_slice(overlay);
    out
}

/// Writes a validated [`A11yTree`] into a tagged-PDF structure intermediate.
///
/// Object numbers are deterministic: `/StructTreeRoot` is 1, `/ParentTree` is 2,
/// and structure elements are numbered from 3 in depth-first presentation order.
/// Artifact nodes receive no structure element; their marked-content sequences
/// are emitted with the `/Artifact` tag in the page overlay instead.
/// Marked-content identifiers are allocated in the same order for every node
/// carrying non-empty text, including artifacts, which guarantees uniqueness
/// and stable ordering.
///
/// Returns a typed [`StructError`] for an invalid tree (including the semantic
/// rules enforced by [`validate_structure_semantics`]), inconsistent options,
/// or a node whose page falls outside the supplied page range.
pub fn write_structure_tree(
    tree: &A11yTree,
    options: &StructOptions,
) -> Result<StructDocument, StructError> {
    tree.validate().map_err(StructError::InvalidTree)?;
    validate_structure_semantics(tree)?;
    options.validate()?;

    for node in tree.nodes() {
        if node.page >= options.page_count {
            return Err(StructError::PageOutOfRange {
                page: node.page,
                page_count: options.page_count,
            });
        }
    }

    let root_ref = ObjRef::new(1);
    let parent_tree_ref = ObjRef::new(2);
    let traversal = tree.traversal();

    let mut declared_index: HashMap<NodeId, usize> = HashMap::with_capacity(tree.len());
    for (index, node) in tree.nodes().iter().enumerate() {
        declared_index.insert(node.id, index);
    }

    let mut parent_of: HashMap<NodeId, NodeId> = HashMap::with_capacity(tree.len());
    for node in tree.nodes() {
        for &child in &node.children {
            parent_of.insert(child, node.id);
        }
    }

    // Artifacts are marked content, not structure elements, so they get no
    // object number and never appear in /K, /P, or the parent tree.
    let struct_traversal: Vec<NodeId> = traversal
        .iter()
        .copied()
        .filter(|id| tree.node(*id).is_some_and(|node| !node.artifact))
        .collect();

    let mut elem_ref: HashMap<NodeId, ObjRef> = HashMap::with_capacity(struct_traversal.len());
    for (next_object, &id) in (3u32..).zip(struct_traversal.iter()) {
        elem_ref.insert(id, ObjRef::new(next_object));
    }

    let mut mcid_of: HashMap<NodeId, u32> = HashMap::new();
    let mut next_mcid = 0u32;
    for &id in &traversal {
        let node = require_node(tree, id)?;
        if !node.text.is_empty() {
            mcid_of.insert(id, next_mcid);
            next_mcid += 1;
        }
    }
    // MCIDs are allocated densely, so this is an invariant guard rather than a
    // reachable state; it keeps the "no duplicate MCIDs" rule explicit.
    let mut allocated: Vec<u32> = mcid_of.values().copied().collect();
    allocated.sort_unstable();
    if allocated.windows(2).any(|pair| pair[0] == pair[1]) {
        let duplicate = allocated
            .windows(2)
            .find(|pair| pair[0] == pair[1])
            .map(|pair| pair[0])
            .unwrap_or(0);
        return Err(StructError::DuplicateMcid(duplicate));
    }

    let mut page_mcids: Vec<Vec<(u32, bool)>> = vec![Vec::new(); options.page_count];
    for &id in &traversal {
        let node = require_node(tree, id)?;
        if let Some(&mcid) = mcid_of.get(&id) {
            if node.page >= page_mcids.len() {
                return Err(StructError::PageOutOfRange {
                    page: node.page,
                    page_count: options.page_count,
                });
            }
            page_mcids[node.page].push((mcid, node.artifact));
        }
    }

    let mut objects: Vec<(u32, PdfValue)> = Vec::with_capacity(struct_traversal.len() + 2);

    for &id in &struct_traversal {
        let node = require_node(tree, id)?;
        let self_ref = require_ref(&elem_ref, id)?;
        let mut dict = PdfDict::new();
        dict.insert("Type".into(), PdfValue::Name("StructElem".into()));
        dict.insert(
            "S".into(),
            PdfValue::Name(role_pdf_name(node.role).to_string()),
        );

        let parent = match parent_of.get(&id) {
            Some(parent_id) => require_ref(&elem_ref, *parent_id)?,
            None => root_ref,
        };
        dict.insert("P".into(), PdfValue::Ref(parent));

        let mut kids: Vec<PdfValue> = Vec::new();
        if let Some(&mcid) = mcid_of.get(&id) {
            let page = options.page_objects.get(node.page).copied().ok_or(
                StructError::PageOutOfRange {
                    page: node.page,
                    page_count: options.page_count,
                },
            )?;
            kids.push(mcr_value(page, mcid));
        }
        for &child in &node.children {
            // A valid tree cannot be a structure kid of an artifact, but the
            // check keeps this loop total if a caller bypasses the semantic
            // validator and supplies an artifact parent directly.
            if tree
                .node(child)
                .is_some_and(|child_node| child_node.artifact)
            {
                continue;
            }
            kids.push(PdfValue::Ref(require_ref(&elem_ref, child)?));
        }
        dict.insert("K".into(), PdfValue::Array(kids));

        if mcid_of.contains_key(&id) || !node.children.is_empty() {
            let page = options.page_objects.get(node.page).copied().ok_or(
                StructError::PageOutOfRange {
                    page: node.page,
                    page_count: options.page_count,
                },
            )?;
            dict.insert("Pg".into(), PdfValue::Ref(page));
        }

        if let Some(alt) = &node.alt_text {
            dict.insert("Alt".into(), PdfValue::Text(alt.clone()));
        }
        if let Some(actual) = &node.actual_text {
            dict.insert("ActualText".into(), PdfValue::Text(actual.clone()));
        }
        if let Some(expansion) = &node.expansion {
            dict.insert("E".into(), PdfValue::Text(expansion.clone()));
        }

        let order = declared_index.get(&id).copied().ok_or_else(|| {
            StructError::BadStructElem(format!("no declaration index for node {id}"))
        })?;
        dict.insert("XI_NodeId".into(), PdfValue::Int(node.id.get() as i64));
        dict.insert(
            "XI_Role".into(),
            PdfValue::Name(node.role.as_str().to_string()),
        );
        dict.insert("XI_Page".into(), PdfValue::Int(node.page as i64));
        dict.insert("XI_Order".into(), PdfValue::Int(order as i64));
        dict.insert("XI_BBox".into(), bbox_values(node.bbox));
        dict.insert("XI_Text".into(), PdfValue::Text(node.text.clone()));
        if let Some(alt) = &node.alt_text {
            dict.insert("XI_Alt".into(), PdfValue::Text(alt.clone()));
        }
        if let Some(actual) = &node.actual_text {
            dict.insert("XI_ActualText".into(), PdfValue::Text(actual.clone()));
        }
        if let Some(expansion) = &node.expansion {
            dict.insert("XI_Expansion".into(), PdfValue::Text(expansion.clone()));
        }
        if node.artifact {
            dict.insert("XI_Artifact".into(), PdfValue::Bool(true));
        }

        objects.push((self_ref.number, PdfValue::Dict(dict)));
    }

    let mut root = PdfDict::new();
    root.insert("Type".into(), PdfValue::Name("StructTreeRoot".into()));
    let root_kids: Vec<PdfValue> = tree
        .roots()
        .iter()
        .filter(|id| tree.node(**id).is_some_and(|node| !node.artifact))
        .map(|id| require_ref(&elem_ref, *id).map(PdfValue::Ref))
        .collect::<Result<Vec<_>, _>>()?;
    root.insert("K".into(), PdfValue::Array(root_kids));
    root.insert("ParentTree".into(), PdfValue::Ref(parent_tree_ref));
    root.insert(
        "ParentTreeNextKey".into(),
        PdfValue::Int(options.page_count as i64),
    );
    root.insert("RoleMap".into(), PdfValue::Dict(role_map(tree)));
    objects.push((root_ref.number, PdfValue::Dict(root)));

    let mut nums: Vec<PdfValue> = Vec::with_capacity(options.page_count * 2);
    for (page, mcids) in page_mcids.iter().enumerate() {
        let page_ref =
            options
                .page_objects
                .get(page)
                .copied()
                .ok_or(StructError::PageOutOfRange {
                    page,
                    page_count: options.page_count,
                })?;
        let mut value: Vec<PdfValue> = Vec::new();
        for &(mcid, artifact) in mcids {
            if !artifact {
                value.push(mcr_value(page_ref, mcid));
            }
        }
        for &id in &struct_traversal {
            let node = require_node(tree, id)?;
            if node.page == page {
                value.push(PdfValue::Ref(require_ref(&elem_ref, id)?));
            }
        }
        nums.push(PdfValue::Int(page as i64));
        nums.push(PdfValue::Array(value));
    }
    let mut parent_tree = PdfDict::new();
    parent_tree.insert("Nums".into(), PdfValue::Array(nums));
    objects.push((parent_tree_ref.number, PdfValue::Dict(parent_tree)));

    objects.sort_by_key(|(number, _)| *number);
    if objects.windows(2).any(|pair| pair[0].0 == pair[1].0) {
        let duplicate = objects
            .windows(2)
            .find(|pair| pair[0].0 == pair[1].0)
            .map(|pair| pair[0].0)
            .unwrap_or(0);
        return Err(StructError::DuplicateObject(duplicate));
    }

    let mut pages = Vec::with_capacity(options.page_count);
    for (page, mcids) in page_mcids.iter().enumerate() {
        let page_ref =
            options
                .page_objects
                .get(page)
                .copied()
                .ok_or(StructError::PageOutOfRange {
                    page,
                    page_count: options.page_count,
                })?;
        pages.push(PageTagInfo {
            page,
            page_object: page_ref,
            struct_parents: page as u32,
            overlay_content: build_overlay(mcids),
            mcid_ids: mcids.iter().map(|(mcid, _)| *mcid).collect(),
            artifact_mcids: mcids
                .iter()
                .filter(|(_, artifact)| *artifact)
                .map(|(mcid, _)| *mcid)
                .collect(),
        });
    }

    let mut catalog_entries = PdfDict::new();
    catalog_entries.insert("StructTreeRoot".into(), PdfValue::Ref(root_ref));
    catalog_entries.insert("Lang".into(), PdfValue::Text(options.lang.clone()));
    let mut mark_info = PdfDict::new();
    mark_info.insert("Marked".into(), PdfValue::Bool(true));
    catalog_entries.insert("MarkInfo".into(), PdfValue::Dict(mark_info));
    catalog_entries.insert(
        "ViewerPreferences".into(),
        options.viewer_preferences.to_pdf_value(),
    );

    Ok(StructDocument {
        objects,
        struct_tree_root: root_ref,
        parent_tree: parent_tree_ref,
        parent_tree_next_key: options.page_count as u32,
        pages,
        catalog_entries,
    })
}

fn parse_struct_elem(dict: &PdfDict) -> Result<(usize, A11yNode), StructError> {
    let id = dict
        .get("XI_NodeId")
        .and_then(PdfValue::as_int)
        .ok_or_else(|| StructError::BadStructElem("missing /XI_NodeId".to_string()))?;
    if id < 0 {
        return Err(StructError::BadStructElem(
            "negative /XI_NodeId".to_string(),
        ));
    }
    let id = NodeId::new(id as u64);

    let role = dict
        .get("XI_Role")
        .and_then(PdfValue::as_name)
        .and_then(role_from_model_name)
        .or_else(|| {
            dict.get("S")
                .and_then(PdfValue::as_name)
                .and_then(role_from_pdf_name)
        })
        .ok_or_else(|| StructError::UnknownRole("unrecognised structure role".to_string()))?;

    let page = dict
        .get("XI_Page")
        .and_then(PdfValue::as_int)
        .ok_or_else(|| StructError::BadStructElem("missing /XI_Page".to_string()))?;
    if page < 0 {
        return Err(StructError::BadStructElem("negative /XI_Page".to_string()));
    }

    let order = dict
        .get("XI_Order")
        .and_then(PdfValue::as_int)
        .ok_or_else(|| StructError::BadStructElem("missing /XI_Order".to_string()))?;
    if order < 0 {
        return Err(StructError::BadStructElem("negative /XI_Order".to_string()));
    }

    let bounds = dict
        .get("XI_BBox")
        .and_then(PdfValue::as_array)
        .ok_or_else(|| StructError::BadStructElem("missing /XI_BBox".to_string()))?;
    if bounds.len() != 4 {
        return Err(StructError::BadStructElem(format!(
            "/XI_BBox has {} values, expected 4",
            bounds.len()
        )));
    }
    let mut coords = [0.0f64; 4];
    for (slot, value) in coords.iter_mut().zip(bounds) {
        let number = value
            .as_number()
            .ok_or_else(|| StructError::BadStructElem("non-numeric /XI_BBox".to_string()))?;
        *slot = number;
    }
    let bbox = BBox::new(
        coords[0] as f32,
        coords[1] as f32,
        coords[2] as f32,
        coords[3] as f32,
    )
    .map_err(StructError::InvalidTree)?;

    let text = dict
        .get("XI_Text")
        .and_then(PdfValue::as_text)
        .unwrap_or("")
        .to_string();
    let alt_text = dict
        .get("XI_Alt")
        .and_then(PdfValue::as_text)
        .or_else(|| dict.get("Alt").and_then(PdfValue::as_text))
        .map(str::to_string);
    let actual_text = dict
        .get("XI_ActualText")
        .and_then(PdfValue::as_text)
        .or_else(|| dict.get("ActualText").and_then(PdfValue::as_text))
        .map(str::to_string);
    let expansion = dict
        .get("XI_Expansion")
        .and_then(PdfValue::as_text)
        .or_else(|| dict.get("E").and_then(PdfValue::as_text))
        .map(str::to_string);
    let artifact = dict
        .get("XI_Artifact")
        .and_then(PdfValue::as_bool)
        .unwrap_or(false);

    let mut node = A11yNode::new(id, role, page as usize, bbox, text, Vec::new());
    node.alt_text = alt_text;
    node.actual_text = actual_text;
    node.expansion = expansion;
    node.artifact = artifact;
    Ok((order as usize, node))
}

/// Reconstructs the [`A11yTree`] encoded in a [`StructDocument`].
///
/// This is the inverse of [`write_structure_tree`]: node ids, roles, pages,
/// boxes, text, alternative text, `/ActualText`, `/E` expansions, child order,
/// and root order all survive the round-trip because the writer records them in
/// documented `XI_` extension entries. Artifact nodes are emitted as `/Artifact`
/// marked content rather than structure elements, so they are intentionally
/// absent from the reconstructed tree. Returns a typed [`StructError`] for
/// malformed generated objects.
pub fn read_structure_tree(doc: &StructDocument) -> Result<A11yTree, StructError> {
    let root_dict = doc
        .object(doc.struct_tree_root)
        .and_then(PdfValue::as_dict)
        .ok_or(StructError::MissingObject(doc.struct_tree_root.number))?;

    let mut elements: Vec<(usize, u32, A11yNode, &PdfDict)> = Vec::new();
    for (number, value) in &doc.objects {
        let Some(dict) = value.as_dict() else {
            continue;
        };
        if dict.get("Type").and_then(PdfValue::as_name) != Some("StructElem") {
            continue;
        }
        let (order, node) = parse_struct_elem(dict)?;
        elements.push((order, *number, node, dict));
    }
    elements.sort_by_key(|(order, _, _, _)| *order);

    let mut id_of_object: HashMap<u32, NodeId> = HashMap::with_capacity(elements.len());
    for (_, number, node, _) in &elements {
        id_of_object.insert(*number, node.id);
    }

    let mut roots: Vec<NodeId> = Vec::new();
    let root_kids = root_dict
        .get("K")
        .and_then(PdfValue::as_array)
        .ok_or_else(|| StructError::BadStructElem("StructTreeRoot has no /K".to_string()))?;
    for item in root_kids {
        let reference = item.as_ref().ok_or_else(|| {
            StructError::BadStructElem("non-reference in StructTreeRoot /K".to_string())
        })?;
        let id = id_of_object
            .get(&reference.number)
            .copied()
            .ok_or(StructError::MissingObject(reference.number))?;
        roots.push(id);
    }

    let mut nodes: Vec<A11yNode> = Vec::with_capacity(elements.len());
    for (_, _, node, dict) in elements {
        let mut node = node;
        let kids = dict
            .get("K")
            .and_then(PdfValue::as_array)
            .ok_or_else(|| StructError::BadStructElem("StructElem has no /K".to_string()))?;
        for item in kids {
            if let Some(reference) = item.as_ref() {
                let child = id_of_object
                    .get(&reference.number)
                    .copied()
                    .ok_or(StructError::MissingObject(reference.number))?;
                node.children.push(child);
            }
        }
        nodes.push(node);
    }

    let tree = A11yTree::new(nodes, roots).map_err(StructError::InvalidTree)?;
    Ok(tree)
}

/// Kind of a projected structure dictionary entry.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum StructEntryType {
    /// The catalog `/StructTreeRoot` dictionary.
    StructTreeRoot,
    /// The `/ParentTree` number tree dictionary.
    ParentTree,
    /// A `/StructElem` dictionary for one non-artifact node.
    StructElem,
    /// Any other generated dictionary.
    Other,
}

impl StructEntryType {
    /// Stable token used by [`StructureProjection::to_canonical_bytes`].
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StructTreeRoot => "StructTreeRoot",
            Self::ParentTree => "ParentTree",
            Self::StructElem => "StructElem",
            Self::Other => "Other",
        }
    }
}

/// A child reference inside a projected structure dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum StructKid {
    /// An indirect reference to another structure element.
    Ref(ObjRef),
    /// A marked-content reference to a page and `/MCID`.
    MarkedContent {
        /// Page object carrying the marked-content sequence.
        page: ObjRef,
        /// Marked-content identifier.
        mcid: u32,
        /// Whether the sequence carries the `/Artifact` tag.
        artifact: bool,
    },
}

/// One structure dictionary entry in a [`StructureProjection`].
///
/// The fields mirror what a PDF object writer needs: object number, type,
/// role, alternative/replacement/expansion text, ordered kids, and parent
/// reference.
#[derive(Clone, Debug, PartialEq)]
pub struct StructureEntry {
    /// Indirect reference of this dictionary.
    pub object: ObjRef,
    /// Kind of dictionary.
    pub entry_type: StructEntryType,
    /// Structure role, present for [`StructEntryType::StructElem`] entries.
    pub role: Option<TagRole>,
    /// Alternative text (`/Alt`).
    pub alt: Option<String>,
    /// Replacement text (`/ActualText`).
    pub actual_text: Option<String>,
    /// Abbreviation expansion (`/E`).
    pub expansion: Option<String>,
    /// Ordered children, as element references or marked-content references.
    pub kids: Vec<StructKid>,
    /// Parent structure element, or the root for top-level elements.
    pub parent: Option<ObjRef>,
}

/// A deterministic, object-level projection of a tagged structure tree.
///
/// Produced by [`project_structure`] from the underlying [`StructDocument`].
/// Entries are ordered by object number, so repeated projections of the same
/// model are byte-identical. [`Self::validate_consistency`] checks that the
/// `/StructTreeRoot` and `/ParentTree` references agree and that no entry has a
/// dangling parent, a parent cycle, or a duplicate `/MCID`.
#[derive(Clone, Debug, PartialEq)]
pub struct StructureProjection {
    /// The generated object document this projection was derived from.
    pub document: StructDocument,
    /// Structure dictionary entries, ordered by object number.
    pub entries: Vec<StructureEntry>,
    /// Reference the root records as its `/ParentTree`.
    pub root_parent_tree: Option<ObjRef>,
    /// Reference to the generated `/StructTreeRoot`.
    pub struct_tree_root: ObjRef,
    /// Reference to the generated `/ParentTree`.
    pub parent_tree: ObjRef,
    /// Value of `/ParentTreeNextKey`.
    pub parent_tree_next_key: u32,
}

impl StructureProjection {
    /// Serializes the projection into a byte-stable, line-oriented form.
    ///
    /// The output depends only on the ordered fields of [`Self::entries`] and
    /// the root/parent-tree references. Text fields escape backslashes, tabs,
    /// and newlines so distinct models never collide.
    pub fn to_canonical_bytes(&self) -> Vec<u8> {
        let mut out = String::from("PDFXI-STRUCT-PROJECTION\t1\n");
        out.push_str(&format!(
            "ROOT\t{}\t{}\t{}\n",
            self.struct_tree_root.number, self.parent_tree.number, self.parent_tree_next_key
        ));
        for entry in &self.entries {
            out.push_str(&format!(
                "E\t{}\t{}\t{}\t{}\t{}\t{}\t{}\t",
                entry.object.number,
                entry.entry_type.as_str(),
                entry
                    .role
                    .map_or_else(|| "-".to_string(), |role| role.as_str().to_string()),
                escape_field(entry.alt.as_deref()),
                escape_field(entry.actual_text.as_deref()),
                escape_field(entry.expansion.as_deref()),
                entry
                    .parent
                    .map_or_else(|| "-".to_string(), |parent| parent.number.to_string()),
            ));
            for kid in &entry.kids {
                match kid {
                    StructKid::Ref(reference) => {
                        out.push_str(&format!("R{}:", reference.number));
                    }
                    StructKid::MarkedContent {
                        page,
                        mcid,
                        artifact,
                    } => {
                        out.push_str(&format!(
                            "M{}:{}:{}:",
                            page.number,
                            mcid,
                            if *artifact { "A" } else { "C" }
                        ));
                    }
                }
            }
            out.push('\n');
        }
        out.into_bytes()
    }

    /// Checks the projection for dangling parents, cycles, duplicate object
    /// numbers, duplicate `/MCID` values, and inconsistent root references.
    pub fn validate_consistency(&self) -> Result<(), StructError> {
        let by_number: HashMap<u32, &StructureEntry> = self
            .entries
            .iter()
            .map(|entry| (entry.object.number, entry))
            .collect();
        let mut numbers: HashSet<u32> = HashSet::with_capacity(self.entries.len());
        for entry in &self.entries {
            if !numbers.insert(entry.object.number) {
                return Err(StructError::DuplicateObject(entry.object.number));
            }
        }
        if !by_number.contains_key(&self.struct_tree_root.number) {
            return Err(StructError::MissingObject(self.struct_tree_root.number));
        }
        if !by_number.contains_key(&self.parent_tree.number) {
            return Err(StructError::MissingObject(self.parent_tree.number));
        }
        if self.root_parent_tree != Some(self.parent_tree) {
            return Err(StructError::BadStructElem(
                "StructTreeRoot /ParentTree is inconsistent".to_string(),
            ));
        }

        // Every parent must resolve, and following parent links must terminate.
        for entry in &self.entries {
            let mut seen: HashSet<u32> = HashSet::new();
            let mut current = Some(entry.object);
            while let Some(reference) = current {
                if !seen.insert(reference.number) {
                    return Err(StructError::BadStructElem(
                        "structure parent cycle".to_string(),
                    ));
                }
                let Some(parent_entry) = by_number.get(&reference.number) else {
                    return Err(StructError::MissingObject(reference.number));
                };
                current = parent_entry.parent;
            }
        }

        let mut mcids: Vec<u32> = Vec::new();
        for entry in &self.entries {
            for kid in &entry.kids {
                if let StructKid::MarkedContent { mcid, .. } = kid {
                    mcids.push(*mcid);
                }
            }
        }
        mcids.sort_unstable();
        if mcids.windows(2).any(|pair| pair[0] == pair[1]) {
            let duplicate = mcids
                .windows(2)
                .find(|pair| pair[0] == pair[1])
                .map(|pair| pair[0])
                .unwrap_or(0);
            return Err(StructError::DuplicateMcid(duplicate));
        }
        Ok(())
    }
}

fn escape_field(value: Option<&str>) -> String {
    let Some(value) = value else {
        return "-".to_string();
    };
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '\\' => out.push_str("\\\\"),
            '\t' => out.push_str("\\t"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            _ => out.push(ch),
        }
    }
    out
}

fn project_kid(value: &PdfValue) -> StructKid {
    if let Some(reference) = value.as_ref() {
        return StructKid::Ref(reference);
    }
    if let Some(dict) = value.as_dict() {
        let mcid = dict
            .get("MCID")
            .and_then(PdfValue::as_int)
            .unwrap_or(0)
            .max(0) as u32;
        let page = dict
            .get("Pg")
            .and_then(PdfValue::as_ref)
            .unwrap_or_else(|| ObjRef::new(0));
        let artifact = dict
            .get("Artifact")
            .and_then(PdfValue::as_bool)
            .unwrap_or(false);
        return StructKid::MarkedContent {
            page,
            mcid,
            artifact,
        };
    }
    StructKid::MarkedContent {
        page: ObjRef::new(0),
        mcid: 0,
        artifact: false,
    }
}

/// Projects a validated [`A11yTree`] into an ordered set of structure
/// dictionary entries that a caller can serialize.
///
/// The projection is derived from [`write_structure_tree`], so it shares the
/// writer's single source of truth for object numbering and field values.
pub fn project_structure(
    tree: &A11yTree,
    options: &StructOptions,
) -> Result<StructureProjection, StructError> {
    let document = write_structure_tree(tree, options)?;
    let mut entries: Vec<StructureEntry> = Vec::with_capacity(document.objects.len());
    for (_, value) in &document.objects {
        let Some(dict) = value.as_dict() else {
            continue;
        };
        let type_name = dict.get("Type").and_then(PdfValue::as_name);
        let entry_type = match type_name {
            Some("StructTreeRoot") => StructEntryType::StructTreeRoot,
            Some("StructElem") => StructEntryType::StructElem,
            _ if dict.contains_key("Nums") => StructEntryType::ParentTree,
            _ => StructEntryType::Other,
        };
        let role = dict
            .get("XI_Role")
            .and_then(PdfValue::as_name)
            .and_then(role_from_model_name)
            .or_else(|| {
                dict.get("S")
                    .and_then(PdfValue::as_name)
                    .and_then(role_from_pdf_name)
            });
        let kids = dict
            .get("K")
            .and_then(PdfValue::as_array)
            .map(|items| items.iter().map(project_kid).collect())
            .unwrap_or_default();
        let parent = dict.get("P").and_then(PdfValue::as_ref).or(
            if entry_type == StructEntryType::StructElem {
                Some(document.struct_tree_root)
            } else {
                None
            },
        );
        entries.push(StructureEntry {
            object: ObjRef::new(0),
            entry_type,
            role,
            alt: dict
                .get("XI_Alt")
                .and_then(PdfValue::as_text)
                .or_else(|| dict.get("Alt").and_then(PdfValue::as_text))
                .map(str::to_string),
            actual_text: dict
                .get("XI_ActualText")
                .and_then(PdfValue::as_text)
                .or_else(|| dict.get("ActualText").and_then(PdfValue::as_text))
                .map(str::to_string),
            expansion: dict
                .get("XI_Expansion")
                .and_then(PdfValue::as_text)
                .or_else(|| dict.get("E").and_then(PdfValue::as_text))
                .map(str::to_string),
            kids,
            parent,
        });
    }
    // Object numbers are read from the sorted document so `entries` is ordered
    // by construction; assigning them here keeps the projection deterministic.
    for ((number, _), entry) in document.objects.iter().zip(entries.iter_mut()) {
        entry.object = ObjRef::new(*number);
    }
    let struct_tree_root = document.struct_tree_root;
    let parent_tree = document.parent_tree;
    let parent_tree_next_key = document.parent_tree_next_key;
    let root_parent_tree = document
        .object(struct_tree_root)
        .and_then(PdfValue::as_dict)
        .and_then(|dict| dict.get("ParentTree"))
        .and_then(PdfValue::as_ref);
    let projection = StructureProjection {
        document,
        entries,
        root_parent_tree,
        struct_tree_root,
        parent_tree,
        parent_tree_next_key,
    };
    projection.validate_consistency()?;
    Ok(projection)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn box_at() -> BBox {
        match BBox::new(4.0, 8.0, 60.0, 24.0) {
            Ok(value) => value,
            Err(_) => panic!("test box must be valid"),
        }
    }

    fn node(id: u64, role: TagRole, page: usize, text: &str, children: Vec<NodeId>) -> A11yNode {
        A11yNode::new(NodeId::new(id), role, page, box_at(), text, children)
    }

    fn sample_tree() -> A11yTree {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![
                        NodeId::new(3),
                        NodeId::new(4),
                        NodeId::new(5),
                        NodeId::new(6),
                    ],
                ),
                node(3, TagRole::Heading1, 0, "Introduction", vec![]),
                node(4, TagRole::Paragraph, 0, "Hello tagged world", vec![]),
                node(5, TagRole::Figure, 0, "", vec![])
                    .with_alt_text("A bar chart of quarterly sales"),
                node(
                    6,
                    TagRole::List,
                    0,
                    "",
                    vec![NodeId::new(7), NodeId::new(8)],
                ),
                node(7, TagRole::ListItem, 0, "First item", vec![]),
                node(8, TagRole::ListItem, 0, "Second item", vec![]),
            ],
            vec![NodeId::new(1)],
        );
        match tree {
            Ok(tree) => tree,
            Err(_) => panic!("sample tree must be valid"),
        }
    }

    fn options(page_count: usize) -> StructOptions {
        StructOptions {
            lang: "en-US".to_string(),
            page_count,
            page_objects: (0..page_count)
                .map(|p| ObjRef::new(100 + p as u32))
                .collect(),
            viewer_preferences: ViewerPreferences {
                display_doc_title: true,
                hide_toolbar: true,
                hide_menubar: false,
                fit_window: true,
            },
        }
    }

    #[test]
    fn role_pdf_names_are_distinct_and_reversible() {
        let mut names = std::collections::BTreeSet::new();
        for &role in ALL_ROLES {
            let name = role_pdf_name(role);
            assert!(names.insert(name), "duplicate PDF role name {name}");
            assert_eq!(role_from_pdf_name(name), Some(role));
            assert_eq!(role_from_model_name(role.as_str()), Some(role));
        }
        assert_eq!(names.len(), ALL_ROLES.len());
    }

    #[test]
    fn role_mapping_uses_standard_pdf_names() {
        assert_eq!(role_pdf_name(TagRole::Document), "Document");
        assert_eq!(role_pdf_name(TagRole::Paragraph), "P");
        assert_eq!(role_pdf_name(TagRole::Heading1), "H1");
        assert_eq!(role_pdf_name(TagRole::Heading6), "H6");
        assert_eq!(role_pdf_name(TagRole::List), "L");
        assert_eq!(role_pdf_name(TagRole::ListItem), "LI");
        assert_eq!(role_pdf_name(TagRole::ListBody), "LBody");
        assert_eq!(role_pdf_name(TagRole::TableRow), "TR");
        assert_eq!(role_pdf_name(TagRole::TableHeader), "TH");
        assert_eq!(role_pdf_name(TagRole::TableData), "TD");
        assert_eq!(role_pdf_name(TagRole::Figure), "Figure");
    }

    #[test]
    fn struct_tree_root_has_kids_parent_tree_and_role_map() {
        let doc = write_structure_tree(&sample_tree(), &options(1)).expect("write succeeds");
        let root = doc.object(doc.struct_tree_root).expect("root object");
        let dict = root.as_dict().expect("root dictionary");
        assert_eq!(
            dict.get("Type").and_then(PdfValue::as_name),
            Some("StructTreeRoot")
        );
        assert_eq!(
            dict.get("ParentTree").and_then(PdfValue::as_ref),
            Some(doc.parent_tree)
        );
        assert_eq!(
            dict.get("ParentTreeNextKey").and_then(PdfValue::as_int),
            Some(1)
        );
        assert_eq!(
            dict.get("K")
                .and_then(PdfValue::as_array)
                .map(|items| items.len()),
            Some(1)
        );
        assert!(dict.get("RoleMap").and_then(PdfValue::as_dict).is_some());
        assert_eq!(doc.struct_tree_root, ObjRef::new(1));
        assert_eq!(doc.parent_tree, ObjRef::new(2));
    }

    #[test]
    fn parent_tree_matches_struct_parents_keys() {
        let doc = write_structure_tree(&sample_tree(), &options(3)).expect("write succeeds");
        let tree = doc.object(doc.parent_tree).expect("parent tree object");
        let nums = tree
            .as_dict()
            .and_then(|dict| dict.get("Nums"))
            .and_then(PdfValue::as_array)
            .expect("Nums array");
        assert_eq!(nums.len(), 6);
        for page in 0..3 {
            assert_eq!(nums[page * 2].as_int(), Some(page as i64));
            assert!(nums[page * 2 + 1].as_array().is_some());
            assert_eq!(doc.pages[page].struct_parents, page as u32);
            assert_eq!(doc.pages[page].page_object.number, 100 + page as u32);
        }
        assert_eq!(doc.parent_tree_next_key, 3);
    }

    #[test]
    fn mcids_are_unique_and_in_traversal_order() {
        let tree = sample_tree();
        let doc = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        let text_node_count = tree
            .traversal()
            .into_iter()
            .filter_map(|id| tree.node(id))
            .filter(|node| !node.text.is_empty())
            .count();
        let reported: Vec<u32> = doc.pages[0].mcid_ids.clone();
        assert_eq!(reported.len(), text_node_count);
        for (index, value) in reported.iter().enumerate() {
            assert_eq!(*value, index as u32, "MCIDs must be dense and ordered");
        }
        let mut all: Vec<u32> = doc
            .pages
            .iter()
            .flat_map(|p| p.mcid_ids.iter().copied())
            .collect();
        let total = all.len();
        all.sort_unstable();
        all.dedup();
        assert_eq!(all.len(), total, "MCIDs must be unique");
    }

    #[test]
    fn alt_text_is_written_for_figures() {
        let doc = write_structure_tree(&sample_tree(), &options(1)).expect("write succeeds");
        let figure = doc
            .objects
            .iter()
            .find_map(|(_, value)| {
                let dict = value.as_dict()?;
                if dict.get("XI_NodeId").and_then(PdfValue::as_int) == Some(5) {
                    Some(dict)
                } else {
                    None
                }
            })
            .expect("figure element");
        assert_eq!(
            figure.get("Alt").and_then(PdfValue::as_text),
            Some("A bar chart of quarterly sales")
        );
        assert_eq!(figure.get("S").and_then(PdfValue::as_name), Some("Figure"));
        let paragraph = doc
            .objects
            .iter()
            .find_map(|(_, value)| {
                let dict = value.as_dict()?;
                if dict.get("XI_NodeId").and_then(PdfValue::as_int) == Some(4) {
                    Some(dict)
                } else {
                    None
                }
            })
            .expect("paragraph element");
        assert!(!paragraph.contains_key("Alt"));
    }

    #[test]
    fn pages_carry_struct_parents_and_mcid_overlay() {
        let doc = write_structure_tree(&sample_tree(), &options(2)).expect("write succeeds");
        assert_eq!(doc.pages.len(), 2);
        assert_eq!(doc.pages[0].struct_parents, 0);
        assert_eq!(doc.pages[1].struct_parents, 1);
        let overlay = doc.overlay_for_page(0).expect("page 0 overlay");
        let text = core::str::from_utf8(overlay).expect("overlay is UTF-8");
        assert_eq!(text.matches("BDC").count(), 4);
        assert_eq!(text.matches("EMC").count(), 4);
        assert!(text.contains("/MCID 0 >> BDC"));
        assert!(doc.overlay_for_page(1).expect("page 1 overlay").is_empty());
    }

    #[test]
    fn round_trip_reconstructs_original_tree() {
        let tree = sample_tree();
        let doc = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        let restored = read_structure_tree(&doc).expect("read-back succeeds");
        assert_eq!(restored, tree);
        assert_eq!(restored.validate(), Ok(()));
    }

    #[test]
    fn empty_tree_produces_minimal_structure_document() {
        let tree = A11yTree::empty();
        let doc = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        assert_eq!(doc.struct_elem_count(), 0);
        assert_eq!(doc.objects.len(), 2);
        let root = doc.object(doc.struct_tree_root).expect("root");
        let kids = root
            .as_dict()
            .and_then(|dict| dict.get("K"))
            .and_then(PdfValue::as_array)
            .expect("empty kids");
        assert!(kids.is_empty());
        assert!(doc.pages[0].overlay_content.is_empty());
        let restored = read_structure_tree(&doc).expect("read-back succeeds");
        assert!(restored.is_empty());
    }

    #[test]
    fn nested_lists_and_tables_serialize_with_correct_hierarchy() {
        let tree = A11yTree::new(
            vec![
                node(
                    1,
                    TagRole::Document,
                    0,
                    "",
                    vec![NodeId::new(2), NodeId::new(6)],
                ),
                node(2, TagRole::List, 0, "", vec![NodeId::new(3)]),
                node(3, TagRole::ListItem, 0, "outer", vec![NodeId::new(4)]),
                node(4, TagRole::List, 0, "", vec![NodeId::new(5)]),
                node(5, TagRole::ListItem, 0, "inner", vec![]),
                node(
                    6,
                    TagRole::Table,
                    0,
                    "",
                    vec![NodeId::new(7), NodeId::new(9)],
                ),
                node(7, TagRole::TableRow, 0, "", vec![NodeId::new(8)]),
                node(8, TagRole::TableHeader, 0, "Name", vec![]),
                node(9, TagRole::TableRow, 0, "", vec![NodeId::new(10)]),
                node(10, TagRole::TableData, 0, "Ada", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid nested tree");
        let doc = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        let names: Vec<&str> = doc
            .objects
            .iter()
            .filter_map(|(_, value)| {
                value
                    .as_dict()
                    .and_then(|dict| dict.get("S"))
                    .and_then(PdfValue::as_name)
            })
            .collect();
        assert!(names.contains(&"L"));
        assert!(names.contains(&"LI"));
        assert!(names.contains(&"Table"));
        assert!(names.contains(&"TR"));
        assert!(names.contains(&"TH"));
        assert!(names.contains(&"TD"));
        assert_eq!(read_structure_tree(&doc).expect("read-back"), tree);
    }

    #[test]
    fn unknown_roles_are_declared_in_role_map() {
        let tree = A11yTree::new(
            vec![
                node(
                    1,
                    TagRole::Document,
                    0,
                    "",
                    vec![NodeId::new(2), NodeId::new(3), NodeId::new(4)],
                ),
                node(2, TagRole::Image, 0, "", vec![]).with_alt_text("picture"),
                node(3, TagRole::Caption, 0, "A caption", vec![]),
                node(4, TagRole::FormField, 0, "", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .expect("valid tree");
        let doc = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        let root = doc.object(doc.struct_tree_root).expect("root");
        let map = root
            .as_dict()
            .and_then(|dict| dict.get("RoleMap"))
            .and_then(PdfValue::as_dict)
            .expect("role map");
        assert_eq!(map.get("Image").and_then(PdfValue::as_name), Some("Figure"));
        assert_eq!(map.get("Caption").and_then(PdfValue::as_name), Some("P"));
        assert_eq!(
            map.get("FormField").and_then(PdfValue::as_name),
            Some("Form")
        );
        assert!(!map.contains_key("Document"));
    }

    #[test]
    fn existing_content_bytes_are_preserved_by_overlay_composition() {
        let existing = b"q 1 0 0 1 0 0 cm\nBT /F1 12 Tf (Original) Tj ET\nQ";
        let overlay = b"/Span << /MCID 0 >> BDC\nEMC\n";
        let composed = compose_page_content(existing, overlay);
        assert!(
            composed.starts_with(existing),
            "original bytes must be an untouched prefix"
        );
        assert!(composed.ends_with(overlay));
        assert_eq!(&composed[existing.len() + 1..], overlay.as_slice());
        let unchanged = compose_page_content(existing, b"");
        assert_eq!(unchanged, existing.to_vec());
    }

    #[test]
    fn lang_and_viewer_preferences_are_written_to_catalog() {
        let doc = write_structure_tree(&sample_tree(), &options(1)).expect("write succeeds");
        assert_eq!(
            doc.catalog_entries.get("Lang").and_then(PdfValue::as_text),
            Some("en-US")
        );
        assert_eq!(
            doc.catalog_entries
                .get("MarkInfo")
                .and_then(PdfValue::as_dict)
                .and_then(|dict| dict.get("Marked"))
                .and_then(PdfValue::as_bool),
            Some(true)
        );
        let vp = doc
            .catalog_entries
            .get("ViewerPreferences")
            .and_then(PdfValue::as_dict)
            .expect("viewer preferences");
        assert_eq!(
            vp.get("DisplayDocTitle").and_then(PdfValue::as_bool),
            Some(true)
        );
        assert_eq!(
            vp.get("HideToolbar").and_then(PdfValue::as_bool),
            Some(true)
        );
        assert_eq!(vp.get("FitWindow").and_then(PdfValue::as_bool), Some(true));
        assert!(!vp.contains_key("HideMenubar"));
    }

    #[test]
    fn invalid_options_and_out_of_range_pages_are_typed_errors() {
        let mut bad = options(2);
        bad.page_objects.pop();
        assert!(matches!(
            bad.validate(),
            Err(StructError::InvalidOptions(_))
        ));
        bad.lang = "  ".to_string();
        assert!(matches!(
            bad.validate(),
            Err(StructError::InvalidOptions(_))
        ));

        let result = write_structure_tree(&sample_tree(), &options(0));
        assert!(matches!(
            result,
            Err(StructError::PageOutOfRange {
                page: 0,
                page_count: 0
            })
        ));
    }

    #[test]
    fn serialization_is_deterministic_and_names_are_escaped() {
        let tree = sample_tree();
        let a = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        let b = write_structure_tree(&tree, &options(1)).expect("write succeeds");
        assert_eq!(a.to_pdf_bytes(), b.to_pdf_bytes());
        assert!(a.to_pdf_bytes().starts_with(b"%PDFXI tagged structure\n"));

        let mut name = String::new();
        write_name("A/B", &mut name);
        assert_eq!(name, "/A#2FB");
        let mut literal = String::new();
        write_literal("a(b)\\c", &mut literal);
        assert_eq!(literal, "(a\\(b\\)\\\\c)");
    }

    fn elem_for(doc: &StructDocument, id: u64) -> PdfDict {
        doc.objects
            .iter()
            .find_map(|(_, value)| {
                let dict = value.as_dict()?;
                if dict.get("XI_NodeId").and_then(PdfValue::as_int) == Some(id as i64) {
                    Some(dict.clone())
                } else {
                    None
                }
            })
            .unwrap_or_else(|| panic!("no structure element for node {id}"))
    }

    fn tree_with_annotations() -> A11yTree {
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![
                        NodeId::new(3),
                        NodeId::new(4),
                        NodeId::new(5),
                        NodeId::new(6),
                    ],
                ),
                node(3, TagRole::Heading1, 0, "Intro", vec![]),
                node(4, TagRole::Paragraph, 0, "fb", vec![])
                    .with_actual_text("feedback")
                    .with_expansion("feedback"),
                node(5, TagRole::Figure, 0, "", vec![]).with_alt_text("chart"),
                node(6, TagRole::Artifact, 0, "decorative", vec![]).with_artifact(true),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("enriched tree must be valid: {error}"))
    }

    fn tree_with_artifact_subtree() -> A11yTree {
        A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(
                    2,
                    TagRole::Page,
                    0,
                    "",
                    vec![NodeId::new(3), NodeId::new(4)],
                ),
                node(3, TagRole::Paragraph, 0, "real", vec![]),
                node(4, TagRole::Artifact, 0, "", vec![NodeId::new(5)]).with_artifact(true),
                node(5, TagRole::Artifact, 0, "noise", vec![]).with_artifact(true),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("artifact-subtree tree must be valid: {error}"))
    }

    #[test]
    fn actual_text_and_expansion_are_serialized_and_restored() {
        let doc = write_structure_tree(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("write: {error}"));
        let paragraph = elem_for(&doc, 4);
        assert_eq!(
            paragraph.get("ActualText").and_then(PdfValue::as_text),
            Some("feedback")
        );
        assert_eq!(
            paragraph.get("E").and_then(PdfValue::as_text),
            Some("feedback")
        );
        assert_eq!(
            paragraph.get("XI_ActualText").and_then(PdfValue::as_text),
            Some("feedback")
        );
        assert_eq!(
            paragraph.get("XI_Expansion").and_then(PdfValue::as_text),
            Some("feedback")
        );
        let restored = read_structure_tree(&doc).unwrap_or_else(|error| panic!("read: {error}"));
        let node = restored
            .node(NodeId::new(4))
            .unwrap_or_else(|| panic!("paragraph must survive"));
        assert_eq!(node.actual_text.as_deref(), Some("feedback"));
        assert_eq!(node.expansion.as_deref(), Some("feedback"));
    }

    #[test]
    fn artifact_is_marked_content_not_a_structure_element() {
        let doc = write_structure_tree(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("write: {error}"));
        assert_eq!(
            doc.objects
                .iter()
                .filter(|(_, value)| {
                    value
                        .as_dict()
                        .and_then(|dict| dict.get("XI_NodeId"))
                        .and_then(PdfValue::as_int)
                        == Some(6)
                })
                .count(),
            0,
            "artifact node must not be a StructElem"
        );
        let page = elem_for(&doc, 2);
        let kids = page
            .get("K")
            .and_then(PdfValue::as_array)
            .unwrap_or_else(|| panic!("page kids"));
        assert_eq!(kids.len(), 3, "artifact child must be pruned from /K");
        let overlay = core::str::from_utf8(
            doc.overlay_for_page(0)
                .unwrap_or_else(|| panic!("page 0 overlay")),
        )
        .unwrap_or_else(|error| panic!("overlay is UTF-8: {error}"));
        assert!(overlay.contains("/Artifact << /MCID 2 >> BDC"));
        assert!(overlay.contains("/Span << /MCID 0 >> BDC"));
    }

    #[test]
    fn artifact_subtree_is_excluded_from_reading_order_and_structure() {
        let tree = tree_with_artifact_subtree();
        let order =
            crate::export_reading_order(&tree).unwrap_or_else(|error| panic!("order: {error}"));
        assert_eq!(order.traversal(), vec![1, 2, 3]);
        assert_eq!(order.len(), 3);
        let doc = write_structure_tree(&tree, &options(1))
            .unwrap_or_else(|error| panic!("write: {error}"));
        for id in [4, 5] {
            assert!(
                doc.objects.iter().all(|(_, value)| value
                    .as_dict()
                    .and_then(|dict| dict.get("XI_NodeId"))
                    .and_then(PdfValue::as_int)
                    != Some(id)),
                "artifact {id} must not be a StructElem"
            );
        }
        let overlay = core::str::from_utf8(
            doc.overlay_for_page(0)
                .unwrap_or_else(|| panic!("page 0 overlay")),
        )
        .unwrap_or_else(|error| panic!("overlay is UTF-8: {error}"));
        assert_eq!(overlay.matches("/Artifact << /MCID").count(), 1);
        assert_eq!(doc.pages[0].artifact_mcids, vec![1]);
    }

    #[test]
    fn projection_is_byte_stable_and_ordered() {
        let tree = tree_with_annotations();
        let left = project_structure(&tree, &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        let right = project_structure(&tree, &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        assert_eq!(left.entries, right.entries);
        assert_eq!(left.to_canonical_bytes(), right.to_canonical_bytes());
        assert!(
            left.entries
                .windows(2)
                .all(|pair| pair[0].object.number < pair[1].object.number)
        );
        let text = String::from_utf8(left.to_canonical_bytes())
            .unwrap_or_else(|error| panic!("canonical bytes are UTF-8: {error}"));
        assert!(text.starts_with("PDFXI-STRUCT-PROJECTION\t1\n"));
        assert!(text.contains("feedback"));
        assert!(left.entries.iter().any(|entry| {
            entry.entry_type == StructEntryType::StructElem
                && entry.actual_text.as_deref() == Some("feedback")
                && entry.expansion.as_deref() == Some("feedback")
        }));
    }

    #[test]
    fn projection_consistency_accepts_a_valid_tree() {
        let projection = project_structure(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        assert_eq!(projection.validate_consistency(), Ok(()));
        assert_eq!(projection.entries.len(), 7);
        assert_eq!(projection.root_parent_tree, Some(projection.parent_tree));
        assert_eq!(projection.parent_tree_next_key, 1);
    }

    #[test]
    fn projection_rejects_a_dangling_parent() {
        let mut projection = project_structure(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        let entry = projection
            .entries
            .iter_mut()
            .find(|entry| entry.entry_type == StructEntryType::StructElem)
            .unwrap_or_else(|| panic!("struct elem"));
        entry.parent = Some(ObjRef::new(999));
        assert_eq!(
            projection.validate_consistency(),
            Err(StructError::MissingObject(999))
        );
    }

    #[test]
    fn projection_rejects_a_parent_cycle() {
        let mut projection = project_structure(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        let struct_root = projection.struct_tree_root;
        let child = projection
            .entries
            .iter()
            .find(|entry| {
                entry.entry_type == StructEntryType::StructElem && entry.parent == Some(struct_root)
            })
            .map(|entry| entry.object)
            .unwrap_or_else(|| panic!("top-level struct elem"));
        let root = projection
            .entries
            .iter_mut()
            .find(|entry| entry.entry_type == StructEntryType::StructTreeRoot)
            .unwrap_or_else(|| panic!("root entry"));
        root.parent = Some(child);
        assert!(matches!(
            projection.validate_consistency(),
            Err(StructError::BadStructElem(ref message)) if message.contains("cycle")
        ));
    }

    #[test]
    fn projection_rejects_duplicate_mcids() {
        let mut projection = project_structure(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("project: {error}"));
        let kid = StructKid::MarkedContent {
            page: ObjRef::new(100),
            mcid: 42,
            artifact: false,
        };
        let first = projection
            .entries
            .iter_mut()
            .find(|entry| entry.entry_type == StructEntryType::StructElem)
            .unwrap_or_else(|| panic!("first elem"));
        first.kids.push(kid.clone());
        let second = projection
            .entries
            .iter_mut()
            .rev()
            .find(|entry| entry.entry_type == StructEntryType::StructElem)
            .unwrap_or_else(|| panic!("second elem"));
        second.kids.push(kid);
        assert_eq!(
            projection.validate_consistency(),
            Err(StructError::DuplicateMcid(42))
        );
    }

    #[test]
    fn actual_text_on_a_non_leaf_is_a_typed_error() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)])
                    .with_actual_text("not a leaf"),
                node(2, TagRole::Paragraph, 0, "body", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("tree must be structurally valid: {error}"));
        let expected = Err(StructError::ActualTextOnNonLeaf(NodeId::new(1)));
        assert_eq!(validate_structure_semantics(&tree), expected);
        assert_eq!(
            write_structure_tree(&tree, &options(1)).err(),
            expected.err()
        );
    }

    #[test]
    fn empty_actual_text_is_a_typed_error() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]),
                node(2, TagRole::Paragraph, 0, "body", vec![]).with_actual_text("   "),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("tree must be structurally valid: {error}"));
        let expected = Err(StructError::EmptyActualText(NodeId::new(2)));
        assert_eq!(validate_structure_semantics(&tree), expected);
        assert_eq!(
            write_structure_tree(&tree, &options(1)).err(),
            expected.err()
        );
    }

    #[test]
    fn artifact_with_structure_kids_is_a_typed_error() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]).with_artifact(true),
                node(2, TagRole::Paragraph, 0, "real content", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("tree must be structurally valid: {error}"));
        let expected = Err(StructError::ArtifactHasStructureKids(NodeId::new(1)));
        assert_eq!(validate_structure_semantics(&tree), expected);
        assert_eq!(
            write_structure_tree(&tree, &options(1)).err(),
            expected.err()
        );
    }

    #[test]
    fn expansion_on_a_forbidden_role_is_a_typed_error() {
        let tree = A11yTree::new(
            vec![
                node(1, TagRole::Document, 0, "", vec![NodeId::new(2)]).with_expansion("document"),
                node(2, TagRole::Paragraph, 0, "body", vec![]),
            ],
            vec![NodeId::new(1)],
        )
        .unwrap_or_else(|error| panic!("tree must be structurally valid: {error}"));
        let expected = Err(StructError::ExpansionForbiddenRole(
            NodeId::new(1),
            TagRole::Document,
        ));
        assert_eq!(validate_structure_semantics(&tree), expected);
        assert_eq!(
            write_structure_tree(&tree, &options(1)).err(),
            expected.err()
        );
        assert!(role_forbids_expansion(TagRole::Document));
        assert!(role_forbids_expansion(TagRole::Table));
        assert!(!role_forbids_expansion(TagRole::Paragraph));
        assert!(!role_forbids_expansion(TagRole::BibEntry));
    }

    #[test]
    fn alt_text_and_annotations_round_trip_together() {
        let doc = write_structure_tree(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("write: {error}"));
        let restored = read_structure_tree(&doc).unwrap_or_else(|error| panic!("read: {error}"));
        assert!(
            restored.node(NodeId::new(6)).is_none(),
            "artifact is marked content, not a structure node"
        );
        let paragraph = restored
            .node(NodeId::new(4))
            .unwrap_or_else(|| panic!("paragraph must survive"));
        assert_eq!(paragraph.actual_text.as_deref(), Some("feedback"));
        assert_eq!(paragraph.expansion.as_deref(), Some("feedback"));
        let figure = restored
            .node(NodeId::new(5))
            .unwrap_or_else(|| panic!("figure must survive"));
        assert_eq!(figure.alt_text.as_deref(), Some("chart"));
        assert_eq!(restored.validate(), Ok(()));
    }

    #[test]
    fn mcid_allocation_is_dense_unique_and_excludes_artifacts_from_parent_tree() {
        let doc = write_structure_tree(&tree_with_annotations(), &options(1))
            .unwrap_or_else(|error| panic!("write: {error}"));
        assert_eq!(doc.pages[0].mcid_ids, vec![0, 1, 2]);
        assert_eq!(doc.pages[0].artifact_mcids, vec![2]);
        let parent_tree = doc
            .object(doc.parent_tree)
            .and_then(PdfValue::as_dict)
            .and_then(|dict| dict.get("Nums"))
            .and_then(PdfValue::as_array)
            .unwrap_or_else(|| panic!("parent tree Nums"));
        let page_zero_values = parent_tree[1]
            .as_array()
            .unwrap_or_else(|| panic!("page 0 values"));
        assert_eq!(
            page_zero_values.len(),
            7,
            "two non-artifact MCRs plus five structure-element refs"
        );
        assert!(
            !page_zero_values.iter().any(|value| value
                .as_dict()
                .and_then(|dict| dict.get("MCID"))
                .and_then(PdfValue::as_int)
                == Some(2)),
            "artifact MCID must not appear in the parent tree"
        );
    }
}
