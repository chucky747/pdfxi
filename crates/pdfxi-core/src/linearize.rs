//! Writing of fully linearized PDFs with classic cross-reference tables.
//!
//! This module is the write-side counterpart of [`crate::linearization`]. It
//! produces a byte stream laid out according to ISO 32000-1, Annex F so that a
//! reader can fetch and render the first page before the rest of the file
//! arrives:
//!
//! 1. a `%PDF-` header followed immediately by the `/Linearized` parameter
//!    dictionary,
//! 2. a hint stream holding a page-offset hint table and a shared-object hint
//!    table,
//! 3. the objects needed for the first page (including the catalog, page tree
//!    and shared resources), ordered deterministically by object number,
//! 4. a **first-page** classic `xref` table covering those objects and a trailer
//!    with `/Size` and `/Root`,
//! 5. the remaining objects, ordered deterministically by object number,
//! 6. a **main** classic `xref` table whose trailer carries `/Prev` pointing at
//!    the first-page `xref`, followed by the single final `startxref` and
//!    `%%EOF`.
//!
//! The layout is computed with a two-pass fixed-point assembly. Every numeric
//! field that depends on the final layout (the `/L`, `/H`, `/E` and `/T` values
//! and all offsets stored in the hint stream) occupies a fixed byte width, so
//! the second pass can be filled in without moving any object. The writer is
//! therefore fully deterministic: the same object set always yields the same
//! bytes.
//!
//! Every document is linearized; there is deliberately **no small-document
//! fallback**. A fallback that emitted an ordinary PDF for tiny inputs would
//! make the layout depend on size in a way that is hard to test and would
//! violate the promise that large documents are linearized. Documents with no
//! pages are rejected instead.
//!
//! The hint stream uses fixed-width, big-endian 32-bit fields. This is a
//! straightforward, self-describing variant of the Annex F tables: the page
//! table header includes the page count so the stream can be parsed on its own
//! with [`parse_hint_stream`]. All offsets are u32, so files larger than 4 GiB
//! are rejected with [`crate::Error::CannotSerialize`].

use std::collections::{BTreeMap, BTreeSet, HashMap};

use crate::object::{Dictionary, Object, ObjectId};
use crate::writer;
use crate::{Error, Result};

/// Number of decimal digits used for every layout-dependent value in the
/// linearization dictionary.
///
/// Twelve digits cover files up to one terabyte while keeping the dictionary a
/// fixed size across both assembly passes.
const NUM_WIDTH: usize = 12;

/// Maximum recursion depth used while walking the page tree or object values.
const MAX_DEPTH: u8 = 64;

/// Upper bound on pages or shared objects accepted while parsing a hint stream.
const MAX_HINT_ENTRIES: u32 = 1_000_000;

/// Options controlling linearized output.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearizationOptions {
    /// Version token written in the `%PDF-` header, for example `"1.7"`.
    ///
    /// Only ASCII digits and dots are accepted. The default is `"1.7"`.
    pub version: String,
}

impl Default for LinearizationOptions {
    fn default() -> Self {
        Self {
            version: "1.7".to_owned(),
        }
    }
}

/// One entry of the page-offset hint table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageHint {
    /// Object number of the page dictionary.
    pub page_object: u32,
    /// Absolute byte offset of the page object in the produced file.
    pub object_offset: u32,
    /// Combined length of the page's content streams in bytes.
    pub content_length: u32,
    /// Number of shared objects referenced by this page.
    pub shared_count: u32,
    /// Indices into the shared-object hint table for this page's shared objects.
    pub shared_indices: Vec<u32>,
}

/// One entry of the shared-object hint table.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SharedObjectHint {
    /// Object number of the shared object.
    pub object_number: u32,
    /// Absolute byte offset of the object in the produced file.
    pub object_offset: u32,
    /// Number of pages that reference this object.
    pub page_reference_count: u32,
}

/// Decoded contents of a linearization hint stream.
///
/// Returned by [`parse_hint_stream`] so callers can compare the declared hints
/// with the real offsets in a produced file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HintStream {
    /// Number of pages described by the page-offset table.
    pub page_count: u32,
    /// Least number of objects found on a page.
    pub least_objects_per_page: u32,
    /// Greatest number of objects found on a page.
    pub greatest_objects_per_page: u32,
    /// Least combined content-stream length across pages.
    pub least_content_length: u32,
    /// Greatest combined content-stream length across pages.
    pub greatest_content_length: u32,
    /// Content-stream length of the first page.
    pub first_page_content_length: u32,
    /// Absolute offset of the first page object.
    pub first_page_offset: u32,
    /// Per-page entries in document order.
    pub page_hints: Vec<PageHint>,
    /// Absolute offset of the first shared object.
    pub first_shared_object_offset: u32,
    /// Shared-object entries in ascending object-number order.
    pub shared_hints: Vec<SharedObjectHint>,
}

/// Summary of the byte layout chosen for a linearized document.
///
/// Returned alongside the bytes by [`write_linearized_document_with_layout`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LinearizedLayout {
    /// Absolute offset of the first-page `xref` table.
    pub first_xref_offset: u64,
    /// Absolute offset of the main `xref` table (the final `startxref` target).
    pub main_xref_offset: u64,
    /// Object number assigned to the linearization dictionary.
    pub lin_dict_id: u32,
    /// Object number assigned to the hint stream.
    pub hint_stream_id: u32,
    /// Object numbers placed in the first-page section, in physical order.
    pub first_section_objects: Vec<u32>,
    /// Object numbers placed in the main section, in physical order.
    pub main_section_objects: Vec<u32>,
    /// Absolute offsets of every object header, keyed by object number.
    pub object_offsets: BTreeMap<u32, u64>,
    /// Absolute offset of the hint stream data (the `/H` first offset).
    pub hint_offset: u64,
    /// Length of the complete hint stream data.
    pub hint_length: u64,
    /// Absolute offset of the shared-object hint table.
    pub shared_hint_offset: u64,
    /// Length of the shared-object hint table.
    pub shared_hint_length: u64,
    /// Total number of bytes in the produced file (the `/L` value).
    pub file_length: u64,
    /// Declared trailer `/Size`.
    pub size: u32,
    /// First page object number (the `/O` value).
    pub first_page_object: u32,
    /// Page count (the `/N` value).
    pub page_count: u32,
}

/// A small byte cursor used by [`parse_hint_stream`].
struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn u32(&mut self) -> Result<u32> {
        let end = self
            .position
            .checked_add(4)
            .ok_or_else(|| Error::MalformedHintStream("hint stream offset overflow".to_owned()))?;
        let slice = self.data.get(self.position..end).ok_or_else(|| {
            Error::MalformedHintStream("hint stream ends inside a field".to_owned())
        })?;
        let bytes: [u8; 4] = [slice[0], slice[1], slice[2], slice[3]];
        self.position = end;
        Ok(u32::from_be_bytes(bytes))
    }

    fn bounded_count(&mut self, label: &str) -> Result<usize> {
        let count = self.u32()?;
        if count == 0 || count > MAX_HINT_ENTRIES {
            return Err(Error::MalformedHintStream(format!(
                "{label} count {count} is out of range"
            )));
        }
        usize::try_from(count)
            .map_err(|_| Error::MalformedHintStream(format!("{label} count does not fit usize")))
    }
}

/// Parse a raw hint stream previously written by this module.
///
/// The complete hint stream (page-offset table followed by shared-object
/// table) is self-describing: the page count lives in the page-table header and
/// the shared-object count lives in the shared-table header. Both tables use
/// fixed-width big-endian u32 fields. A truncated or inconsistent stream is
/// rejected with [`Error::MalformedHintStream`] rather than panicking.
///
/// # Errors
///
/// Returns [`Error::MalformedHintStream`] when the slice is truncated, a count
/// is out of range, or trailing bytes remain after both tables.
pub fn parse_hint_stream(data: &[u8]) -> Result<HintStream> {
    let mut cursor = Cursor::new(data);
    let page_count = cursor.bounded_count("page")?;
    let least_objects_per_page = cursor.u32()?;
    let greatest_objects_per_page = cursor.u32()?;
    let least_content_length = cursor.u32()?;
    let greatest_content_length = cursor.u32()?;
    let first_page_content_length = cursor.u32()?;
    let first_page_offset = cursor.u32()?;

    let mut page_hints = Vec::with_capacity(page_count);
    for _ in 0..page_count {
        let page_object = cursor.u32()?;
        let object_offset = cursor.u32()?;
        let content_length = cursor.u32()?;
        let shared_count = cursor.u32()?;
        if shared_count > MAX_HINT_ENTRIES {
            return Err(Error::MalformedHintStream(
                "page shared-object count is out of range".to_owned(),
            ));
        }
        let mut shared_indices = Vec::with_capacity(shared_count as usize);
        for _ in 0..shared_count {
            shared_indices.push(cursor.u32()?);
        }
        page_hints.push(PageHint {
            page_object,
            object_offset,
            content_length,
            shared_count,
            shared_indices,
        });
    }

    // The shared table may be empty, so its count is read without the
    // non-zero requirement used for the page count.
    let shared_count = cursor.u32()?;
    if shared_count > MAX_HINT_ENTRIES {
        return Err(Error::MalformedHintStream(
            "shared-object count is out of range".to_owned(),
        ));
    }
    let first_shared_object_offset = cursor.u32()?;
    let mut shared_hints = Vec::with_capacity(shared_count as usize);
    for _ in 0..shared_count {
        shared_hints.push(SharedObjectHint {
            object_number: cursor.u32()?,
            object_offset: cursor.u32()?,
            page_reference_count: cursor.u32()?,
        });
    }
    if cursor.position != data.len() {
        return Err(Error::MalformedHintStream(
            "trailing bytes after hint tables".to_owned(),
        ));
    }

    Ok(HintStream {
        page_count: u32::try_from(page_count)
            .map_err(|_| Error::MalformedHintStream("page count exceeds u32".to_owned()))?,
        least_objects_per_page,
        greatest_objects_per_page,
        least_content_length,
        greatest_content_length,
        first_page_content_length,
        first_page_offset,
        page_hints,
        first_shared_object_offset,
        shared_hints,
    })
}

/// Logical decisions that do not depend on any byte offset.
struct Plan {
    root: ObjectId,
    first_page_id: ObjectId,
    version: String,
    page_ids: Vec<u32>,
    shared_ids: Vec<ObjectId>,
    first_section_ids: Vec<ObjectId>,
    main_section_ids: Vec<ObjectId>,
    page_content_lengths: Vec<u32>,
    shared_page_counts: Vec<u32>,
    shared_indices_per_page: Vec<Vec<u32>>,
    least_objects_per_page: u32,
    greatest_objects_per_page: u32,
    least_content_length: u32,
    greatest_content_length: u32,
    lin_dict_id: ObjectId,
    hint_stream_id: ObjectId,
    size: u32,
    /// Owned copies of the input objects, used during assembly.
    objects: HashMap<u32, (ObjectId, Object)>,
}

/// Numeric values embedded in the linearization dictionary.
#[derive(Clone, Copy)]
struct Numbers {
    file_length: u64,
    first_page_end: u64,
    hint_offset: u64,
    hint_length: u64,
    shared_hint_offset: Option<u64>,
    shared_hint_length: u64,
    main_xref_offset: u64,
}

impl Numbers {
    fn zero() -> Self {
        Self {
            file_length: 0,
            first_page_end: 0,
            hint_offset: 0,
            hint_length: 0,
            // Keep the /H array at four fixed-width entries in both passes;
            // a "-1 0" placeholder here would change the lin dict length.
            shared_hint_offset: Some(0),
            shared_hint_length: 0,
            main_xref_offset: 0,
        }
    }
}

/// Layout-dependent offsets embedded in the hint stream.
struct EmbedOffsets {
    page_offsets: Vec<u64>,
    first_page_offset: u64,
    shared_offsets: Vec<u64>,
    first_shared_offset: u64,
}

impl EmbedOffsets {
    fn zero(plan: &Plan) -> Self {
        Self {
            page_offsets: vec![0; plan.page_ids.len()],
            first_page_offset: 0,
            shared_offsets: vec![0; plan.shared_ids.len()],
            first_shared_offset: 0,
        }
    }
}

/// Byte layout measured during one assembly pass.
#[derive(Clone, PartialEq, Eq)]
struct Layout {
    object_offsets: BTreeMap<u32, u64>,
    hint_data_offset: u64,
    page_table_len: u64,
    shared_table_offset: u64,
    shared_table_len: u64,
    first_xref_offset: u64,
    main_xref_offset: u64,
    file_length: u64,
}

/// Result of one assembly pass.
struct Assembled {
    bytes: Vec<u8>,
    layout: Layout,
}

/// Write a fully linearized PDF from a set of indirect objects.
///
/// `objects` must contain `root` (the document catalog) and `first_page` (the
/// first leaf page in document order). The page tree is walked directly from
/// this object set to determine page order and which objects are shared between
/// pages; those decisions are then used to place the first page ahead of the
/// rest and to build the hint tables.
///
/// The returned bytes begin with `%PDF-` and end with `startxref`, the main xref
/// offset and `%%EOF`. They can be handed straight to
/// [`crate::Document::from_bytes`], which will see a conventional classic-xref
/// file with an additional `/Prev` section.
///
/// # Errors
///
/// Returns [`Error::LinearizationFailed`] for an empty document, a missing
/// first page, an invalid version or a malformed page tree,
/// [`Error::InvalidObjectId`] for duplicate or reserved object numbers, and
/// [`Error::CannotSerialize`] when a value exceeds the fixed field widths.
pub fn write_linearized_document(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    first_page: ObjectId,
    options: &LinearizationOptions,
) -> Result<Vec<u8>> {
    let (bytes, _) = write_linearized_document_with_layout(objects, root, first_page, options)?;
    Ok(bytes)
}

/// Write a linearized PDF and also return a summary of the chosen layout.
///
/// This is the same writer as [`write_linearized_document`] with the measured
/// offsets exposed, which is useful for validation and for callers that want to
/// report where objects were placed.
///
/// # Errors
///
/// Returns the same errors as [`write_linearized_document`], plus
/// [`Error::LinearizationFailed`] if the two fixed-point passes do not agree.
pub fn write_linearized_document_with_layout(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    first_page: ObjectId,
    options: &LinearizationOptions,
) -> Result<(Vec<u8>, LinearizedLayout)> {
    let plan = build_plan(objects, root, first_page, options)?;

    // Pass one: assemble with placeholder values to measure every offset. The
    // numeric fields are fixed width, so these offsets are final.
    let first = assemble(&plan, &Numbers::zero(), &EmbedOffsets::zero(&plan))?;

    // Turn the measured layout into the values that must be embedded in the
    // dictionary and hint stream for the second (final) pass.
    let numbers = Numbers {
        file_length: first.layout.file_length,
        first_page_end: first.layout.first_xref_offset,
        hint_offset: first.layout.hint_data_offset,
        hint_length: merged_hint_length(&first.layout),
        shared_hint_offset: Some(first.layout.shared_table_offset),
        shared_hint_length: first.layout.shared_table_len,
        main_xref_offset: first.layout.main_xref_offset,
    };
    let embed = embed_from_layout(&plan, &first.layout)?;

    let second = assemble(&plan, &numbers, &embed)?;

    // Guard the fixed-point assumption explicitly: if any embedded value were
    // to change the layout, the numbers in the file would be false.
    if second.layout != first.layout
        || second.layout.hint_data_offset != numbers.hint_offset
        || merged_hint_length(&second.layout) != numbers.hint_length
        || second.layout.shared_table_offset != numbers.shared_hint_offset.unwrap_or(0)
    {
        return Err(Error::LinearizationFailed(
            "linearized layout did not converge".to_owned(),
        ));
    }

    let final_hint_length = merged_hint_length(&second.layout);
    let layout = LinearizedLayout {
        first_xref_offset: second.layout.first_xref_offset,
        main_xref_offset: second.layout.main_xref_offset,
        lin_dict_id: plan.lin_dict_id.number,
        hint_stream_id: plan.hint_stream_id.number,
        first_section_objects: first_section_with_generated(&plan),
        main_section_objects: plan.main_section_ids.iter().map(|id| id.number).collect(),
        object_offsets: second.layout.object_offsets,
        hint_offset: second.layout.hint_data_offset,
        hint_length: final_hint_length,
        shared_hint_offset: second.layout.shared_table_offset,
        shared_hint_length: second.layout.shared_table_len,
        file_length: second.layout.file_length,
        size: plan.size,
        first_page_object: plan.first_page_id.number,
        page_count: u32::try_from(plan.page_ids.len())
            .map_err(|_| Error::LinearizationFailed("page count exceeds u32".to_owned()))?,
    };
    Ok((second.bytes, layout))
}

/// Total hint data length (page table plus shared table).
fn merged_hint_length(layout: &Layout) -> u64 {
    layout.page_table_len.saturating_add(layout.shared_table_len)
}

/// Build the offsets to embed in the hint stream from a measured layout.
fn embed_from_layout(plan: &Plan, layout: &Layout) -> Result<EmbedOffsets> {
    let mut page_offsets = Vec::with_capacity(plan.page_ids.len());
    for number in &plan.page_ids {
        page_offsets.push(*layout.object_offsets.get(number).ok_or_else(|| {
            Error::LinearizationFailed("page missing from measured layout".to_owned())
        })?);
    }
    let first_page_offset = *page_offsets
        .first()
        .ok_or_else(|| Error::LinearizationFailed("no pages to embed".to_owned()))?;

    let mut shared_offsets = Vec::with_capacity(plan.shared_ids.len());
    for id in &plan.shared_ids {
        shared_offsets.push(*layout.object_offsets.get(&id.number).ok_or_else(|| {
            Error::LinearizationFailed("shared object missing from measured layout".to_owned())
        })?);
    }
    let first_shared_offset = shared_offsets
        .first()
        .copied()
        .ok_or_else(|| Error::LinearizationFailed("no shared objects to embed".to_owned()))?;

    Ok(EmbedOffsets {
        page_offsets,
        first_page_offset,
        shared_offsets,
        first_shared_offset,
    })
}

/// First-page section including the two generated objects, in physical order.
fn first_section_with_generated(plan: &Plan) -> Vec<u32> {
    let mut numbers = vec![plan.lin_dict_id.number, plan.hint_stream_id.number];
    numbers.extend(plan.first_section_ids.iter().map(|id| id.number));
    numbers
}

/// Build the logical plan from raw objects.
fn build_plan(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    first_page: ObjectId,
    options: &LinearizationOptions,
) -> Result<Plan> {
    if options.version.is_empty()
        || !options
            .version
            .bytes()
            .all(|byte| byte.is_ascii_digit() || byte == b'.')
    {
        return Err(Error::LinearizationFailed(
            "invalid PDF version token".to_owned(),
        ));
    }
    if objects.is_empty() {
        return Err(Error::LinearizationFailed(
            "cannot linearize an empty object set".to_owned(),
        ));
    }

    let mut map: HashMap<u32, (ObjectId, &Object)> = HashMap::new();
    let mut highest = 0u32;
    for (id, object) in objects {
        if id.number == 0 {
            return Err(Error::InvalidObjectId(
                "object number 0 is reserved".to_owned(),
            ));
        }
        if map.insert(id.number, (*id, object)).is_some() {
            return Err(Error::InvalidObjectId(format!(
                "duplicate object number {}",
                id.number
            )));
        }
        highest = highest.max(id.number);
    }
    if !map.contains_key(&root.number) {
        return Err(Error::MissingRoot);
    }
    if !map.contains_key(&first_page.number) {
        return Err(Error::LinearizationFailed(
            "first page object is not in the object set".to_owned(),
        ));
    }

    // Walk the page tree from the catalog's /Pages entry.
    let catalog = map.get(&root.number).ok_or(Error::MissingRoot)?.1;
    let pages_id = catalog
        .as_dictionary()
        .and_then(|dict| dict.get("Pages"))
        .and_then(Object::as_reference)
        .ok_or(Error::MissingPages)?;
    let mut visited = BTreeSet::new();
    let mut tree_nodes = BTreeSet::new();
    tree_nodes.insert(root.number);
    let mut page_ids = Vec::new();
    walk_pages(
        &map,
        pages_id.number,
        &mut visited,
        &mut tree_nodes,
        &mut page_ids,
        0,
    )?;
    if page_ids.is_empty() {
        return Err(Error::LinearizationFailed(
            "cannot linearize a document with no pages".to_owned(),
        ));
    }
    if page_ids.first() != Some(&first_page.number) {
        return Err(Error::LinearizationFailed(
            "supplied first page is not the first leaf in the page tree".to_owned(),
        ));
    }

    // Direct object references of each page, used to classify shared objects.
    let mut page_deps = Vec::with_capacity(page_ids.len());
    let mut page_content_lengths = Vec::with_capacity(page_ids.len());
    for number in &page_ids {
        let object = map
            .get(number)
            .ok_or_else(|| Error::LinearizationFailed("page missing from object map".to_owned()))?
            .1;
        let mut deps = BTreeSet::new();
        collect_references(object, &mut deps, 0);
        page_deps.push(deps);
        page_content_lengths.push(to_u32(page_content_length(object, &map))?);
    }

    let mut ref_counts: HashMap<u32, usize> = HashMap::new();
    for deps in &page_deps {
        for number in deps {
            *ref_counts.entry(*number).or_insert(0) += 1;
        }
    }

    let mut shared_numbers: BTreeSet<u32> = tree_nodes;
    for (number, count) in ref_counts {
        if count >= 2 {
            shared_numbers.insert(number);
        }
    }

    let mut shared_ids = Vec::with_capacity(shared_numbers.len());
    for number in &shared_numbers {
        shared_ids.push(
            map.get(number)
                .ok_or_else(|| {
                    Error::LinearizationFailed("shared object missing from map".to_owned())
                })?
                .0,
        );
    }
    let shared_index: HashMap<u32, usize> = shared_numbers
        .iter()
        .enumerate()
        .map(|(index, number)| (*number, index))
        .collect();

    let mut shared_page_counts = vec![0u32; shared_ids.len()];
    let mut shared_indices_per_page = Vec::with_capacity(page_ids.len());
    for deps in &page_deps {
        let mut indices = Vec::new();
        for number in deps {
            if let Some(index) = shared_index.get(number) {
                indices.push(u32::try_from(*index).map_err(|_| {
                    Error::LinearizationFailed("shared index exceeds u32".to_owned())
                })?);
                if let Some(count) = shared_page_counts.get_mut(*index) {
                    *count = count.saturating_add(1);
                }
            }
        }
        indices.sort_unstable();
        shared_indices_per_page.push(indices);
    }

    // The first-page section is the shared objects plus everything the first
    // page references directly. Everything else goes into the main section.
    let mut first_numbers = shared_numbers.clone();
    if let Some(first_deps) = page_deps.first() {
        first_numbers.extend(first_deps.iter().copied());
    }
    first_numbers.insert(first_page.number);

    let lin_number = highest
        .checked_add(1)
        .ok_or_else(|| Error::CannotSerialize("object number space is exhausted".to_owned()))?;
    let hint_number = lin_number
        .checked_add(1)
        .ok_or_else(|| Error::CannotSerialize("object number space is exhausted".to_owned()))?;
    let size = hint_number
        .checked_add(1)
        .ok_or_else(|| Error::CannotSerialize("trailer /Size exceeds u32".to_owned()))?;

    let mut first_section_ids = Vec::new();
    let mut main_section_ids = Vec::new();
    for (number, (id, _)) in &map {
        if first_numbers.contains(number) {
            first_section_ids.push(*id);
        } else {
            main_section_ids.push(*id);
        }
    }
    first_section_ids.sort_by_key(|id| id.number);
    main_section_ids.sort_by_key(|id| id.number);

    let object_counts: Vec<u32> = page_deps
        .iter()
        .map(|deps| {
            let count = u64::try_from(deps.len().saturating_add(1))
                .map_err(|_| Error::CannotSerialize("page object count exceeds u64".to_owned()))?;
            to_u32(count)
        })
        .collect::<Result<_>>()?;
    let least_objects_per_page = object_counts.iter().copied().min().unwrap_or(0);
    let greatest_objects_per_page = object_counts.iter().copied().max().unwrap_or(0);
    let least_content_length = page_content_lengths.iter().copied().min().unwrap_or(0);
    let greatest_content_length = page_content_lengths.iter().copied().max().unwrap_or(0);

    // Own the object bodies so assembly never needs the caller's slice again.
    let owned: HashMap<u32, (ObjectId, Object)> = objects
        .iter()
        .map(|(id, object)| (id.number, (*id, object.clone())))
        .collect();

    Ok(Plan {
        root,
        first_page_id: first_page,
        version: options.version.clone(),
        page_ids,
        shared_ids,
        first_section_ids,
        main_section_ids,
        page_content_lengths,
        shared_page_counts,
        shared_indices_per_page,
        least_objects_per_page,
        greatest_objects_per_page,
        least_content_length,
        greatest_content_length,
        lin_dict_id: ObjectId::new(lin_number, 0),
        hint_stream_id: ObjectId::new(hint_number, 0),
        size,
        objects: owned,
    })
}

/// Recursively walk a page tree, collecting leaf pages and tree node ids.
fn walk_pages(
    map: &HashMap<u32, (ObjectId, &Object)>,
    node: u32,
    visited: &mut BTreeSet<u32>,
    tree_nodes: &mut BTreeSet<u32>,
    pages: &mut Vec<u32>,
    depth: u8,
) -> Result<()> {
    if depth > MAX_DEPTH {
        return Err(Error::MalformedPageTree("page tree is too deep".to_owned()));
    }
    if !visited.insert(node) {
        return Err(Error::MalformedPageTree(
            "page tree contains a cycle".to_owned(),
        ));
    }
    let object = map
        .get(&node)
        .ok_or_else(|| Error::MalformedPageTree(format!("page tree node {node} is missing")))?
        .1;
    let dictionary = object.as_dictionary().ok_or_else(|| {
        Error::MalformedPageTree(format!("page tree node {node} is not a dictionary"))
    })?;
    let kind = dictionary
        .get("Type")
        .and_then(Object::as_name)
        .unwrap_or("");
    match kind {
        "Page" => pages.push(node),
        "Pages" | "" => {
            tree_nodes.insert(node);
            let kids = dictionary
                .get("Kids")
                .and_then(Object::as_array)
                .ok_or_else(|| {
                    Error::MalformedPageTree(format!("page tree node {node} has no /Kids array"))
                })?;
            for kid in kids {
                let id = kid.as_reference().ok_or_else(|| {
                    Error::MalformedPageTree(format!(
                        "page tree node {node} has a non-reference kid"
                    ))
                })?;
                walk_pages(map, id.number, visited, tree_nodes, pages, depth + 1)?;
            }
        }
        other => {
            return Err(Error::MalformedPageTree(format!(
                "unexpected page tree node type /{other}"
            )));
        }
    }
    Ok(())
}

/// Collect object numbers referenced anywhere inside `object`.
fn collect_references(object: &Object, out: &mut BTreeSet<u32>, depth: u8) {
    if depth > MAX_DEPTH {
        return;
    }
    match object {
        Object::Reference(id) => {
            out.insert(id.number);
        }
        Object::Array(items) => {
            for item in items {
                collect_references(item, out, depth + 1);
            }
        }
        Object::Dictionary(dictionary) | Object::Stream { dictionary, .. } => {
            for value in dictionary.values() {
                collect_references(value, out, depth + 1);
            }
        }
        _ => {}
    }
}

/// Total length of the content streams referenced by a page object.
fn page_content_length(page: &Object, map: &HashMap<u32, (ObjectId, &Object)>) -> u64 {
    let Some(dictionary) = page.as_dictionary() else {
        return 0;
    };
    let Some(contents) = dictionary.get("Contents") else {
        return 0;
    };
    let ids: Vec<u32> = match contents {
        Object::Reference(id) => vec![id.number],
        Object::Array(items) => items
            .iter()
            .filter_map(Object::as_reference)
            .map(|id| id.number)
            .collect(),
        _ => Vec::new(),
    };
    let mut total = 0u64;
    for id in ids {
        if let Some((_, Object::Stream { data, .. })) = map.get(&id) {
            total = total.saturating_add(data.len() as u64);
        }
    }
    total
}

/// Convert a `u64` count to `u32` with a structured error.
fn to_u32(value: u64) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| Error::CannotSerialize("linearization count exceeds u32".to_owned()))
}

/// Format a layout-dependent number as a fixed-width decimal string.
fn padded(value: u64) -> Result<String> {
    let text = format!("{value:0NUM_WIDTH$}");
    if text.len() > NUM_WIDTH {
        return Err(Error::CannotSerialize(
            "linearization value exceeds the fixed field width".to_owned(),
        ));
    }
    Ok(text)
}

/// Convert a `u64` offset to the u32 used by the hint tables.
fn offset_u32(value: u64) -> Result<u32> {
    u32::try_from(value)
        .map_err(|_| Error::CannotSerialize("linearized file exceeds 4 GiB".to_owned()))
}

/// Assemble the hint stream for a plan and a set of embedded offsets.
fn build_hint(plan: &Plan, embed: &EmbedOffsets) -> Result<HintStream> {
    let mut page_hints = Vec::with_capacity(plan.page_ids.len());
    for (index, number) in plan.page_ids.iter().enumerate() {
        let object_offset = *embed
            .page_offsets
            .get(index)
            .ok_or_else(|| Error::LinearizationFailed("missing embedded page offset".to_owned()))?;
        let indices = plan
            .shared_indices_per_page
            .get(index)
            .ok_or_else(|| Error::LinearizationFailed("missing shared index list".to_owned()))?;
        page_hints.push(PageHint {
            page_object: *number,
            object_offset: offset_u32(object_offset)?,
            content_length: *plan
                .page_content_lengths
                .get(index)
                .ok_or_else(|| Error::LinearizationFailed("missing content length".to_owned()))?,
            shared_count: u32::try_from(indices.len()).map_err(|_| {
                Error::LinearizationFailed("shared index count exceeds u32".to_owned())
            })?,
            shared_indices: indices.clone(),
        });
    }

    let mut shared_hints = Vec::with_capacity(plan.shared_ids.len());
    for (index, id) in plan.shared_ids.iter().enumerate() {
        let object_offset = *embed.shared_offsets.get(index).ok_or_else(|| {
            Error::LinearizationFailed("missing embedded shared offset".to_owned())
        })?;
        shared_hints.push(SharedObjectHint {
            object_number: id.number,
            object_offset: offset_u32(object_offset)?,
            page_reference_count: *plan.shared_page_counts.get(index).ok_or_else(|| {
                Error::LinearizationFailed("missing shared page count".to_owned())
            })?,
        });
    }

    Ok(HintStream {
        page_count: u32::try_from(plan.page_ids.len())
            .map_err(|_| Error::LinearizationFailed("page count exceeds u32".to_owned()))?,
        least_objects_per_page: plan.least_objects_per_page,
        greatest_objects_per_page: plan.greatest_objects_per_page,
        least_content_length: plan.least_content_length,
        greatest_content_length: plan.greatest_content_length,
        first_page_content_length: plan
            .page_content_lengths
            .first()
            .copied()
            .ok_or_else(|| Error::LinearizationFailed("no first page".to_owned()))?,
        first_page_offset: offset_u32(embed.first_page_offset)?,
        page_hints,
        first_shared_object_offset: offset_u32(embed.first_shared_offset)?,
        shared_hints,
    })
}

/// Encode a [`HintStream`] as the fixed-width binary payload.
fn encode_hint_stream(hint: &HintStream) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, hint.page_count);
    push_u32(&mut out, hint.least_objects_per_page);
    push_u32(&mut out, hint.greatest_objects_per_page);
    push_u32(&mut out, hint.least_content_length);
    push_u32(&mut out, hint.greatest_content_length);
    push_u32(&mut out, hint.first_page_content_length);
    push_u32(&mut out, hint.first_page_offset);
    for page in &hint.page_hints {
        push_u32(&mut out, page.page_object);
        push_u32(&mut out, page.object_offset);
        push_u32(&mut out, page.content_length);
        push_u32(&mut out, page.shared_count);
        for index in &page.shared_indices {
            push_u32(&mut out, *index);
        }
    }
    push_u32(
        &mut out,
        u32::try_from(hint.shared_hints.len()).unwrap_or(u32::MAX),
    );
    push_u32(&mut out, hint.first_shared_object_offset);
    for shared in &hint.shared_hints {
        push_u32(&mut out, shared.object_number);
        push_u32(&mut out, shared.object_offset);
        push_u32(&mut out, shared.page_reference_count);
    }
    out
}

/// Length of just the page-offset hint table.
fn page_table_length(hint: &HintStream) -> usize {
    7 * 4
        + hint
            .page_hints
            .iter()
            .map(|page| 4 * 4 + page.shared_indices.len() * 4)
            .sum::<usize>()
}

/// Append a big-endian u32.
fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

/// Write one assembly of the complete document, returning bytes and offsets.
fn assemble(plan: &Plan, numbers: &Numbers, embed: &EmbedOffsets) -> Result<Assembled> {
    let mut out = Vec::new();
    out.extend_from_slice(format!("%PDF-{}\n%PDFXI\n", plan.version).as_bytes());

    // Linearization dictionary: the first object in the file.
    let mut object_offsets = BTreeMap::new();
    let lin_offset = out.len() as u64;
    out.extend_from_slice(format!("{} 0 obj\n", plan.lin_dict_id.number).as_bytes());
    write_linearization_dict(plan, numbers, &mut out)?;
    out.extend_from_slice(b"\nendobj\n");
    object_offsets.insert(plan.lin_dict_id.number, lin_offset);

    // Hint stream: the second object. Writing it by hand lets us record the
    // exact data offset referenced by /H.
    let hint = build_hint(plan, embed)?;
    let encoded = encode_hint_stream(&hint);
    let page_table_len = page_table_length(&hint);
    if page_table_len > encoded.len() {
        return Err(Error::LinearizationFailed(
            "page hint table is longer than the hint stream".to_owned(),
        ));
    }
    let shared_table_len = (encoded.len() - page_table_len) as u64;
    let shared_table_offset_in_data = u64::try_from(page_table_len)
        .map_err(|_| Error::CannotSerialize("hint table offset exceeds u64".to_owned()))?;

    let hint_obj_offset = out.len() as u64;
    let mut hint_dictionary = Dictionary::new();
    hint_dictionary.insert(
        "Length".to_owned(),
        Object::Integer(
            i64::try_from(encoded.len())
                .map_err(|_| Error::CannotSerialize("hint stream length exceeds i64".to_owned()))?,
        ),
    );
    let hint_data_offset =
        write_stream_object(plan.hint_stream_id, &hint_dictionary, &encoded, &mut out)?;
    object_offsets.insert(plan.hint_stream_id.number, hint_obj_offset);

    // First-page section objects.
    for id in &plan.first_section_ids {
        let object = plan
            .objects
            .get(&id.number)
            .map(|(_, object)| object)
            .ok_or_else(|| {
                Error::LinearizationFailed("first-page object missing from plan".to_owned())
            })?;
        let offset = out.len() as u64;
        writer::serialize_indirect(*id, object, &mut out)?;
        out.push(b'\n');
        object_offsets.insert(id.number, offset);
    }

    // First-page xref and its trailer (no startxref yet).
    let first_xref_offset = out.len() as u64;
    let first_xref = section_offsets(&object_offsets);
    writer::write_xref_section(&mut out, &first_xref, true);
    write_trailer_dict(&base_trailer(plan), &mut out)?;

    // Main section objects.
    for id in &plan.main_section_ids {
        let object = plan
            .objects
            .get(&id.number)
            .map(|(_, object)| object)
            .ok_or_else(|| {
                Error::LinearizationFailed("main object missing from plan".to_owned())
            })?;
        let offset = out.len() as u64;
        writer::serialize_indirect(*id, object, &mut out)?;
        out.push(b'\n');
        object_offsets.insert(id.number, offset);
    }

    // Main xref, trailer with /Prev, and the single final startxref / EOF.
    let main_xref_offset = out.len() as u64;
    let main_xref: BTreeMap<u32, usize> = plan
        .main_section_ids
        .iter()
        .filter_map(|id| {
            object_offsets
                .get(&id.number)
                .and_then(|offset| usize::try_from(*offset).ok())
                .map(|offset| (id.number, offset))
        })
        .collect();
    writer::write_xref_section(&mut out, &main_xref, true);
    let mut trailer = base_trailer(plan);
    trailer.insert(
        "Prev".to_owned(),
        Object::Integer(
            i64::try_from(first_xref_offset)
                .map_err(|_| Error::CannotSerialize("first xref offset exceeds i64".to_owned()))?,
        ),
    );
    writer::write_trailer(
        &mut out,
        &trailer,
        usize::try_from(main_xref_offset)
            .map_err(|_| Error::CannotSerialize("main xref offset exceeds usize".to_owned()))?,
    )?;

    let file_length = u64::try_from(out.len())
        .map_err(|_| Error::CannotSerialize("linearized file length exceeds u64".to_owned()))?;
    Ok(Assembled {
        bytes: out,
        layout: Layout {
            object_offsets,
            hint_data_offset,
            page_table_len: page_table_len as u64,
            shared_table_offset: hint_data_offset
                .checked_add(shared_table_offset_in_data)
                .ok_or_else(|| Error::CannotSerialize("hint offset overflow".to_owned()))?,
            shared_table_len,
            first_xref_offset,
            main_xref_offset,
            file_length,
        },
    })
}

/// Write the linearization parameter dictionary with fixed-width numbers.
fn write_linearization_dict(plan: &Plan, numbers: &Numbers, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<< /Linearized 1");
    out.extend_from_slice(format!(" /L {}", padded(numbers.file_length)?).as_bytes());
    out.extend_from_slice(b" /H [");
    out.extend_from_slice(format!(" {}", padded(numbers.hint_offset)?).as_bytes());
    out.extend_from_slice(format!(" {}", padded(numbers.hint_length)?).as_bytes());
    match numbers.shared_hint_offset {
        Some(offset) => {
            out.extend_from_slice(format!(" {}", padded(offset)?).as_bytes());
            out.extend_from_slice(format!(" {}", padded(numbers.shared_hint_length)?).as_bytes());
        }
        None => out.extend_from_slice(b" -1 0"),
    }
    out.extend_from_slice(b" ]");
    out.extend_from_slice(format!(" /O {}", plan.first_page_id.number).as_bytes());
    out.extend_from_slice(format!(" /E {}", padded(numbers.first_page_end)?).as_bytes());
    out.extend_from_slice(format!(" /N {}", plan.page_ids.len()).as_bytes());
    out.extend_from_slice(format!(" /T {}", padded(numbers.main_xref_offset)?).as_bytes());
    out.extend_from_slice(format!(" /V {}", plan.version).as_bytes());
    out.extend_from_slice(b" >>");
    Ok(())
}

/// Write just a `trailer` dictionary without `startxref` or `%%EOF`.
fn write_trailer_dict(trailer: &Dictionary, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"trailer\n");
    writer::serialize_object(&Object::Dictionary(trailer.clone()), out)?;
    out.push(b'\n');
    Ok(())
}

/// Build the common trailer dictionary for both xref sections.
fn base_trailer(plan: &Plan) -> Dictionary {
    let mut trailer = Dictionary::new();
    trailer.insert("Size".to_owned(), Object::Integer(i64::from(plan.size)));
    trailer.insert("Root".to_owned(), Object::Reference(plan.root));
    trailer
}

/// All object offsets as usize for a classic xref table.
fn section_offsets(offsets: &BTreeMap<u32, u64>) -> BTreeMap<u32, usize> {
    offsets
        .iter()
        .filter_map(|(number, offset)| {
            usize::try_from(*offset)
                .ok()
                .map(|offset| (*number, offset))
        })
        .collect()
}

/// Write a stream object and return the absolute offset of its data bytes.
fn write_stream_object(
    id: ObjectId,
    dictionary: &Dictionary,
    data: &[u8],
    out: &mut Vec<u8>,
) -> Result<u64> {
    out.extend_from_slice(format!("{} {} obj\n", id.number, id.generation).as_bytes());
    writer::serialize_object(&Object::Dictionary(dictionary.clone()), out)?;
    out.extend_from_slice(b"\nstream\n");
    let data_offset = u64::try_from(out.len())
        .map_err(|_| Error::CannotSerialize("stream data offset exceeds u64".to_owned()))?;
    out.extend_from_slice(data);
    out.extend_from_slice(b"\nendstream\nendobj\n");
    Ok(data_offset)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::linearization::parse_linearization;

    /// Gather the object set, root and first page of a loaded fixture.
    fn gather(document: &crate::Document) -> (Vec<(ObjectId, Object)>, ObjectId, ObjectId) {
        let structure = document
            .structure
            .as_ref()
            .expect("fixture has a structure");
        let mut objects = Vec::new();
        for number in structure.xref.all_object_numbers() {
            if number == 0 {
                continue;
            }
            let generation =
                crate::parser::object_generation(document.bytes(), &structure.xref, number)
                    .expect("object generation");
            let id = ObjectId::new(number, generation);
            let object = crate::parser::load_object(document.bytes(), &structure.xref, id)
                .expect("load object");
            objects.push((id, object));
        }
        objects.sort_by_key(|(id, _)| id.number);
        let first = structure.pages.first().expect("fixture has a page").id;
        (objects, structure.root_id, first)
    }

    /// Linearize a fixture PDF and return the bytes and measured layout.
    fn linearize(source: Vec<u8>) -> (Vec<u8>, LinearizedLayout) {
        let document = crate::Document::from_bytes(source).expect("fixture parses");
        let (objects, root, first) = gather(&document);
        write_linearized_document_with_layout(
            &objects,
            root,
            first,
            &LinearizationOptions::default(),
        )
        .expect("linearized")
    }

    /// Slice out and decode the hint stream using the measured layout.
    fn hint_from(bytes: &[u8], layout: &LinearizedLayout) -> HintStream {
        let start = usize::try_from(layout.hint_offset).expect("hint offset fits usize");
        let end = start + usize::try_from(layout.hint_length).expect("hint length fits usize");
        parse_hint_stream(&bytes[start..end]).expect("hint stream parses")
    }

    /// Read and validate the linearization dictionary from produced bytes.
    fn lin_from(bytes: &[u8]) -> crate::Linearization {
        parse_linearization(bytes)
            .expect("dictionary parses")
            .expect("dictionary present")
    }

    #[test]
    fn first_page_objects_are_placed_before_the_rest() {
        let source = pdfxi_testkit::multipage_pdf(3);
        let document = crate::Document::from_bytes(source.clone()).expect("fixture");
        let page_ids: Vec<u32> = document
            .pages()
            .expect("pages")
            .iter()
            .map(|page| page.id.number)
            .collect();
        let (bytes, layout) = linearize(source);

        assert_eq!(&bytes[..5], b"%PDF-");
        let first_xref = usize::try_from(layout.first_xref_offset).expect("usize");
        let main_xref = usize::try_from(layout.main_xref_offset).expect("usize");
        for number in &layout.first_section_objects {
            let offset = usize::try_from(layout.object_offsets[number]).expect("usize");
            assert!(offset < first_xref, "first-section object {number} is late");
        }
        for number in &layout.main_section_objects {
            let offset = usize::try_from(layout.object_offsets[number]).expect("usize");
            assert!(
                offset > first_xref && offset < main_xref,
                "main object {number} is misplaced"
            );
        }
        assert!(layout.first_section_objects.contains(&page_ids[0]));
        assert!(layout.main_section_objects.contains(&page_ids[1]));
        assert!(
            layout.object_offsets[&page_ids[0]] < layout.object_offsets[&page_ids[1]],
            "page 1 must precede page 2"
        );
    }

    #[test]
    fn declared_length_matches_actual_file_size() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let lin = lin_from(&bytes);
        assert_eq!(lin.file_length, bytes.len() as u64);
        assert_eq!(layout.file_length, bytes.len() as u64);
    }

    #[test]
    fn hint_ranges_match_the_emitted_layout() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let lin = lin_from(&bytes);
        assert_eq!(lin.hint.offset, layout.hint_offset);
        assert_eq!(lin.hint.length, layout.hint_length);
        let shared = lin.shared_hint.expect("shared hint is present");
        assert_eq!(shared.offset, layout.shared_hint_offset);
        assert_eq!(shared.length, layout.shared_hint_length);
        assert_eq!(
            layout.shared_hint_offset - layout.hint_offset + layout.shared_hint_length,
            layout.hint_length
        );
        assert_eq!(hint_from(&bytes, &layout).page_count, 3);
    }

    #[test]
    fn page_offset_hints_agree_with_actual_offsets() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let hint = hint_from(&bytes, &layout);
        assert_eq!(hint.page_hints.len(), 3);
        for page in &hint.page_hints {
            let actual = layout
                .object_offsets
                .get(&page.page_object)
                .expect("page object was written");
            assert_eq!(u64::from(page.object_offset), *actual);
            assert!(page.content_length > 0, "page has content");
        }
        assert_eq!(
            u64::from(hint.first_page_offset),
            layout.object_offsets[&layout.first_page_object]
        );
        assert_eq!(
            hint.first_page_content_length,
            hint.page_hints[0].content_length
        );
    }

    #[test]
    fn shared_object_hints_agree_with_actual_offsets() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let hint = hint_from(&bytes, &layout);
        assert!(!hint.shared_hints.is_empty());
        for shared in &hint.shared_hints {
            assert_eq!(
                u64::from(shared.object_offset),
                layout.object_offsets[&shared.object_number]
            );
            assert!(layout.first_section_objects.contains(&shared.object_number));
            assert!(shared.page_reference_count <= layout.page_count);
        }
        assert!(
            hint.shared_hints
                .iter()
                .any(|shared| shared.page_reference_count >= 2)
        );
        assert_eq!(
            u64::from(hint.first_shared_object_offset),
            u64::from(hint.shared_hints[0].object_offset)
        );
    }

    #[test]
    fn end_offset_marks_the_first_xref_table() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(2));
        let lin = lin_from(&bytes);
        assert_eq!(lin.first_page_end, Some(layout.first_xref_offset));
        let at = usize::try_from(layout.first_xref_offset).expect("usize");
        assert_eq!(&bytes[at..at + 4], b"xref");
    }

    #[test]
    fn page_count_first_page_and_version_are_correct() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(4));
        let lin = lin_from(&bytes);
        assert_eq!(lin.page_count, Some(4));
        assert_eq!(lin.first_page_object, layout.first_page_object);
        assert_eq!(lin.version, Some(1.7));
        assert_eq!(layout.page_count, 4);
        assert_eq!(layout.size, layout.hint_stream_id + 1);
    }

    #[test]
    fn first_page_xref_and_main_xref_link_are_valid() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let text = String::from_utf8_lossy(&bytes).into_owned();

        let first = usize::try_from(layout.first_xref_offset).expect("usize");
        assert_eq!(&bytes[first..first + 4], b"xref");
        assert!(
            text.contains("0000000000 65535 f"),
            "free-list head present"
        );
        assert!(
            text.contains(&format!("/Prev {}", layout.first_xref_offset)),
            "main xref must link to the first-page xref"
        );
        assert!(text.contains("/Root "), "both trailers carry /Root");

        // The final startxref points at the main xref table.
        let marker = "startxref\n";
        let at = text.rfind(marker).expect("startxref marker") + marker.len();
        let value: usize = text[at..]
            .lines()
            .next()
            .expect("offset line")
            .parse()
            .expect("numeric offset");
        assert_eq!(value as u64, layout.main_xref_offset);
        let main = usize::try_from(layout.main_xref_offset).expect("usize");
        assert_eq!(&bytes[main..main + 4], b"xref");
        assert!(bytes.ends_with(b"%%EOF\n"));

        // Generated objects resolve through the merged xref.
        let document = crate::Document::from_bytes(bytes).expect("reparse");
        let lin_dict = document
            .object(ObjectId::new(layout.lin_dict_id, 0))
            .expect("linearization dictionary resolves");
        assert!(
            lin_dict
                .as_dictionary()
                .is_some_and(|dict| dict.contains_key("Linearized"))
        );
        let hint_object = document
            .object(ObjectId::new(layout.hint_stream_id, 0))
            .expect("hint stream resolves");
        assert!(matches!(hint_object, Object::Stream { .. }));
    }

    #[test]
    fn full_reparse_and_resave_preserve_every_page() {
        let source = pdfxi_testkit::multipage_pdf(3);
        let original = crate::Document::from_bytes(source.clone()).expect("source");
        let expected_text: Vec<String> = (0..3)
            .map(|index| original.page_text(index).expect("source text"))
            .collect();

        let (bytes, _) = linearize(source);
        let document = crate::Document::from_bytes(bytes.clone()).expect("linearized reparses");
        assert_eq!(document.page_count().expect("page count"), 3);
        assert_eq!(
            document
                .linearization()
                .expect("hint parses")
                .expect("hint present")
                .page_count,
            Some(3)
        );
        for (index, text) in expected_text.iter().enumerate() {
            assert_eq!(&document.page_text(index).expect("page text"), text);
        }

        let saved = document
            .save_linearized(&LinearizationOptions::default())
            .expect("re-save");
        let again = crate::Document::from_bytes(saved).expect("saved file reparses");
        assert_eq!(again.page_count().expect("page count"), 3);
        for (index, text) in expected_text.iter().enumerate() {
            assert_eq!(&again.page_text(index).expect("page text"), text);
        }
    }

    #[test]
    fn malformed_inputs_are_rejected() {
        let options = LinearizationOptions::default();
        assert!(matches!(
            write_linearized_document(&[], ObjectId::new(1, 0), ObjectId::new(3, 0), &options),
            Err(Error::LinearizationFailed(_))
        ));

        let document = crate::Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("fixture");
        let (objects, root, first) = gather(&document);
        assert!(matches!(
            write_linearized_document(&objects, ObjectId::new(999, 0), first, &options),
            Err(Error::MissingRoot)
        ));
        assert!(matches!(
            write_linearized_document(&objects, root, ObjectId::new(999, 0), &options),
            Err(Error::LinearizationFailed(_))
        ));
        let bad_version = LinearizationOptions {
            version: "x.y".to_owned(),
        };
        assert!(matches!(
            write_linearized_document(&objects, root, first, &bad_version),
            Err(Error::LinearizationFailed(_))
        ));

        let (bytes, layout) = linearize(pdfxi_testkit::minimal_pdf());
        let start = usize::try_from(layout.hint_offset).expect("usize");
        assert!(matches!(
            parse_hint_stream(&bytes[start..start + 5]),
            Err(Error::MalformedHintStream(_))
        ));
        assert!(parse_hint_stream(&[]).is_err());
    }

    #[test]
    fn output_is_deterministic() {
        let first = linearize(pdfxi_testkit::multipage_pdf(3)).0;
        let second = linearize(pdfxi_testkit::multipage_pdf(3)).0;
        assert_eq!(first, second);
    }

    #[test]
    fn large_document_is_fully_linearized() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(8));
        let lin = lin_from(&bytes);
        assert_eq!(lin.page_count, Some(8));
        assert_eq!(lin.file_length, bytes.len() as u64);
        assert!(
            layout.object_offsets[&layout.first_page_object] < layout.first_xref_offset,
            "first page stays in the first-page section"
        );
        assert!(
            String::from_utf8_lossy(&bytes).contains("/Linearized 1"),
            "linearization dictionary is present"
        );
    }

    #[test]
    fn hint_stream_round_trips_through_the_parser() {
        let (bytes, layout) = linearize(pdfxi_testkit::multipage_pdf(3));
        let hint = hint_from(&bytes, &layout);
        let reencoded = encode_hint_stream(&hint);
        assert_eq!(reencoded.len(), layout.hint_length as usize);
        assert_eq!(parse_hint_stream(&reencoded).expect("reparse"), hint);
    }

    #[test]
    fn repair_path_accepts_a_linearized_document() {
        let (bytes, _) = linearize(pdfxi_testkit::multipage_pdf(3));
        let repaired =
            crate::repair::repair_document(&bytes).expect("linearized file is repairable");
        let document =
            crate::Document::from_bytes(repaired.bytes().to_vec()).expect("repaired file reparses");
        assert_eq!(document.page_count().expect("page count"), 3);
        for index in 0..3 {
            assert_eq!(
                document.page_text(index).expect("repaired page text"),
                format!("Page {} of 3", index + 1)
            );
        }
    }
}
