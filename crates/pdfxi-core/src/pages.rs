//! Page-level operations on parsed documents: extract, reorder, merge, split and
//! rotate, followed by a standalone classic-table save.
//!
//! This module is dependency-free and UI-free. A [`PagePlan`] is a small,
//! cloneable value object that records a sequence of [`PageOperation`]s over a
//! primary document (the receiver of [`Document::apply_page_plan`]) plus any
//! extra documents added by [`PagePlan::merge`]. Constructors such as
//! [`PagePlan::extract`], [`PagePlan::reorder`], [`PagePlan::split`] and
//! [`PagePlan::rotate`] cover the common cases; [`PagePlan::then`] composes
//! operations when more than one is needed.
//!
//! # Rendering model
//!
//! Applying a plan rebuilds a fresh page tree. Every selected page gets a new
//! dictionary whose `/Parent` points at the single new `/Pages` root, whose
//! `/Count` equals the number of selected pages, and whose `/Kids` list is in
//! plan order. Inherited attributes (`/MediaBox`, `/Resources`, `/Rotate`) are
//! flattened onto each page so the new tree needs none of its own. All other
//! in-use objects of each source are copied with remapped object numbers, so
//! content streams, fonts and other referenced resources keep their exact raw
//! bytes. Merge sources are assigned disjoint, deterministic number ranges, so
//! a source that used object `4 0 R` can coexist with another that used the same
//! number.
//!
//! # Hostile input
//!
//! Every index is bounds-checked before use and every arithmetic operation on
//! page numbers and rotations uses checked or modular arithmetic. Empty
//! selections, out-of-range indices, duplicate entries in an order plan,
//! non-multiple-of-90 `/Rotate` values, encrypted documents and xref-stream
//! documents are all rejected with structured [`Error`] variants. No code in
//! this module can panic on hostile indices.

use std::collections::{BTreeMap, HashSet};

use crate::object::{Dictionary, Object, ObjectId};
use crate::parser::load_object;
use crate::writer::{serialize_indirect, write_trailer, write_xref_section};
use crate::{Document, Error, Result};

/// Maximum depth allowed while rewriting object graphs during a plan render.
const MAX_REMAP_DEPTH: usize = 64;

/// Selects the pages a rotation applies to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RotationTarget {
    /// Rotate every page currently selected by the plan.
    All,
    /// Rotate only the given zero-based positions in the current selection.
    ///
    /// Duplicate positions are collapsed so a page is never rotated twice by
    /// one operation. Out-of-range positions are rejected when the plan is
    /// applied.
    Indices(Vec<usize>),
}

/// A single transformation step in a [`PagePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageOperation {
    /// Keep the pages at the given positions of the current selection, in the
    /// listed order.
    ///
    /// A position may repeat, in which case the page is duplicated in the
    /// output (each occurrence becomes its own page object). Positions are
    /// validated against the current selection when the plan is applied.
    Extract(Vec<usize>),
    /// Replace the current selection with a strict permutation of itself.
    ///
    /// Unlike [`PageOperation::Extract`], every position must appear exactly
    /// once; duplicates and out-of-range positions are rejected.
    Reorder(Vec<usize>),
    /// Append all pages of the listed source documents, in document order.
    ///
    /// Indices address the plan's source list, where `0` is the primary
    /// document (the receiver of [`Document::apply_page_plan`]) and `1..` are
    /// the documents passed to [`PagePlan::merge`].
    Merge(Vec<usize>),
    /// Rotate the targeted pages by `quarter_turns` clockwise quarter turns.
    ///
    /// The result is normalized to `0`, `90`, `180` or `270` degrees and composed
    /// with any inherited rotation on the source page.
    Rotate {
        /// Which pages to rotate.
        target: RotationTarget,
        /// Number of clockwise quarter turns; may be negative.
        quarter_turns: i32,
    },
}

/// One page in a plan's working selection.
#[derive(Debug, Clone)]
struct SelectionEntry {
    /// Index of the source document in the rendered source list.
    source: usize,
    /// Zero-based page index within that document.
    page: usize,
    /// Absolute rotation, in degrees, to write on the new page dictionary.
    rotation: u16,
}

/// A deterministic, reusable description of a page-level edit.
///
/// A plan can be inspected and cloned freely; it owns the extra documents used
/// by merge operations but never mutates the primary document. Apply it with
/// [`Document::apply_page_plan`].
#[derive(Debug, Clone)]
pub struct PagePlan {
    operations: Vec<PageOperation>,
    extras: Vec<Document>,
}

impl PagePlan {
    /// Return a plan with no operations, which selects every page of the
    /// primary document in document order.
    pub fn identity() -> Self {
        Self {
            operations: Vec::new(),
            extras: Vec::new(),
        }
    }

    /// Build a plan that keeps the pages at `indices` of the primary document,
    /// in the listed order. Repeated indices duplicate the corresponding page.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`] when `indices` is empty.
    pub fn extract(indices: &[usize]) -> Result<Self> {
        require_non_empty(indices)?;
        Ok(Self::identity().with_unchecked(PageOperation::Extract(indices.to_vec())))
    }

    /// Build a plan that reorders every page of the primary document according
    /// to the permutation `order`.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`] when `order` is empty and
    /// [`Error::DuplicatePageIndex`] when a position repeats. The permutation
    /// must also cover all pages; that length check happens when the plan is
    /// applied because the page count is only known then.
    pub fn reorder(order: &[usize]) -> Result<Self> {
        require_non_empty(order)?;
        validate_unique(order)?;
        Ok(Self::identity().with_unchecked(PageOperation::Reorder(order.to_vec())))
    }

    /// Build a plan that merges `documents` after the primary document, in the
    /// order given. The first document in the returned plan's source list is
    /// always the receiver of [`Document::apply_page_plan`].
    ///
    /// An empty `documents` slice is a no-op merge and is accepted.
    pub fn merge(documents: Vec<Document>) -> Self {
        let indices: Vec<usize> = (1..=documents.len()).collect();
        Self {
            operations: vec![PageOperation::Merge(indices)],
            extras: documents,
        }
    }

    /// Split the primary document into one plan per chunk of source page
    /// indices.
    ///
    /// Each returned plan extracts its chunk from the *original* document, so
    /// every plan can be applied independently with the same receiver. Chunk
    /// indices are validated against the real page count when applied.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`] when `chunks` is empty or any
    /// individual chunk is empty.
    pub fn split(chunks: &[Vec<usize>]) -> Result<Vec<Self>> {
        require_non_empty(chunks)?;
        let mut plans = Vec::with_capacity(chunks.len());
        for chunk in chunks {
            plans.push(Self::extract(chunk)?);
        }
        Ok(plans)
    }

    /// Build a plan that rotates the targeted pages of the primary document.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`] when
    /// [`RotationTarget::Indices`] is given an empty slice.
    pub fn rotate(target: RotationTarget, quarter_turns: i32) -> Result<Self> {
        if let RotationTarget::Indices(indices) = &target {
            require_non_empty(indices)?;
        }
        Ok(Self::identity().with_unchecked(PageOperation::Rotate {
            target,
            quarter_turns,
        }))
    }

    /// Append `operation` to this plan, returning a new plan.
    ///
    /// Structural invariants that do not require page counts (non-empty
    /// selections, unique order entries) are checked here; range checks against
    /// the real selection are deferred to [`Document::apply_page_plan`].
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`] or [`Error::DuplicatePageIndex`]
    /// for structurally invalid operations.
    pub fn then(mut self, operation: PageOperation) -> Result<Self> {
        validate_operation_shape(&operation)?;
        self.operations.push(operation);
        Ok(self)
    }

    /// Return the operations in the order they will be applied.
    pub fn operations(&self) -> &[PageOperation] {
        &self.operations
    }

    /// Return the additional documents referenced by merge operations.
    ///
    /// The primary document is not included; its index in the rendered source
    /// list is always `0`.
    pub fn extras(&self) -> &[Document] {
        &self.extras
    }

    /// Append an operation that has already passed its shape validation.
    fn with_unchecked(mut self, operation: PageOperation) -> Self {
        self.operations.push(operation);
        self
    }
}

/// Return the effective `/Rotate` value of page `index`, in degrees.
///
/// The value is the sum of every direct and inherited `/Rotate` along the
/// page's `/Parent` chain, normalized to `0`, `90`, `180` or `270`. Each `/Rotate`
/// entry must be an integer multiple of 90; a malformed value is reported as a
/// structured error rather than silently rounded.
///
/// # Errors
///
/// Returns [`Error::PageNotFound`] when `index` is out of range,
/// [`Error::InvalidPageRotation`] for a non-multiple-of-90 value, and structural
/// or unsupported-state errors from the source document.
pub fn effective_rotation(document: &Document, index: usize) -> Result<u16> {
    let structure = document.supported_structure()?;
    let page = structure
        .pages
        .get(index)
        .ok_or(Error::PageNotFound(index))?;

    let mut total = 0i64;
    let mut current = page.id;
    let mut seen = HashSet::new();
    loop {
        if !seen.insert(current.number) {
            return Err(Error::MalformedPageTree(
                "cycle in page /Parent chain".to_owned(),
            ));
        }
        let object = load_object(document.bytes(), &structure.xref, current)?;
        let dictionary = object.as_dictionary().ok_or_else(|| {
            Error::MalformedPageTree("page tree node is not a dictionary".to_owned())
        })?;
        if let Some(value) = dictionary.get("Rotate") {
            let degrees = value.as_i64().ok_or_else(|| {
                Error::MalformedPageTree("page /Rotate is not an integer".to_owned())
            })?;
            if degrees % 90 != 0 {
                return Err(Error::InvalidPageRotation(degrees));
            }
            total = total.wrapping_add(degrees % 360);
        }
        match dictionary.get("Parent").and_then(Object::as_reference) {
            Some(parent) => current = parent,
            None => break,
        }
    }
    Ok(normalize_rotation(total))
}

/// Apply `plan` to `document` and return a standalone classic-table PDF.
///
/// This is the implementation behind [`Document::apply_page_plan`]. The primary
/// document is always source `0`; documents supplied through
/// [`PagePlan::merge`] occupy sources `1..`.
pub(crate) fn apply(document: &Document, plan: &PagePlan) -> Result<Vec<u8>> {
    let sources: Vec<&Document> = std::iter::once(document)
        .chain(plan.extras.iter())
        .collect();
    let sources_len = sources.len();

    let mut page_counts = Vec::with_capacity(sources_len);
    for source in &sources {
        let structure = source.supported_structure()?;
        page_counts.push(structure.pages.len());
    }
    let primary_count = *page_counts.first().ok_or(Error::EmptyPageSelection)?;

    let mut selection: Vec<SelectionEntry> = Vec::with_capacity(primary_count);
    for page_index in 0..primary_count {
        selection.push(SelectionEntry {
            source: 0,
            page: page_index,
            rotation: effective_rotation(sources[0], page_index)?,
        });
    }

    for operation in &plan.operations {
        match operation {
            PageOperation::Extract(indices) => {
                require_non_empty(indices)?;
                let mut next = Vec::with_capacity(indices.len());
                for &index in indices {
                    let entry = selection.get(index).ok_or(Error::InvalidPageIndex {
                        index,
                        len: selection.len(),
                        context: "page selection",
                    })?;
                    next.push(entry.clone());
                }
                selection = next;
            }
            PageOperation::Reorder(order) => {
                require_non_empty(order)?;
                if order.len() != selection.len() {
                    return Err(Error::InvalidPageIndex {
                        index: order.len(),
                        len: selection.len(),
                        context: "reorder length",
                    });
                }
                let mut seen = HashSet::new();
                let mut next = Vec::with_capacity(order.len());
                for &index in order {
                    if !seen.insert(index) {
                        return Err(Error::DuplicatePageIndex(index));
                    }
                    let entry = selection.get(index).ok_or(Error::InvalidPageIndex {
                        index,
                        len: selection.len(),
                        context: "page order",
                    })?;
                    next.push(entry.clone());
                }
                selection = next;
            }
            PageOperation::Merge(documents) => {
                for &doc_index in documents {
                    if doc_index >= sources_len {
                        return Err(Error::InvalidSourceDocument {
                            index: doc_index,
                            len: sources_len,
                        });
                    }
                    let count =
                        *page_counts
                            .get(doc_index)
                            .ok_or(Error::InvalidSourceDocument {
                                index: doc_index,
                                len: sources_len,
                            })?;
                    for page_index in 0..count {
                        selection.push(SelectionEntry {
                            source: doc_index,
                            page: page_index,
                            rotation: effective_rotation(sources[doc_index], page_index)?,
                        });
                    }
                }
            }
            PageOperation::Rotate {
                target,
                quarter_turns,
            } => {
                let turns = quarter_turns.rem_euclid(4);
                let delta = i64::from(turns) * 90;
                let positions = rotation_positions(target, selection.len())?;
                for position in positions {
                    if let Some(entry) = selection.get_mut(position) {
                        entry.rotation = normalize_rotation(i64::from(entry.rotation) + delta);
                    }
                }
            }
        }
    }

    if selection.is_empty() {
        return Err(Error::EmptyPageSelection);
    }
    render(&sources, &selection)
}

/// Resolve a rotation target to a list of distinct selection positions.
fn rotation_positions(target: &RotationTarget, len: usize) -> Result<Vec<usize>> {
    match target {
        RotationTarget::All => Ok((0..len).collect()),
        RotationTarget::Indices(indices) => {
            require_non_empty(indices)?;
            let mut seen = HashSet::new();
            let mut positions = Vec::with_capacity(indices.len());
            for &index in indices {
                if index >= len {
                    return Err(Error::InvalidPageIndex {
                        index,
                        len,
                        context: "rotation target",
                    });
                }
                if seen.insert(index) {
                    positions.push(index);
                }
            }
            Ok(positions)
        }
    }
}

/// Render the final selection into a standalone PDF byte stream.
fn render(sources: &[&Document], selection: &[SelectionEntry]) -> Result<Vec<u8>> {
    // Object numbers belonging to a source's old catalog/page tree are replaced
    // by the new tree we build below, so they are excluded from the copy.
    let mut structural = Vec::with_capacity(sources.len());
    for source in sources {
        structural.push(collect_page_tree_ids(source)?);
    }

    // Load every remaining in-use object of each source into the typed model.
    // Stream `data` is the raw (still encoded) payload and is preserved verbatim.
    let mut object_store: Vec<BTreeMap<u32, Object>> = Vec::with_capacity(sources.len());
    for (source_index, source) in sources.iter().enumerate() {
        let structure = source.supported_structure()?;
        let mut store = BTreeMap::new();
        for (number, _offset) in structure.xref.in_use_entries() {
            if structural
                .get(source_index)
                .is_some_and(|ids| ids.contains(&number))
            {
                continue;
            }
            let object = load_object(source.bytes(), &structure.xref, ObjectId::new(number, 0))?;
            store.insert(number, object);
        }
        object_store.push(store);
    }

    // Allocate disjoint output object numbers deterministically.
    let mut next = 1u32;
    let mut allocate = || -> Result<u32> {
        let value = next;
        next = next.checked_add(1).ok_or_else(|| {
            Error::CannotSerialize("too many objects in page plan output".to_owned())
        })?;
        Ok(value)
    };
    let catalog_number = allocate()?;
    let pages_number = allocate()?;
    let mut page_numbers = Vec::with_capacity(selection.len());
    for _ in selection {
        page_numbers.push(allocate()?);
    }
    let mut remap: Vec<BTreeMap<u32, u32>> = Vec::with_capacity(sources.len());
    for store in &object_store {
        let mut map = BTreeMap::new();
        for number in store.keys() {
            map.insert(*number, allocate()?);
        }
        remap.push(map);
    }

    // Build a fresh page dictionary for each selected position.
    let mut new_pages: Vec<(u32, Object)> = Vec::with_capacity(selection.len());
    for (position, entry) in selection.iter().enumerate() {
        let source = sources[entry.source];
        let structure = source.supported_structure()?;
        let page = structure
            .pages
            .get(entry.page)
            .ok_or(Error::PageNotFound(entry.page))?;
        let original = load_object(source.bytes(), &structure.xref, page.id)?;
        let rewritten = remap_object(entry.source, &original, &remap, 0)?;
        let mut dictionary = match rewritten {
            Object::Dictionary(dictionary) => dictionary,
            _ => {
                return Err(Error::MalformedPageTree(
                    "page object is not a dictionary".to_owned(),
                ));
            }
        };

        dictionary.insert("Type".to_owned(), Object::Name("Page".to_owned()));
        dictionary.insert(
            "Parent".to_owned(),
            Object::Reference(ObjectId::new(pages_number, 0)),
        );
        dictionary.insert(
            "Rotate".to_owned(),
            Object::Integer(i64::from(entry.rotation)),
        );
        let mut media_box = Vec::with_capacity(4);
        for coordinate in [
            page.media_box.left,
            page.media_box.bottom,
            page.media_box.right,
            page.media_box.top,
        ] {
            media_box.push(if coordinate.fract() == 0.0 {
                Object::Integer(coordinate as i64)
            } else {
                Object::Real(coordinate)
            });
        }
        dictionary.insert("MediaBox".to_owned(), Object::Array(media_box));
        if let Some(resources) = page.resources {
            let value = match remap
                .get(entry.source)
                .and_then(|map| map.get(&resources.number))
            {
                Some(number) => Object::Reference(ObjectId::new(*number, 0)),
                None => Object::Null,
            };
            dictionary.insert("Resources".to_owned(), value);
        }

        let number = *page_numbers.get(position).ok_or(Error::CannotSerialize(
            "page number allocation is inconsistent".to_owned(),
        ))?;
        new_pages.push((number, Object::Dictionary(dictionary)));
    }

    // New page tree root and catalog.
    let kids: Vec<Object> = page_numbers
        .iter()
        .map(|number| Object::Reference(ObjectId::new(*number, 0)))
        .collect();
    let mut pages_dictionary = Dictionary::new();
    pages_dictionary.insert("Type".to_owned(), Object::Name("Pages".to_owned()));
    pages_dictionary.insert("Kids".to_owned(), Object::Array(kids));
    pages_dictionary.insert(
        "Count".to_owned(),
        Object::Integer(
            i64::try_from(selection.len())
                .map_err(|_| Error::CannotSerialize("page count exceeds i64".to_owned()))?,
        ),
    );

    let mut catalog = Dictionary::new();
    catalog.insert("Type".to_owned(), Object::Name("Catalog".to_owned()));
    catalog.insert(
        "Pages".to_owned(),
        Object::Reference(ObjectId::new(pages_number, 0)),
    );

    // Assemble every output object, keyed by its new object number.
    let mut output: BTreeMap<u32, Object> = BTreeMap::new();
    output.insert(catalog_number, Object::Dictionary(catalog));
    output.insert(pages_number, Object::Dictionary(pages_dictionary));
    for (number, object) in new_pages {
        output.insert(number, object);
    }
    for (source_index, store) in object_store.iter().enumerate() {
        for (number, object) in store {
            let new_number = *remap
                .get(source_index)
                .and_then(|map| map.get(number))
                .ok_or_else(|| Error::CannotSerialize("object remap is inconsistent".to_owned()))?;
            let rewritten = remap_object(source_index, object, &remap, 0)?;
            output.insert(new_number, rewritten);
        }
    }

    // Rebuild the trailer from the primary source, dropping revision-specific
    // keys and overriding /Size and /Root for the freshly written xref.
    let base_trailer = sources[0].supported_structure()?.xref.trailer();
    let mut trailer = Dictionary::new();
    for (key, value) in base_trailer {
        if matches!(
            key.as_str(),
            "Size"
                | "Root"
                | "Prev"
                | "Encrypt"
                | "XRefStm"
                | "Length"
                | "Filter"
                | "W"
                | "Index"
                | "Type"
        ) {
            continue;
        }
        trailer.insert(key, remap_object(0, &value, &remap, 0)?);
    }
    let highest = output
        .keys()
        .copied()
        .max()
        .ok_or_else(|| Error::CannotSerialize("page plan produced no objects".to_owned()))?;
    let size = highest
        .checked_add(1)
        .ok_or_else(|| Error::CannotSerialize("trailer /Size overflow".to_owned()))?;
    trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));
    trailer.insert(
        "Root".to_owned(),
        Object::Reference(ObjectId::new(catalog_number, 0)),
    );

    // Serialize objects followed by one classic xref section and trailer.
    let version = sources
        .iter()
        .map(|source| source.version())
        .max()
        .unwrap_or("1.7");
    let mut out = Vec::new();
    out.extend_from_slice(format!("%PDF-{version}\n%PDFXI\n").as_bytes());
    let mut offsets = BTreeMap::new();
    for (number, object) in &output {
        offsets.insert(*number, out.len());
        serialize_indirect(ObjectId::new(*number, 0), object, &mut out)?;
        out.push(b'\n');
    }
    let xref_offset = out.len();
    write_xref_section(&mut out, &offsets, true);
    write_trailer(&mut out, &trailer, xref_offset)?;
    Ok(out)
}

/// Collect the object numbers of a document's catalog and entire page tree.
///
/// These objects are replaced by a fresh tree during rendering and are therefore
/// excluded from the copied object store.
fn collect_page_tree_ids(document: &Document) -> Result<HashSet<u32>> {
    let structure = document.supported_structure()?;
    let mut ids = HashSet::new();
    ids.insert(structure.root_id.number);

    let catalog = load_object(document.bytes(), &structure.xref, structure.root_id)?;
    let pages_id = catalog
        .as_dictionary()
        .and_then(|dictionary| dictionary.get("Pages"))
        .and_then(Object::as_reference)
        .ok_or(Error::MissingPages)?;

    let mut stack = vec![pages_id];
    let mut seen = HashSet::new();
    while let Some(id) = stack.pop() {
        if !seen.insert(id.number) {
            continue;
        }
        ids.insert(id.number);
        let object = load_object(document.bytes(), &structure.xref, id)?;
        if let Some(dictionary) = object.as_dictionary()
            && let Some(kids) = dictionary.get("Kids").and_then(Object::as_array)
        {
            for kid in kids {
                if let Some(kid_id) = kid.as_reference() {
                    stack.push(kid_id);
                }
            }
        }
    }
    Ok(ids)
}

/// Rewrite every indirect reference in `object` using the per-source number
/// map. References to objects excluded from the output become `null`. Stream
/// payload bytes are copied unchanged.
fn remap_object(
    source: usize,
    object: &Object,
    remap: &[BTreeMap<u32, u32>],
    depth: usize,
) -> Result<Object> {
    if depth > MAX_REMAP_DEPTH {
        return Err(Error::NestingTooDeep(MAX_REMAP_DEPTH));
    }
    match object {
        Object::Reference(id) => {
            let new_number = remap
                .get(source)
                .and_then(|map| map.get(&id.number))
                .copied();
            Ok(match new_number {
                Some(number) => Object::Reference(ObjectId::new(number, 0)),
                None => Object::Null,
            })
        }
        Object::Array(items) => {
            let mut out = Vec::with_capacity(items.len());
            for item in items {
                out.push(remap_object(source, item, remap, depth + 1)?);
            }
            Ok(Object::Array(out))
        }
        Object::Dictionary(dictionary) => Ok(Object::Dictionary(remap_dictionary(
            source, dictionary, remap, depth,
        )?)),
        Object::Stream { dictionary, data } => Ok(Object::Stream {
            dictionary: remap_dictionary(source, dictionary, remap, depth)?,
            data: data.clone(),
        }),
        other => Ok(other.clone()),
    }
}

/// Rewrite the values of a dictionary with [`remap_object`].
fn remap_dictionary(
    source: usize,
    dictionary: &Dictionary,
    remap: &[BTreeMap<u32, u32>],
    depth: usize,
) -> Result<Dictionary> {
    let mut out = Dictionary::new();
    for (key, value) in dictionary {
        out.insert(key.clone(), remap_object(source, value, remap, depth + 1)?);
    }
    Ok(out)
}

/// Normalize a degree total into the canonical set `{0, 90, 180, 270}`.
fn normalize_rotation(total: i64) -> u16 {
    let normalized = total.rem_euclid(360);
    u16::try_from(normalized).unwrap_or(0)
}

/// Reject empty index selections with a structured error.
fn require_non_empty<T>(items: &[T]) -> Result<()> {
    if items.is_empty() {
        Err(Error::EmptyPageSelection)
    } else {
        Ok(())
    }
}

/// Reject duplicate positions in an order plan.
fn validate_unique(indices: &[usize]) -> Result<()> {
    let mut seen = HashSet::new();
    for &index in indices {
        if !seen.insert(index) {
            return Err(Error::DuplicatePageIndex(index));
        }
    }
    Ok(())
}

/// Check the structural invariants of an operation that need no page counts.
fn validate_operation_shape(operation: &PageOperation) -> Result<()> {
    match operation {
        PageOperation::Extract(indices) | PageOperation::Reorder(indices) => {
            require_non_empty(indices)?;
            if matches!(operation, PageOperation::Reorder(_)) {
                validate_unique(indices)?;
            }
            Ok(())
        }
        PageOperation::Merge(_) => Ok(()),
        PageOperation::Rotate { target, .. } => match target {
            RotationTarget::All => Ok(()),
            RotationTarget::Indices(indices) => require_non_empty(indices),
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Assemble a complete classic-table PDF from a list of object bodies.
    fn assemble(objects: &[String]) -> Vec<u8> {
        let mut out = String::from("%PDF-1.7\n%PDFXI\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{offset:010} 00000 n \n"));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
            objects.len() + 1
        ));
        out.into_bytes()
    }

    /// A PDF with `count` pages whose text is `Page N`.
    fn pages_pdf(count: usize) -> Vec<u8> {
        let font = 3 + 2 * count;
        let kids = (0..count)
            .map(|i| format!("{} 0 R", 3 + 2 * i))
            .collect::<Vec<_>>()
            .join(" ");
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [ {kids} ] /Count {count} >>"),
        ];
        for i in 0..count {
            let content = 3 + 2 * i + 1;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents {content} 0 R /Resources << /Font << /F1 {font} 0 R >> >> >>"
            ));
            let text = format!("BT /F1 24 Tf 72 720 Td (Page {}) Tj ET", i + 1);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                text.len(),
                text
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
        assemble(&objects)
    }

    /// A PDF with `count` pages and a `/Rotate` on the page-tree root.
    fn pages_pdf_with_root_rotate(count: usize, rotate: i64) -> Vec<u8> {
        let font = 3 + 2 * count;
        let kids = (0..count)
            .map(|i| format!("{} 0 R", 3 + 2 * i))
            .collect::<Vec<_>>()
            .join(" ");
        let mut objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            format!("<< /Type /Pages /Kids [ {kids} ] /Count {count} /Rotate {rotate} >>"),
        ];
        for i in 0..count {
            let content = 3 + 2 * i + 1;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
                 /Contents {content} 0 R /Resources << /Font << /F1 {font} 0 R >> >> >>"
            ));
            let text = format!("BT /F1 24 Tf 72 720 Td (Page {}) Tj ET", i + 1);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                text.len(),
                text
            ));
        }
        objects.push("<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned());
        assemble(&objects)
    }

    /// A one-page PDF whose content stream is a genuine zlib payload.
    fn compressed_pdf() -> (Vec<u8>, &'static [u8]) {
        const COMPRESSED: &[u8] = &[
            0x78, 0x9C, 0x73, 0x0A, 0x51, 0xD0, 0x77, 0x33, 0x54, 0x30, 0x32, 0x51, 0x08, 0x49,
            0x53, 0x30, 0x37, 0x02, 0x22, 0x03, 0x85, 0x90, 0x14, 0x05, 0x0D, 0x8F, 0xD4, 0x9C,
            0x9C, 0x7C, 0x1D, 0x05, 0xB7, 0x9C, 0xC4, 0x92, 0x54, 0x85, 0x00, 0x17, 0xB7, 0x08,
            0x4F, 0x45, 0x4D, 0x85, 0x90, 0x2C, 0x05, 0xD7, 0x10, 0x00, 0x34, 0x63, 0x0C, 0xC7,
        ];
        let mut fourth =
            format!("<< /Length {} /Filter /FlateDecode >>", COMPRESSED.len()).into_bytes();
        fourth.extend_from_slice(b"\nstream\n");
        fourth.extend_from_slice(COMPRESSED);
        fourth.extend_from_slice(b"\nendstream");
        // The stream payload is binary; assemble the object bodies as bytes.
        let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
        let mut offsets = Vec::new();
        let bodies: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
            fourth,
        ];
        for (index, body) in bodies.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", bodies.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                bodies.len() + 1
            )
            .as_bytes(),
        );
        (out, COMPRESSED)
    }

    /// Return all page texts of a reloaded PDF.
    fn texts(pdf: &[u8]) -> Vec<String> {
        let document = Document::from_bytes(pdf.to_vec()).expect("valid PDF");
        let count = document.page_count().expect("page count");
        (0..count)
            .map(|index| document.page_text(index).expect("page text"))
            .collect()
    }

    /// Return the distinct object numbers declared in a PDF byte stream.
    fn object_numbers(pdf: &[u8]) -> Vec<u32> {
        let text = String::from_utf8_lossy(pdf);
        let mut numbers = Vec::new();
        let mut cursor = 0usize;
        while let Some(relative) = text[cursor..].find(" 0 obj") {
            let position = cursor + relative;
            let digits: String = text[..position]
                .chars()
                .rev()
                .take_while(|ch| ch.is_ascii_digit())
                .collect();
            if let Ok(number) = digits.chars().rev().collect::<String>().parse::<u32>() {
                numbers.push(number);
            }
            cursor = position + 6;
        }
        numbers
    }

    #[test]
    fn extract_subset_reparses_with_correct_order_and_text() {
        let document = Document::from_bytes(pages_pdf(4)).expect("source PDF");
        let plan = PagePlan::extract(&[2, 0]).expect("extract plan");
        let saved = document.apply_page_plan(&plan).expect("apply extract");
        let reparsed = Document::from_bytes(saved.clone()).expect("saved PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 2);
        assert_eq!(
            texts(&saved),
            vec!["Page 3".to_owned(), "Page 1".to_owned()]
        );
        assert_eq!(reparsed.declared_page_count().expect("count"), Some(2));
    }

    #[test]
    fn reorder_reparses_in_the_requested_order() {
        let document = Document::from_bytes(pages_pdf(4)).expect("source PDF");
        let plan = PagePlan::reorder(&[3, 1, 2, 0]).expect("reorder plan");
        let saved = document.apply_page_plan(&plan).expect("apply reorder");
        assert_eq!(
            texts(&saved),
            vec![
                "Page 4".to_owned(),
                "Page 2".to_owned(),
                "Page 3".to_owned(),
                "Page 1".to_owned(),
            ]
        );
    }

    #[test]
    fn extract_can_duplicate_a_page() {
        let document = Document::from_bytes(pages_pdf(2)).expect("source PDF");
        let plan = PagePlan::extract(&[1, 1, 0]).expect("extract with duplicates");
        let saved = document.apply_page_plan(&plan).expect("apply");
        assert_eq!(
            texts(&saved),
            vec![
                "Page 2".to_owned(),
                "Page 2".to_owned(),
                "Page 1".to_owned(),
            ]
        );
        let reparsed = Document::from_bytes(saved).expect("saved PDF");
        let ids: Vec<_> = reparsed
            .pages()
            .expect("pages")
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids.len(), 3);
        assert_ne!(ids[0], ids[1], "duplicated pages must be distinct objects");
    }

    #[test]
    fn merge_two_documents_reparses_all_pages_and_remaps_numbers() {
        let a = Document::from_bytes(pages_pdf(2)).expect("document A");
        let b = Document::from_bytes(pages_pdf(2)).expect("document B");
        let plan = PagePlan::merge(vec![b]);
        let saved = a.apply_page_plan(&plan).expect("apply merge");
        assert_eq!(
            texts(&saved),
            vec![
                "Page 1".to_owned(),
                "Page 2".to_owned(),
                "Page 1".to_owned(),
                "Page 2".to_owned(),
            ]
        );
        let numbers = object_numbers(&saved);
        let unique: HashSet<u32> = numbers.iter().copied().collect();
        assert_eq!(
            numbers.len(),
            unique.len(),
            "object numbers must not collide"
        );
        assert!(
            numbers.iter().all(|number| *number > 0),
            "object 0 is reserved"
        );
    }

    #[test]
    fn split_into_chunks_produces_independent_pdfs() {
        let document = Document::from_bytes(pages_pdf(4)).expect("source PDF");
        let plans = PagePlan::split(&[vec![0, 1], vec![2, 3]]).expect("split plans");
        assert_eq!(plans.len(), 2);
        let first = document.apply_page_plan(&plans[0]).expect("first chunk");
        let second = document.apply_page_plan(&plans[1]).expect("second chunk");
        assert_eq!(
            texts(&first),
            vec!["Page 1".to_owned(), "Page 2".to_owned()]
        );
        assert_eq!(
            texts(&second),
            vec!["Page 3".to_owned(), "Page 4".to_owned()]
        );
    }

    #[test]
    fn rotate_writes_rotate_on_the_page_and_reparses() {
        let document = Document::from_bytes(pages_pdf(2)).expect("source PDF");
        let plan = PagePlan::rotate(RotationTarget::Indices(vec![0]), 1).expect("rotate plan");
        let saved = document.apply_page_plan(&plan).expect("apply rotate");
        let reparsed = Document::from_bytes(saved.clone()).expect("saved PDF");
        assert_eq!(effective_rotation(&reparsed, 0).expect("rotation"), 90);
        assert_eq!(effective_rotation(&reparsed, 1).expect("rotation"), 0);
        assert!(String::from_utf8_lossy(&saved).contains("/Rotate 90"));
    }

    #[test]
    fn rotate_all_and_negative_turns_normalize_correctly() {
        let document = Document::from_bytes(pages_pdf(2)).expect("source PDF");
        let plan = PagePlan::rotate(RotationTarget::All, -1).expect("rotate plan");
        let saved = document.apply_page_plan(&plan).expect("apply rotate");
        let reparsed = Document::from_bytes(saved).expect("saved PDF");
        assert_eq!(effective_rotation(&reparsed, 0).expect("rotation"), 270);
        assert_eq!(effective_rotation(&reparsed, 1).expect("rotation"), 270);
    }

    #[test]
    fn inherited_rotation_is_flattened_and_composed() {
        let document = Document::from_bytes(pages_pdf_with_root_rotate(2, 90)).expect("source PDF");
        assert_eq!(
            effective_rotation(&document, 0).expect("inherited rotation"),
            90
        );
        assert_eq!(
            effective_rotation(&document, 1).expect("inherited rotation"),
            90
        );

        let plan = PagePlan::rotate(RotationTarget::Indices(vec![0]), 1).expect("rotate plan");
        let saved = document.apply_page_plan(&plan).expect("apply rotate");
        let reparsed = Document::from_bytes(saved.clone()).expect("saved PDF");
        assert_eq!(
            effective_rotation(&reparsed, 0).expect("composed rotation"),
            180
        );
        assert_eq!(
            effective_rotation(&reparsed, 1).expect("inherited rotation kept"),
            90
        );
        // The new /Pages root carries no /Rotate; rotation lives on each page.
        let text = String::from_utf8_lossy(&saved);
        let pages_root = text
            .lines()
            .find(|line| line.contains("/Type /Pages"))
            .expect("pages root")
            .to_owned();
        assert!(
            !pages_root.contains("/Rotate"),
            "inherited rotation must be flattened"
        );
    }

    #[test]
    fn page_tree_metadata_is_correct_after_extract() {
        let document = Document::from_bytes(pages_pdf(5)).expect("source PDF");
        let plan = PagePlan::extract(&[4, 2, 0]).expect("extract plan");
        let saved = document.apply_page_plan(&plan).expect("apply");
        let reparsed = Document::from_bytes(saved.clone()).expect("saved PDF");
        assert_eq!(reparsed.page_count().expect("count"), 3);
        assert_eq!(reparsed.declared_page_count().expect("declared"), Some(3));
        let ids: Vec<_> = reparsed
            .pages()
            .expect("pages")
            .iter()
            .map(|p| p.id)
            .collect();
        assert_eq!(ids.len(), 3);
        let unique: HashSet<_> = ids.iter().collect();
        assert_eq!(unique.len(), 3, "kept pages must have distinct objects");

        // Every /Parent in the output refers to the single new /Pages root.
        let text = String::from_utf8_lossy(&saved);
        let parents: HashSet<&str> = text
            .match_indices("/Parent ")
            .filter_map(|(index, _)| {
                let rest = &text[index + "/Parent ".len()..];
                let end = rest.find(' ')?;
                Some(&rest[..end])
            })
            .collect();
        assert_eq!(parents.len(), 1, "all pages must share one /Pages parent");
    }

    #[test]
    fn compressed_stream_bytes_are_preserved_after_rotation() {
        let (source, compressed) = compressed_pdf();
        let document = Document::from_bytes(source).expect("compressed source");
        assert_eq!(document.page_text(0).expect("text"), "Hello, Flate PDFXI!");
        let plan = PagePlan::rotate(RotationTarget::All, 1).expect("rotate plan");
        let saved = document.apply_page_plan(&plan).expect("apply rotate");
        assert!(
            saved
                .windows(compressed.len())
                .any(|window| window == compressed),
            "raw compressed stream bytes must be preserved"
        );
        let reparsed = Document::from_bytes(saved).expect("saved PDF");
        assert_eq!(reparsed.page_text(0).expect("text"), "Hello, Flate PDFXI!");
        assert_eq!(effective_rotation(&reparsed, 0).expect("rotation"), 90);
    }

    #[test]
    fn empty_selections_are_rejected() {
        assert!(matches!(
            PagePlan::extract(&[]),
            Err(Error::EmptyPageSelection)
        ));
        assert!(matches!(
            PagePlan::reorder(&[]),
            Err(Error::EmptyPageSelection)
        ));
        assert!(matches!(
            PagePlan::rotate(RotationTarget::Indices(Vec::new()), 1),
            Err(Error::EmptyPageSelection)
        ));
        assert!(matches!(
            PagePlan::split(&[]),
            Err(Error::EmptyPageSelection)
        ));
        assert!(matches!(
            PagePlan::split(&[vec![0], Vec::new()]),
            Err(Error::EmptyPageSelection)
        ));
    }

    #[test]
    fn out_of_range_indices_are_rejected_without_panicking() {
        let document = Document::from_bytes(pages_pdf(3)).expect("source PDF");
        let plan = PagePlan::extract(&[99]).expect("plan constructor accepts raw index");
        assert!(matches!(
            document.apply_page_plan(&plan),
            Err(Error::InvalidPageIndex { index: 99, .. })
        ));
        let plan = PagePlan::extract(&[usize::MAX]).expect("hostile index");
        assert!(document.apply_page_plan(&plan).is_err());
        let reorder = PagePlan::extract(&[0, 1])
            .expect("two pages")
            .then(PageOperation::Reorder(vec![0, 99]))
            .expect("reorder");
        assert!(document.apply_page_plan(&reorder).is_err());
    }

    #[test]
    fn duplicate_index_in_reorder_is_rejected() {
        assert!(matches!(
            PagePlan::reorder(&[0, 0, 1]),
            Err(Error::DuplicatePageIndex(0))
        ));
        let document = Document::from_bytes(pages_pdf(3)).expect("source PDF");
        let plan = PagePlan::identity()
            .then(PageOperation::Reorder(vec![0, 1]))
            .expect("short permutation")
            .then(PageOperation::Reorder(vec![0, 1]))
            .expect("valid second permutation");
        assert!(matches!(
            document.apply_page_plan(&plan),
            Err(Error::InvalidPageIndex {
                context: "reorder length",
                ..
            })
        ));
    }

    #[test]
    fn invalid_source_document_index_is_rejected() {
        let document = Document::from_bytes(pages_pdf(1)).expect("source PDF");
        let plan = PagePlan {
            operations: vec![PageOperation::Merge(vec![5])],
            extras: Vec::new(),
        };
        assert!(matches!(
            document.apply_page_plan(&plan),
            Err(Error::InvalidSourceDocument { index: 5, .. })
        ));
    }

    #[test]
    fn non_multiple_of_90_rotation_is_rejected() {
        let source = pages_pdf_with_root_rotate(1, 45);
        let document = Document::from_bytes(source).expect("source PDF");
        let plan = PagePlan::identity();
        assert!(matches!(
            document.apply_page_plan(&plan),
            Err(Error::InvalidPageRotation(45))
        ));
    }

    #[test]
    fn empty_document_selection_is_rejected() {
        let document = Document::from_bytes(pdfxi_testkit::multipage_pdf(0)).expect("zero pages");
        assert!(matches!(
            document.apply_page_plan(&PagePlan::identity()),
            Err(Error::EmptyPageSelection)
        ));
    }

    /// Build the stream object body for a single text-showing page.
    fn content_object(text: &str) -> String {
        let content = format!("BT /F1 24 Tf 72 720 Td ({text}) Tj ET");
        format!(
            "<< /Length {} >>\nstream\n{}\nendstream",
            content.len(),
            content
        )
    }

    /// A PDF whose page tree has an intermediate branch carrying `/Rotate 90`.
    fn nested_pages_pdf() -> Vec<u8> {
        let objects = vec![
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R 6 0 R] /Count 3 >>".to_owned(),
            "<< /Type /Pages /Kids [4 0 R 5 0 R] /Count 2 /Rotate 90 \
             /MediaBox [0 0 612 792] /Resources << /Font << /F1 10 0 R >> >> >>"
                .to_owned(),
            "<< /Type /Page /Parent 3 0 R /Contents 7 0 R >>".to_owned(),
            "<< /Type /Page /Parent 3 0 R /Contents 8 0 R >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 9 0 R \
             /Resources << /Font << /F1 10 0 R >> >> >>"
                .to_owned(),
            content_object("Page 1"),
            content_object("Page 2"),
            content_object("Page 3"),
            "<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_owned(),
        ];
        assemble(&objects)
    }

    #[test]
    fn nested_page_tree_with_branch_rotation_is_flattened_correctly() {
        let document = Document::from_bytes(nested_pages_pdf()).expect("nested PDF");
        assert_eq!(document.page_count().expect("count"), 3);
        assert_eq!(effective_rotation(&document, 0).expect("rotation"), 90);
        assert_eq!(effective_rotation(&document, 1).expect("rotation"), 90);
        assert_eq!(effective_rotation(&document, 2).expect("rotation"), 0);

        let plan = PagePlan::extract(&[2, 0]).expect("extract plan");
        let saved = document.apply_page_plan(&plan).expect("apply");
        let reparsed = Document::from_bytes(saved.clone()).expect("saved PDF");
        assert_eq!(reparsed.declared_page_count().expect("count"), Some(2));
        assert_eq!(
            texts(&saved),
            vec!["Page 3".to_owned(), "Page 1".to_owned()]
        );
        assert_eq!(effective_rotation(&reparsed, 0).expect("rotation"), 0);
        assert_eq!(effective_rotation(&reparsed, 1).expect("rotation"), 90);
        let text = String::from_utf8_lossy(&saved);
        let parents: HashSet<&str> = text
            .match_indices("/Parent ")
            .filter_map(|(index, _)| {
                let rest = &text[index + "/Parent ".len()..];
                Some(&rest[..rest.find(' ')?])
            })
            .collect();
        assert_eq!(parents.len(), 1, "all pages share one new /Pages root");
    }

    #[test]
    fn merge_remaps_internal_content_references() {
        let a = Document::from_bytes(pdfxi_testkit::text_pdf("Alpha")).expect("document A");
        let b = Document::from_bytes(pdfxi_testkit::text_pdf("Beta")).expect("document B");
        let plan = PagePlan::merge(vec![b]);
        let saved = a.apply_page_plan(&plan).expect("apply merge");
        let reparsed = Document::from_bytes(saved).expect("saved PDF");
        let pages = reparsed.pages().expect("pages");
        assert_eq!(pages.len(), 2);
        assert_eq!(pages[0].contents.len(), 1);
        assert_eq!(pages[1].contents.len(), 1);
        assert_ne!(
            pages[0].contents[0], pages[1].contents[0],
            "each source's content stream must get a distinct remapped number"
        );
        assert_eq!(reparsed.page_text(0).expect("text"), "Alpha");
        assert_eq!(reparsed.page_text(1).expect("text"), "Beta");
    }

    #[test]
    fn encrypted_and_xref_stream_sources_are_rejected() {
        let mut encrypted = String::from_utf8(pages_pdf(1)).expect("ASCII PDF");
        encrypted = encrypted.replacen("trailer\n<<", "trailer\n<< /Encrypt 9 0 R", 1);
        let document = Document::from_bytes(encrypted.into_bytes()).expect("parses");
        assert!(matches!(
            document.apply_page_plan(&PagePlan::identity()),
            Err(Error::UnsupportedState(_))
        ));

        let source = pages_pdf(1);
        let object_offset = String::from_utf8_lossy(&source)
            .find("1 0 obj")
            .expect("first object") as u64;
        let marker = "startxref\n";
        let text = String::from_utf8(source).expect("ASCII PDF");
        let at = text.find(marker).expect("startxref") + marker.len();
        let end = at + text[at..].find('\n').expect("line end");
        let stream_like = format!("{}{}{}", &text[..at], object_offset, &text[end..]);
        let document = Document::from_bytes(stream_like.into_bytes()).expect("header parses");
        assert!(matches!(
            document.apply_page_plan(&PagePlan::identity()),
            Err(Error::UnsupportedState(_))
        ));
    }
}
