//! Cross-reference and page-tree parsing.
//!
//! This module implements both cross-reference formats defined by ISO 32000:
//! the classic table-based form (including chained `/Prev` sections produced
//! by incremental updates) and the stream-based form introduced in PDF 1.5.
//! Xref streams encode their entries as fixed-width big-endian fields inside a
//! Flate-compressed stream; object streams (`/ObjStm`) hold several indirect
//! objects in one compressed stream and are referenced by type-2 xref entries.
//!
//! Both formats are merged into one [`XrefTable`], so the rest of the crate
//! resolves object references through a single code path. Every read is
//! bounds-checked, widths and counts are validated against hostile values,
//! `/Prev` chains are cycle-detected, and no parser path can panic on input.

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crate::filter::{decode_stream, filters_from_dictionary};
use crate::lexer::Lexer;
use crate::object::{Dictionary, Object, ObjectId};
use crate::page::{Page, Rectangle};
use crate::{Error, Result};

/// Maximum number of indirect references followed for a single value.
const MAX_REFERENCE_DEPTH: usize = 32;
/// Maximum number of leaf pages accepted before treating the tree as hostile.
const MAX_PAGES: usize = 1_000_000;
/// Upper bound on page-tree nesting. Real documents nest a handful of levels;
/// a deeper chain is malformed and would otherwise recurse until the stack
/// overflows (an abort the panic hook cannot catch).
const MAX_PAGE_TREE_DEPTH: usize = 64;
/// Maximum number of chained `/Prev` xref sections.
const MAX_XREF_SECTIONS: usize = 64;
/// Maximum number of entries in one classic xref subsection, relative to file size.
const XREF_ENTRY_MIN_BYTES: usize = 4;
/// Largest byte width accepted for a single `/W` field of an xref stream.
///
/// PDF field values are at most 64 bits, so any width above eight is hostile.
const MAX_XREF_FIELD_WIDTH: u8 = 8;
/// Largest combined width of an xref stream's three `/W` fields.
///
/// Eight bytes per field would allow a 24-byte row; real files use at most
/// `[1 4 2]`. Sixteen bytes is a generous but still bounded ceiling that also
/// rejects a hostile row built from several wide fields.
const MAX_XREF_ROW_WIDTH: usize = 16;

/// The structural kind of one cross-reference section.
///
/// Hybrid files contain both kinds at the same revision: a classic table for
/// readers that cannot handle xref streams and a `/Type /XRef` stream for
/// readers that can.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum XrefSectionKind {
    /// A classic `xref` table followed by a `trailer` dictionary.
    Classic,
    /// A `/Type /XRef` stream object (PDF 1.5 and later).
    Stream,
}

/// A typed description of one cross-reference section.
///
/// Sections are reported in the order they were visited: newest first, then
/// older sections through their `/Prev` pointers. A hybrid file contributes two
/// adjacent reports for one revision: the `/XRefStm` stream first, followed by
/// the classic table that references it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrefSectionReport {
    /// Whether this section is a classic table or an xref stream.
    pub kind: XrefSectionKind,
    /// Byte offset in the source file where this section begins.
    pub offset: usize,
    /// Number of object entries declared by this section.
    pub entries: usize,
    /// Byte offset of the next older section from `/Prev`, when present.
    pub previous: Option<usize>,
}

/// A typed, deterministic report of a document's cross-reference structure.
///
/// The report is derived solely from the source bytes and does not depend on
/// page-tree resolution, so it is available even when `/Root` or `/Pages` is
/// damaged. Entry counts are per section; [`Self::merged_entries`] is the size
/// of the newest-wins map actually used to resolve objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XrefReport {
    /// Every visited section, newest first.
    pub sections: Vec<XrefSectionReport>,
    /// Number of distinct object numbers in the merged cross-reference table.
    pub merged_entries: usize,
    /// `true` when any visited section (including a hybrid `/XRefStm`) was a
    /// stream. Write paths use this to decline xref-stream sources explicitly.
    pub uses_xref_stream: bool,
    /// Byte offset read from the final `startxref` marker.
    pub startxref: usize,
}
/// Upper bound on entries declared by a single xref stream or subsection.
///
/// This is independent of the stream-length check: it keeps a hostile `/Index`
/// from forcing a large `HashMap` allocation even when the width is zero.
const MAX_XREF_ENTRIES: u32 = 16_000_000;
/// Upper bound on the number of subsections declared by `/Index`.
const MAX_XREF_SUBSECTIONS: usize = 4096;
/// Upper bound on objects packed into a single `/ObjStm`.
const MAX_OBJECT_STREAM_OBJECTS: u32 = 1_000_000;
/// Upper bound on nested object-stream resolution (object streams are not
/// allowed to contain other object streams by specification; the guard stops
/// a malformed file from recursing indefinitely).
const MAX_OBJECT_STREAM_DEPTH: u8 = 8;

/// One entry of a cross-reference table.
#[derive(Debug, Clone)]
enum XrefEntry {
    /// The object number exists but the object is free (or a null placeholder).
    Free,
    /// The object is in use at the given byte offset.
    InUse {
        /// Byte offset of the indirect object definition.
        offset: usize,
    },
    /// The object lives inside an object stream (a type-2 xref entry).
    Compressed {
        /// Object number of the `/ObjStm` that contains the object.
        objstm: u32,
        /// Zero-based index of the object inside that stream's header.
        index: u32,
    },
}

/// A single cross-reference section (classic table or xref stream) and its
/// trailer dictionary.
#[derive(Debug, Clone)]
struct XrefSection {
    entries: HashMap<u32, XrefEntry>,
    trailer: Dictionary,
    prev: Option<usize>,
    kind: XrefSectionKind,
    offset: usize,
}

/// The merged cross-reference table for a document.
#[derive(Debug, Clone)]
pub(crate) struct XrefTable {
    entries: HashMap<u32, XrefEntry>,
    trailer: Dictionary,
    /// Typed structural report captured while the sections were parsed.
    pub(crate) report: XrefReport,
}

impl XrefTable {
    /// Return `(object number, byte offset)` for every in-use object, sorted by
    /// object number. Free entries and objects stored in object streams are
    /// omitted; the latter have no standalone byte offset.
    pub(crate) fn in_use_entries(&self) -> Vec<(u32, usize)> {
        let mut entries: Vec<(u32, usize)> = self
            .entries
            .iter()
            .filter_map(|(number, entry)| match entry {
                XrefEntry::InUse { offset } => Some((*number, *offset)),
                XrefEntry::Free | XrefEntry::Compressed { .. } => None,
            })
            .collect();
        entries.sort_unstable_by_key(|(number, _)| *number);
        entries
    }

    /// Return every object number known to the merged table, sorted ascending.
    ///
    /// The result includes free and compressed objects; callers decide which
    /// entries to load. Object number zero is present when the source declared
    /// a free-list head.
    pub(crate) fn all_object_numbers(&self) -> Vec<u32> {
        let mut numbers: Vec<u32> = self.entries.keys().copied().collect();
        numbers.sort_unstable();
        numbers
    }

    /// Return every object number that is in use or stored in an `/ObjStm`.
    ///
    /// Free entries are omitted. Unlike [`Self::in_use_entries`], compressed
    /// objects are included: a full rewrite resolves them through
    /// [`load_object`] and emits them as standalone indirect objects, so
    /// dropping them would lose part of the document.
    pub(crate) fn live_object_numbers(&self) -> Vec<u32> {
        let mut numbers: Vec<u32> = self
            .entries
            .iter()
            .filter_map(|(number, entry)| match entry {
                XrefEntry::Free => None,
                XrefEntry::InUse { .. } | XrefEntry::Compressed { .. } => Some(*number),
            })
            .collect();
        numbers.sort_unstable();
        numbers
    }

    /// Return a clone of the merged trailer dictionary (newest keys win).
    pub(crate) fn trailer(&self) -> Dictionary {
        self.trailer.clone()
    }
}

/// Read the generation number of object `number` from its indirect-object
/// header, returning zero for compressed/free objects and unknown numbers.
pub(crate) fn object_generation(data: &[u8], xref: &XrefTable, number: u32) -> Result<u16> {
    match xref.entries.get(&number) {
        Some(XrefEntry::InUse { offset }) => {
            let mut lexer = Lexer::new(data, *offset)?;
            lexer.skip_ws();
            let header_number = lexer.read_regular().ok_or_else(|| Error::MalformedObject {
                number,
                reason: "missing object number in header".to_owned(),
            })?;
            if parse_decimal(header_number, "object header number")?
                != usize::try_from(number).unwrap_or(usize::MAX)
            {
                return Err(Error::MalformedObject {
                    number,
                    reason: "object header number does not match xref".to_owned(),
                });
            }
            lexer.skip_ws();
            let generation = lexer.read_regular().ok_or_else(|| Error::MalformedObject {
                number,
                reason: "missing generation number in header".to_owned(),
            })?;
            let value = parse_decimal(generation, "object header generation")?;
            u16::try_from(value).map_err(|_| Error::MalformedObject {
                number,
                reason: "generation exceeds 65535".to_owned(),
            })
        }
        Some(XrefEntry::Compressed { .. }) | Some(XrefEntry::Free) | None => Ok(0),
    }
}

/// Return `true` when `object` is a cross-reference or object stream.
///
/// Neither kind survives a full rewrite: their contents are expanded into
/// standalone indirect objects and their xref section is regenerated as a
/// classic table.
pub(crate) fn is_structural_stream(object: &Object) -> bool {
    let Object::Stream { dictionary, .. } = object else {
        return false;
    };
    matches!(
        dictionary.get("Type").and_then(Object::as_name),
        Some("XRef" | "ObjStm")
    )
}

/// Fully resolved structural information about a document.
#[derive(Debug, Clone)]
pub(crate) struct Structure {
    pub(crate) root_id: ObjectId,
    pub(crate) trailer_size: Option<u32>,
    pub(crate) declared_page_count: Option<u32>,
    pub(crate) pages: Vec<Page>,
    pub(crate) xref: XrefTable,
}

/// Parse the structural layer of a PDF: xref, trailer, catalog and page tree.
pub(crate) fn parse_document(data: &[u8]) -> Result<Structure> {
    let start = find_startxref(data)?;
    let xref = load_xref(data, start)?;

    let root_id = match xref.trailer.get("Root") {
        Some(Object::Reference(id)) => *id,
        _ => return Err(Error::MissingRoot),
    };
    let trailer_size = non_negative_u32(xref.trailer.get("Size"));

    let catalog = resolve_id(data, &xref, root_id)?;
    let catalog_dict = as_dictionary(&catalog, "document catalog")?;
    let pages_id = match catalog_dict.get("Pages") {
        Some(Object::Reference(id)) => *id,
        _ => return Err(Error::MissingPages),
    };

    let pages_root = resolve_id(data, &xref, pages_id)?;
    let pages_dict = as_dictionary(&pages_root, "page tree root")?;
    let declared_page_count = non_negative_u32(pages_dict.get("Count"));

    let mut walker = PageTreeWalker {
        data,
        xref: &xref,
        visited: HashSet::new(),
        pages: Vec::new(),
    };
    walker.walk(pages_id, InheritedPage::default(), 0)?;
    let pages = walker.pages;

    Ok(Structure {
        root_id,
        trailer_size,
        declared_page_count,
        pages,
        xref,
    })
}

/// Return the concatenated, decoded content bytes for `page`.
///
/// Each `/Contents` reference is loaded through the cross-reference table and
/// must resolve to a stream object. The stream dictionary is converted into a
/// filter chain by [`crate::filter::filters_from_dictionary`] and decoded with
/// [`crate::filter::decode_stream`]. `FlateDecode` is handled transparently;
/// an unsupported filter is reported as [`Error::FilteredContent`] with the
/// filter's name so callers never receive encoded bytes by mistake.
pub(crate) fn page_content(data: &[u8], structure: &Structure, page: &Page) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for id in &page.contents {
        let object = load_object(data, &structure.xref, *id)?;
        match object {
            Object::Stream { dictionary, data } => {
                let filters = crate::filter::filters_from_dictionary(&dictionary)?;
                let decoded =
                    crate::filter::decode_stream(&data, &filters).map_err(|error| match error {
                        Error::UnsupportedFilter(filter) => {
                            Error::FilteredContent { id: *id, filter }
                        }
                        other => other,
                    })?;
                if !out.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(&decoded);
            }
            _ => return Err(Error::ContentNotStream(*id)),
        }
    }
    Ok(out)
}

/// Locate the final `startxref` marker and read its byte offset.
fn find_startxref(data: &[u8]) -> Result<usize> {
    let marker_pos = data
        .windows(9)
        .rposition(|window| window == b"startxref")
        .ok_or(Error::MissingStartXref)?;
    let mut lexer = Lexer::new(data, marker_pos + 9)?;
    lexer.skip_ws();
    let token = lexer
        .read_regular()
        .ok_or_else(|| Error::InvalidStartXref("no offset after startxref".to_owned()))?;
    let text = std::str::from_utf8(token)
        .map_err(|_| Error::InvalidStartXref("non-UTF-8 offset".to_owned()))?;
    let offset = text
        .parse::<usize>()
        .map_err(|_| Error::InvalidStartXref(text.to_owned()))?;
    if offset >= data.len() {
        return Err(Error::StartXrefOutOfBounds {
            offset,
            len: data.len(),
        });
    }
    Ok(offset)
}

/// Parse one classic xref section at `offset`.
fn parse_xref_section(data: &[u8], offset: usize) -> Result<XrefSection> {
    let mut lexer = Lexer::new(data, offset)?;
    lexer
        .expect_keyword(b"xref")
        .map_err(|_| Error::MissingXref(offset))?;

    let mut entries = HashMap::new();
    loop {
        lexer.skip_ws();
        let first_token = lexer
            .read_regular()
            .ok_or_else(|| Error::MalformedXref("unexpected end before trailer".to_owned()))?;
        if first_token == b"trailer" {
            break;
        }
        let first = parse_decimal(first_token, "xref subsection start")?;
        lexer.skip_ws();
        let count_token = lexer
            .read_regular()
            .ok_or_else(|| Error::MalformedXref("missing subsection count".to_owned()))?;
        let count = parse_decimal(count_token, "xref subsection count")?;
        if count > data.len() / XREF_ENTRY_MIN_BYTES {
            return Err(Error::MalformedXref(
                "xref subsection count exceeds file size".to_owned(),
            ));
        }

        for index in 0..count {
            let object_number = first
                .checked_add(index)
                .and_then(|value| u32::try_from(value).ok())
                .ok_or_else(|| Error::MalformedXref("object number overflow".to_owned()))?;

            lexer.skip_ws();
            let offset_token = lexer
                .read_regular()
                .ok_or_else(|| Error::MalformedXref("missing xref entry offset".to_owned()))?;
            let object_offset = parse_decimal(offset_token, "xref entry offset")?;

            lexer.skip_ws();
            let generation_token = lexer
                .read_regular()
                .ok_or_else(|| Error::MalformedXref("missing xref generation".to_owned()))?;
            let generation = parse_decimal(generation_token, "xref generation")?;
            if generation > usize::from(u16::MAX) {
                return Err(Error::MalformedXref(
                    "xref generation exceeds 65535".to_owned(),
                ));
            }

            lexer.skip_ws();
            let kind_token = lexer
                .read_regular()
                .ok_or_else(|| Error::MalformedXref("missing xref entry type".to_owned()))?;
            let entry = match kind_token {
                b"n" => {
                    if object_offset >= data.len() {
                        return Err(Error::MalformedXref(format!(
                            "object {object_number} points past end of file"
                        )));
                    }
                    XrefEntry::InUse {
                        offset: object_offset,
                    }
                }
                b"f" => XrefEntry::Free,
                _ => {
                    return Err(Error::MalformedXref(format!(
                        "invalid entry type for object {object_number}"
                    )));
                }
            };
            entries.insert(object_number, entry);
        }
    }

    let trailer_value = lexer.parse_value(0)?;
    let trailer = match trailer_value {
        Object::Dictionary(dictionary) => dictionary,
        _ => return Err(Error::MissingTrailer),
    };
    let prev = prev_from_dictionary(&trailer);
    Ok(XrefSection {
        entries,
        trailer,
        prev,
        kind: XrefSectionKind::Classic,
        offset,
    })
}

/// Read a valid `/Prev` byte offset from a trailer dictionary.
fn prev_from_dictionary(dictionary: &Dictionary) -> Option<usize> {
    match dictionary.get("Prev") {
        Some(Object::Integer(value)) if *value >= 0 => usize::try_from(*value).ok(),
        _ => None,
    }
}

/// Parse one xref stream section at `offset`.
///
/// An xref stream is an indirect object whose dictionary doubles as the section
/// trailer. Its `/W` array gives the byte widths of the three fixed-width,
/// big-endian fields of each entry; `/Index` (or `/Size`) selects which object
/// numbers those entries describe. The stream bytes are decoded through the
/// normal filter pipeline (typically `FlateDecode` with optional
/// `/DecodeParms`) using the existing decode cap.
fn parse_xref_stream(data: &[u8], offset: usize) -> Result<XrefSection> {
    let object = parse_indirect_object_at(data, offset, None)?;
    let (dictionary, raw) = match object {
        Object::Stream { dictionary, data } => (dictionary, data),
        _ => return Err(Error::MissingXref(offset)),
    };
    if dictionary.get("Type").and_then(Object::as_name) != Some("XRef") {
        return Err(Error::MissingXref(offset));
    }

    let (width_type, width_two, width_three) = parse_xref_widths(&dictionary)?;
    let row_width = usize::from(width_type)
        .checked_add(usize::from(width_two))
        .and_then(|sum| sum.checked_add(usize::from(width_three)))
        .ok_or_else(|| Error::MalformedXrefStream("xref entry width overflow".to_owned()))?;

    let filters = filters_from_dictionary(&dictionary)?;
    let decoded = decode_stream(&raw, &filters)?;

    let indices = parse_xref_index(&dictionary)?;
    let total_entries = indices
        .iter()
        .try_fold(0u64, |sum, &(_, count)| sum.checked_add(u64::from(count)))
        .ok_or_else(|| Error::MalformedXrefStream("/Index entry count overflow".to_owned()))?;
    if total_entries > u64::from(MAX_XREF_ENTRIES) {
        return Err(Error::MalformedXrefStream(
            "xref stream declares too many entries".to_owned(),
        ));
    }
    // Both a zero row and an oversized row are hostile, even when /Index is
    // empty: the widths describe the entry layout and must stay bounded.
    if row_width == 0 {
        return Err(Error::MalformedXrefStream(
            "xref /W field widths sum to zero".to_owned(),
        ));
    }
    if row_width > MAX_XREF_ROW_WIDTH {
        return Err(Error::MalformedXrefStream(format!(
            "xref /W field widths sum to {row_width}, above the {MAX_XREF_ROW_WIDTH} byte limit"
        )));
    }
    let required = total_entries
        .checked_mul(row_width as u64)
        .ok_or_else(|| Error::MalformedXrefStream("xref stream data size overflow".to_owned()))?;
    if required > decoded.len() as u64 {
        return Err(Error::MalformedXrefStream(format!(
            "xref stream data is truncated: need {required} bytes, have {}",
            decoded.len()
        )));
    }

    let mut entries = HashMap::new();
    let mut position = 0usize;
    for (first, count) in indices {
        for index in 0..count {
            let object_number = first.checked_add(index).ok_or_else(|| {
                Error::MalformedXrefStream("xref object number overflow".to_owned())
            })?;
            // A zero-width type field means type 1 for every entry.
            let kind = read_xref_field(&decoded, &mut position, width_type, 1)?;
            let field_two = read_xref_field(&decoded, &mut position, width_two, 0)?;
            let field_three = read_xref_field(&decoded, &mut position, width_three, 0)?;
            let entry = match kind {
                0 => XrefEntry::Free,
                1 => {
                    let offset = usize::try_from(field_two).map_err(|_| {
                        Error::MalformedXrefStream(format!(
                            "object {object_number} has an out-of-range offset"
                        ))
                    })?;
                    if offset >= data.len() {
                        return Err(Error::MalformedXrefStream(format!(
                            "object {object_number} points past end of file"
                        )));
                    }
                    XrefEntry::InUse { offset }
                }
                2 => {
                    let objstm = u32::try_from(field_two).map_err(|_| {
                        Error::MalformedXrefStream(format!(
                            "object {object_number} has an invalid object stream number"
                        ))
                    })?;
                    if objstm == 0 {
                        return Err(Error::MalformedXrefStream(format!(
                            "object {object_number} references object stream 0"
                        )));
                    }
                    let index = u32::try_from(field_three).map_err(|_| {
                        Error::MalformedXrefStream(format!(
                            "object {object_number} has an invalid object stream index"
                        ))
                    })?;
                    XrefEntry::Compressed { objstm, index }
                }
                // ISO 32000 reserves values above 2 for future use and asks
                // readers to treat them as null entries.
                _ => XrefEntry::Free,
            };
            entries.insert(object_number, entry);
        }
    }

    let prev = prev_from_dictionary(&dictionary);
    Ok(XrefSection {
        entries,
        trailer: dictionary,
        prev,
        kind: XrefSectionKind::Stream,
        offset,
    })
}

/// Parse the three byte widths from an xref stream's `/W` array.
fn parse_xref_widths(dictionary: &Dictionary) -> Result<(u8, u8, u8)> {
    let array = dictionary
        .get("W")
        .and_then(Object::as_array)
        .ok_or_else(|| Error::MalformedXrefStream("missing /W array".to_owned()))?;
    if array.len() != 3 {
        return Err(Error::MalformedXrefStream(format!(
            "/W must have three entries, found {}",
            array.len()
        )));
    }
    let mut widths = [0u8; 3];
    for (slot, item) in widths.iter_mut().zip(array.iter()) {
        let value = item.as_i64().filter(|value| *value >= 0).ok_or_else(|| {
            Error::MalformedXrefStream("/W entries must be non-negative integers".to_owned())
        })?;
        let width = u8::try_from(value)
            .ok()
            .filter(|width| *width <= MAX_XREF_FIELD_WIDTH)
            .ok_or_else(|| {
                Error::MalformedXrefStream(format!(
                    "/W field width {value} exceeds the {MAX_XREF_FIELD_WIDTH} byte limit"
                ))
            })?;
        *slot = width;
    }
    Ok((widths[0], widths[1], widths[2]))
}

/// Parse `/Index` (or the `[0 /Size]` fallback) into `(first, count)` pairs.
fn parse_xref_index(dictionary: &Dictionary) -> Result<Vec<(u32, u32)>> {
    match dictionary.get("Index") {
        None => {
            let size = non_negative_u32(dictionary.get("Size")).ok_or_else(|| {
                Error::MalformedXrefStream(
                    "xref stream needs /Size when /Index is absent".to_owned(),
                )
            })?;
            if size == 0 {
                return Ok(Vec::new());
            }
            Ok(vec![(0, size)])
        }
        Some(Object::Array(items)) => {
            if items.is_empty() || items.len() % 2 != 0 {
                return Err(Error::MalformedXrefStream(
                    "/Index must contain an even number of integers".to_owned(),
                ));
            }
            let subsection_count = items.len() / 2;
            if subsection_count > MAX_XREF_SUBSECTIONS {
                return Err(Error::MalformedXrefStream(
                    "xref stream declares too many /Index subsections".to_owned(),
                ));
            }
            let mut pairs = Vec::with_capacity(subsection_count);
            for chunk in items.as_chunks::<2>().0 {
                let first = non_negative_u32(Some(&chunk[0])).ok_or_else(|| {
                    Error::MalformedXrefStream(
                        "/Index start must be a non-negative integer".to_owned(),
                    )
                })?;
                let count = non_negative_u32(Some(&chunk[1])).ok_or_else(|| {
                    Error::MalformedXrefStream(
                        "/Index count must be a non-negative integer".to_owned(),
                    )
                })?;
                pairs.push((first, count));
            }
            Ok(pairs)
        }
        _ => Err(Error::MalformedXrefStream(
            "/Index must be an array".to_owned(),
        )),
    }
}

/// Read one big-endian field of `width` bytes, advancing `position`.
///
/// A width of zero consumes no bytes and yields `default`, which is how the
/// PDF specification represents an omitted field.
fn read_xref_field(decoded: &[u8], position: &mut usize, width: u8, default: u64) -> Result<u64> {
    if width == 0 {
        return Ok(default);
    }
    let end = position
        .checked_add(usize::from(width))
        .ok_or_else(|| Error::MalformedXrefStream("xref field offset overflow".to_owned()))?;
    let bytes = decoded
        .get(*position..end)
        .ok_or_else(|| Error::MalformedXrefStream("xref stream entry is truncated".to_owned()))?;
    let mut value = 0u64;
    for &byte in bytes {
        value = (value << 8) | u64::from(byte);
    }
    *position = end;
    Ok(value)
}

/// Parse the indirect object that begins at `offset`.
///
/// When `expected` is supplied the object number and generation must match,
/// which validates a classic xref entry against the object body. Xref-stream
/// resolution passes `None` because it runs before a table is available.
fn parse_indirect_object_at(
    data: &[u8],
    offset: usize,
    expected: Option<(u32, u16)>,
) -> Result<Object> {
    let trouble = |reason: String| Error::MalformedObject {
        number: expected.map_or(0, |(number, _)| number),
        reason,
    };
    let mut lexer = Lexer::new(data, offset)?;
    lexer.skip_ws();
    let number_token = lexer
        .read_regular()
        .ok_or_else(|| trouble("missing object number".to_owned()))?;
    let number = parse_decimal(number_token, "object number")
        .map_err(|_| trouble("invalid object number".to_owned()))?;
    if let Some((wanted, _)) = expected
        && u64::try_from(number).ok() != Some(u64::from(wanted))
    {
        return Err(trouble(format!("xref points to object {number}")));
    }

    lexer.skip_ws();
    let generation_token = lexer
        .read_regular()
        .ok_or_else(|| trouble("missing generation number".to_owned()))?;
    let generation = parse_decimal(generation_token, "object generation")
        .map_err(|_| trouble("invalid generation number".to_owned()))?;
    if generation > usize::from(u16::MAX) {
        return Err(trouble("generation exceeds 65535".to_owned()));
    }
    if let Some((_, wanted_generation)) = expected
        && generation != usize::from(wanted_generation)
    {
        return Err(trouble(
            "generation does not match the xref entry".to_owned(),
        ));
    }

    lexer
        .expect_keyword(b"obj")
        .map_err(|_| trouble("missing obj keyword".to_owned()))?;
    lexer.parse_value(0)
}

/// Distinguish a classic `xref` table from an xref stream at `offset`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SectionKind {
    /// The bytes begin with the classic `xref` keyword.
    Classic,
    /// The bytes begin with an indirect object header (`N G obj`).
    Stream,
}

/// Inspect the token at `offset` to decide how the section must be parsed.
fn section_kind(data: &[u8], offset: usize) -> Result<SectionKind> {
    let mut lexer = Lexer::new(data, offset)?;
    lexer.skip_ws();
    let token = lexer
        .read_regular()
        .ok_or_else(|| Error::MalformedXref("empty xref section".to_owned()))?;
    if token == b"xref" {
        Ok(SectionKind::Classic)
    } else if !token.is_empty() && token.iter().all(u8::is_ascii_digit) {
        Ok(SectionKind::Stream)
    } else {
        Err(Error::MalformedXref(format!(
            "unexpected token at xref offset {offset}"
        )))
    }
}

/// Return a typed report for one parsed section.
fn section_report(section: &XrefSection) -> XrefSectionReport {
    XrefSectionReport {
        kind: section.kind,
        offset: section.offset,
        entries: section.entries.len(),
        previous: section.prev,
    }
}

/// Parse the `/XRefStm` companion of a classic section in a hybrid-reference
/// file (ISO 32000-1, 7.5.8.4).
///
/// The pointer is validated and registered in `seen` before the stream is
/// parsed, so a self-reference or a pointer into an already visited section is
/// reported as [`Error::XrefCycle`]. The xref stream's entries supersede the
/// classic table's placeholders for the same object numbers; its `/Prev` is
/// used only when the classic trailer has none.
fn hybrid_xref_stream(
    data: &[u8],
    classic: &XrefSection,
    seen: &mut HashSet<usize>,
) -> Result<Option<XrefSection>> {
    let Some(value) = classic.trailer.get("XRefStm").and_then(Object::as_i64) else {
        return Ok(None);
    };
    let offset = usize::try_from(value).map_err(|_| {
        Error::MalformedXref("/XRefStm must be a non-negative byte offset".to_owned())
    })?;
    if offset >= data.len() {
        return Err(Error::MalformedXref(format!(
            "/XRefStm offset {offset} is outside the file"
        )));
    }
    if !seen.insert(offset) {
        return Err(Error::XrefCycle(offset));
    }
    let section = parse_xref_stream(data, offset).map_err(|error| match error {
        Error::MissingXref(_) => Error::MalformedXrefStream(format!(
            "/XRefStm at {offset} does not reference a /Type /XRef stream"
        )),
        other => other,
    })?;
    Ok(Some(section))
}

/// Merge all xref sections reachable through `/Prev`, classic and stream alike.
///
/// Sections are visited newest first. Entries are merged newest-wins through
/// `or_insert`, and trailer keys are applied oldest-first so the newest section
/// wins there too. Revisiting an offset is a cycle and is rejected.
///
/// When a classic table carries `/XRefStm`, its companion stream is parsed as
/// part of the same revision and pushed first so that the stream's type-2
/// entries replace the classic table's free placeholders. The classic
/// trailer's `/Prev` takes precedence for continuing the chain; the stream's
/// `/Prev` is used as a fallback when the classic one is absent.
fn load_xref(data: &[u8], start: usize) -> Result<XrefTable> {
    let mut sections = Vec::new();
    let mut reports = Vec::new();
    let mut next = Some(start);
    let mut seen = HashSet::new();
    let mut uses_xref_stream = false;

    while let Some(offset) = next {
        if !seen.insert(offset) {
            return Err(Error::XrefCycle(offset));
        }
        if sections.len() >= MAX_XREF_SECTIONS {
            return Err(Error::MalformedXref(
                "too many chained xref sections".to_owned(),
            ));
        }
        let section = match section_kind(data, offset)? {
            SectionKind::Classic => parse_xref_section(data, offset)?,
            SectionKind::Stream => {
                uses_xref_stream = true;
                parse_xref_stream(data, offset)?
            }
        };
        let hybrid = if section.kind == XrefSectionKind::Classic {
            hybrid_xref_stream(data, &section, &mut seen)?
        } else {
            None
        };
        next = section
            .prev
            .or_else(|| hybrid.as_ref().and_then(|stream| stream.prev));
        if let Some(stream) = hybrid {
            uses_xref_stream = true;
            reports.push(section_report(&stream));
            sections.push(stream);
        }
        reports.push(section_report(&section));
        sections.push(section);
    }

    let mut entries = HashMap::new();
    for section in &sections {
        for (number, entry) in &section.entries {
            entries.entry(*number).or_insert_with(|| entry.clone());
        }
    }
    let mut trailer = Dictionary::new();
    for section in sections.iter().rev() {
        for (key, value) in &section.trailer {
            trailer.insert(key.clone(), value.clone());
        }
    }
    let merged_entries = entries.len();
    Ok(XrefTable {
        entries,
        trailer,
        report: XrefReport {
            sections: reports,
            merged_entries,
            uses_xref_stream,
            startxref: start,
        },
    })
}

/// Inspect a PDF's cross-reference structure without resolving its page tree.
///
/// The returned report is deterministic: it lists classic tables, xref streams
/// and hybrid `/XRefStm` companions in visit order, together with their entry
/// counts and `/Prev` pointers. Only the header-independent structural layer is
/// read; a malformed section is reported as an error rather than recovered.
///
/// # Errors
///
/// Returns any [`Error`] produced by locating `startxref` or parsing a section,
/// including [`Error::XrefCycle`] for a cyclic `/Prev` or `/XRefStm` chain.
pub fn inspect_xref(data: &[u8]) -> Result<XrefReport> {
    let start = find_startxref(data)?;
    Ok(load_xref(data, start)?.report)
}

/// Parse and return the indirect object identified by `id`.
///
/// Both xref entry forms are handled: type-1 entries are read at their byte
/// offset, while type-2 entries are expanded from their `/ObjStm`. The `depth`
/// argument bounds nested object-stream resolution.
pub(crate) fn load_object(data: &[u8], xref: &XrefTable, id: ObjectId) -> Result<Object> {
    load_object_inner(data, xref, id, 0)
}

/// Implementation of [`load_object`] carrying an object-stream depth guard.
fn load_object_inner(data: &[u8], xref: &XrefTable, id: ObjectId, depth: u8) -> Result<Object> {
    let entry = xref
        .entries
        .get(&id.number)
        .ok_or(Error::ObjectNotFound(id.number))?;
    match entry {
        XrefEntry::Free => Err(Error::FreeObject(id.number)),
        XrefEntry::InUse { offset } => {
            parse_indirect_object_at(data, *offset, Some((id.number, id.generation)))
        }
        XrefEntry::Compressed { objstm, index } => {
            if depth >= MAX_OBJECT_STREAM_DEPTH {
                return Err(Error::MalformedObjectStream(
                    "object stream nesting is too deep".to_owned(),
                ));
            }
            load_compressed_object(data, xref, *objstm, *index, Some(id.number), depth + 1)
        }
    }
}

/// Extract object `index` from the `/ObjStm` addressed by `objstm`.
///
/// The stream is decoded with the normal filter pipeline, its `/N` header pairs
/// are parsed and bounds-checked, and only the requested object body is parsed
/// on demand. When `expected_number` is supplied the header's object number
/// must match, which detects a corrupt type-2 entry.
fn load_compressed_object(
    data: &[u8],
    xref: &XrefTable,
    objstm: u32,
    index: u32,
    expected_number: Option<u32>,
    depth: u8,
) -> Result<Object> {
    let container = load_object_inner(data, xref, ObjectId::new(objstm, 0), depth)?;
    let (dictionary, raw) = match container {
        Object::Stream { dictionary, data } => (dictionary, data),
        _ => {
            return Err(Error::MalformedObjectStream(format!(
                "object stream {objstm} is not a stream"
            )));
        }
    };
    if dictionary.get("Type").and_then(Object::as_name) != Some("ObjStm") {
        return Err(Error::MalformedObjectStream(format!(
            "object {objstm} is not an /ObjStm"
        )));
    }

    let count = non_negative_u32(dictionary.get("N")).ok_or_else(|| {
        Error::MalformedObjectStream(format!("object stream {objstm} has an invalid /N"))
    })?;
    if count == 0 || count > MAX_OBJECT_STREAM_OBJECTS {
        return Err(Error::MalformedObjectStream(format!(
            "object stream {objstm} declares an out-of-range /N of {count}"
        )));
    }
    let first = usize::try_from(non_negative_u32(dictionary.get("First")).ok_or_else(|| {
        Error::MalformedObjectStream(format!("object stream {objstm} has an invalid /First"))
    })?)
    .map_err(|_| {
        Error::MalformedObjectStream(format!("object stream {objstm} has an oversized /First"))
    })?;

    let filters = filters_from_dictionary(&dictionary)?;
    let decoded = decode_stream(&raw, &filters)?;
    if first > decoded.len() {
        return Err(Error::MalformedObjectStream(format!(
            "object stream {objstm} /First {first} lies outside its {}-byte data",
            decoded.len()
        )));
    }

    // Parse the `N` pairs of (object number, relative offset) that make up the
    // object stream header. `read_regular` stops at whitespace and delimiters,
    // which is exactly how the header is laid out.
    let mut lexer = Lexer::new(&decoded, 0)?;
    let mut offsets: Vec<(u32, u32)> = Vec::with_capacity(count as usize);
    for pair in 0..count {
        lexer.skip_ws();
        let number_token = lexer.read_regular().ok_or_else(|| {
            Error::MalformedObjectStream(format!(
                "object stream {objstm} header pair {pair} is truncated"
            ))
        })?;
        let number = parse_u32_token(number_token).ok_or_else(|| {
            Error::MalformedObjectStream(format!(
                "object stream {objstm} header pair {pair} has an invalid object number"
            ))
        })?;
        lexer.skip_ws();
        let offset_token = lexer.read_regular().ok_or_else(|| {
            Error::MalformedObjectStream(format!(
                "object stream {objstm} header pair {pair} is missing its offset"
            ))
        })?;
        let relative = parse_u32_token(offset_token).ok_or_else(|| {
            Error::MalformedObjectStream(format!(
                "object stream {objstm} header pair {pair} has an invalid offset"
            ))
        })?;
        offsets.push((number, relative));
    }
    if lexer.position() > first {
        return Err(Error::MalformedObjectStream(format!(
            "object stream {objstm} header runs past /First"
        )));
    }

    let (number, relative) = offsets.get(index as usize).ok_or_else(|| {
        Error::MalformedObjectStream(format!(
            "object stream {objstm} has no entry at index {index}"
        ))
    })?;
    if let Some(expected) = expected_number
        && *number != expected
    {
        return Err(Error::MalformedObjectStream(format!(
            "object stream {objstm} index {index} holds object {number}, expected {expected}"
        )));
    }
    let absolute = first.checked_add(*relative as usize).ok_or_else(|| {
        Error::MalformedObjectStream(format!("object stream {objstm} offset overflow"))
    })?;
    if absolute >= decoded.len() {
        return Err(Error::MalformedObjectStream(format!(
            "object stream {objstm} object {number} lies outside its data"
        )));
    }
    let mut body = Lexer::new(&decoded, absolute)?;
    body.parse_value(0)
}

/// Parse a non-negative decimal token into a `u32`.
fn parse_u32_token(token: &[u8]) -> Option<u32> {
    std::str::from_utf8(token).ok()?.parse::<u32>().ok()
}

/// Resolve indirect references to their underlying object value.
fn resolve(data: &[u8], xref: &XrefTable, value: &Object, depth: usize) -> Result<Object> {
    if depth > MAX_REFERENCE_DEPTH {
        return Err(Error::MalformedPageTree(
            "indirect reference chain is too deep".to_owned(),
        ));
    }
    match value {
        Object::Reference(id) => {
            let loaded = load_object(data, xref, *id)?;
            resolve(data, xref, &loaded, depth + 1)
        }
        _ => Ok(value.clone()),
    }
}

fn resolve_id(data: &[u8], xref: &XrefTable, id: ObjectId) -> Result<Object> {
    resolve(data, xref, &Object::Reference(id), 0)
}

fn as_dictionary<'a>(value: &'a Object, context: &str) -> Result<&'a Dictionary> {
    match value {
        Object::Dictionary(dictionary) => Ok(dictionary),
        Object::Stream { dictionary, .. } => Ok(dictionary),
        _ => Err(Error::MalformedPageTree(format!(
            "{context} is not a dictionary"
        ))),
    }
}

/// Page attributes inherited from `/Pages` ancestors.
#[derive(Clone, Copy, Default)]
struct InheritedPage {
    /// `/MediaBox` inherited from the nearest ancestor that declares one.
    media_box: Option<Rectangle>,
    /// `/Resources` inherited from the nearest ancestor that declares one.
    resources: Option<ObjectId>,
}

/// Collects the leaf pages of one document's `/Pages` tree in order.
struct PageTreeWalker<'a> {
    /// Document bytes used to load objects.
    data: &'a [u8],
    /// Cross-reference table for the document.
    xref: &'a XrefTable,
    /// Object numbers already visited, used to reject cycles.
    visited: HashSet<u32>,
    /// Leaf pages collected so far.
    pages: Vec<Page>,
}

impl PageTreeWalker<'_> {
    /// Recursively walk one `/Pages` node, collecting leaf pages in order.
    fn walk(&mut self, id: ObjectId, inherited: InheritedPage, depth: usize) -> Result<()> {
        if depth > MAX_PAGE_TREE_DEPTH {
            return Err(Error::MalformedPageTree(format!(
                "page tree is nested deeper than {MAX_PAGE_TREE_DEPTH} levels at object {}",
                id.number
            )));
        }
        if !self.visited.insert(id.number) {
            return Err(Error::MalformedPageTree(format!(
                "cycle in page tree at object {}",
                id.number
            )));
        }

        let node = resolve_id(self.data, self.xref, id)?;
        let dictionary = as_dictionary(&node, "page tree node")?;

        let media_box = match dictionary.get("MediaBox") {
            Some(value) => Some(parse_media_box(value, id.number)?),
            None => inherited.media_box,
        };
        let resources = match dictionary.get("Resources") {
            Some(Object::Reference(reference)) => Some(*reference),
            _ => inherited.resources,
        };

        let node_type = dictionary
            .get("Type")
            .and_then(Object::as_name)
            .unwrap_or("");
        let kids = dictionary.get("Kids").and_then(Object::as_array);

        let is_branch = node_type == "Pages" || kids.is_some();
        if is_branch {
            let kids = kids.ok_or_else(|| {
                Error::MalformedPageTree(format!("/Pages node {} has no /Kids array", id.number))
            })?;
            for kid in kids {
                let kid_id = kid.as_reference().ok_or_else(|| {
                    Error::MalformedPageTree(format!(
                        "non-reference /Kids entry in object {}",
                        id.number
                    ))
                })?;
                self.walk(
                    kid_id,
                    InheritedPage {
                        media_box,
                        resources,
                    },
                    depth + 1,
                )?;
            }
            return Ok(());
        }

        if self.pages.len() >= MAX_PAGES {
            return Err(Error::MalformedPageTree("too many pages".to_owned()));
        }
        let contents = match dictionary.get("Contents") {
            Some(Object::Reference(reference)) => vec![*reference],
            Some(Object::Array(items)) => items
                .iter()
                .filter_map(Object::as_reference)
                .collect::<Vec<_>>(),
            _ => Vec::new(),
        };
        self.pages.push(Page {
            index: self.pages.len(),
            id,
            media_box: media_box.unwrap_or_default(),
            contents,
            resources,
        });
        Ok(())
    }
}

fn parse_media_box(value: &Object, page_id: u32) -> Result<Rectangle> {
    let values = value.as_array().ok_or(Error::InvalidMediaBox(page_id))?;
    if values.len() < 4 {
        return Err(Error::InvalidMediaBox(page_id));
    }
    let mut numbers = [0.0f64; 4];
    for (slot, item) in numbers.iter_mut().zip(values.iter().take(4)) {
        *slot = item.as_f64().ok_or(Error::InvalidMediaBox(page_id))?;
    }
    Ok(Rectangle::new(
        numbers[0], numbers[1], numbers[2], numbers[3],
    ))
}

fn non_negative_u32(value: Option<&Object>) -> Option<u32> {
    match value {
        Some(Object::Integer(number)) if *number >= 0 => u32::try_from(*number).ok(),
        _ => None,
    }
}

fn parse_decimal(token: &[u8], context: &str) -> Result<usize> {
    let text = std::str::from_utf8(token)
        .map_err(|_| Error::MalformedXref(format!("{context} is not UTF-8")))?;
    text.parse::<usize>()
        .map_err(|_| Error::MalformedXref(format!("invalid {context}: {text}")))
}

/// Whether the newest trailer names an `/Encrypt` dictionary.
///
/// Reads only the cross-reference layer and the merged trailer, so it works on
/// encrypted files whose object streams cannot be decoded without a password.
/// A `/Encrypt` name that appears inside a content stream or comment is never
/// consulted.
pub(crate) fn trailer_encryption_reference(data: &[u8]) -> Result<Option<ObjectId>> {
    let start = find_startxref(data)?;
    let table = load_xref(data, start)?;
    Ok(table
        .trailer()
        .get("Encrypt")
        .and_then(Object::as_reference))
}

/// Return the byte offset recorded by the final `startxref` marker.
///
/// This is the offset an incremental save must reference through `/Prev`.
pub(crate) fn startxref_offset(data: &[u8]) -> Result<usize> {
    find_startxref(data)
}

/// Best-effort read of the final xref table's in-use object offsets.
///
/// The repair scanner uses this to distinguish a merely misplaced `startxref`
/// (whose table is still readable) from a genuinely broken xref section.
/// `None` means the `startxref` marker or the xref section could not be parsed.
pub(crate) fn xref_offsets(data: &[u8]) -> Option<Vec<(u32, usize)>> {
    let start = find_startxref(data).ok()?;
    let table = load_xref(data, start).ok()?;
    Some(table.in_use_entries())
}

/// Return the byte range covering the whole indirect object at `offset`.
///
/// The range starts at the xref offset and ends just after the `endobj`
/// keyword. Stream bodies are skipped using their declared `/Length`, so an
/// `endobj` byte sequence inside binary stream data cannot truncate the copy.
/// A source object without a terminating `endobj` is reported as
/// [`Error::UnexpectedToken`] instead of being silently repaired.
pub(crate) fn object_range(data: &[u8], offset: usize) -> Result<Range<usize>> {
    let mut lexer = Lexer::new(data, offset)?;
    lexer.skip_ws();
    lexer.read_regular().ok_or_else(|| Error::MalformedObject {
        number: 0,
        reason: "missing object number".to_owned(),
    })?;
    lexer.skip_ws();
    lexer.read_regular().ok_or_else(|| Error::MalformedObject {
        number: 0,
        reason: "missing generation number".to_owned(),
    })?;
    lexer.expect_keyword(b"obj")?;
    lexer.skip_ws();
    lexer.parse_value(0)?;
    let value_end = lexer.position();
    let tail = data.get(value_end..).unwrap_or(&[]);
    let relative_end = tail
        .windows(6)
        .position(|window| window == b"endobj")
        .ok_or_else(|| Error::UnexpectedToken("missing endobj after object".to_owned()))?;
    Ok(offset..value_end + relative_end + 6)
}

/// Return `true` when the bytes at `offset` look like an indirect object
/// header (`N G obj`), which is how a cross-reference stream section begins.
pub(crate) fn looks_like_xref_stream(data: &[u8], offset: usize) -> bool {
    let Ok(mut lexer) = Lexer::new(data, offset) else {
        return false;
    };
    lexer.skip_ws();
    let Some(number) = lexer.read_regular() else {
        return false;
    };
    if number.is_empty() || !number.iter().all(u8::is_ascii_digit) {
        return false;
    }
    lexer.skip_ws();
    let Some(generation) = lexer.read_regular() else {
        return false;
    };
    if generation.is_empty() || !generation.iter().all(u8::is_ascii_digit) {
        return false;
    }
    lexer.expect_keyword(b"obj").is_ok()
}
