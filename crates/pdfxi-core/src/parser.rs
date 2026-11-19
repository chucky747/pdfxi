//! Cross-reference and page-tree parsing.
//!
//! This module implements the classic (table-based) cross-reference format
//! defined by ISO 32000, including chained `/Prev` sections produced by
//! incremental updates. Xref streams and object streams are deliberately out
//! of scope for this stage and remain handled by a later task.

use std::collections::{HashMap, HashSet};

use crate::lexer::Lexer;
use crate::object::{Dictionary, Object, ObjectId};
use crate::page::{Page, Rectangle};
use crate::{Error, Result};

/// Maximum number of indirect references followed for a single value.
const MAX_REFERENCE_DEPTH: usize = 32;
/// Maximum number of leaf pages accepted before treating the tree as hostile.
const MAX_PAGES: usize = 1_000_000;
/// Maximum number of chained `/Prev` xref sections.
const MAX_XREF_SECTIONS: usize = 64;
/// Maximum number of entries in one xref subsection, relative to file size.
const XREF_ENTRY_MIN_BYTES: usize = 4;

/// One entry of a cross-reference table.
#[derive(Debug, Clone)]
enum XrefEntry {
    /// The object number exists but the object is free.
    Free,
    /// The object is in use at the given byte offset.
    InUse {
        /// Byte offset of the indirect object definition.
        offset: usize,
    },
}

/// A single classic `xref` section and its trailer.
#[derive(Debug, Clone)]
struct XrefSection {
    entries: HashMap<u32, XrefEntry>,
    trailer: Dictionary,
    prev: Option<usize>,
}

/// The merged cross-reference table for a document.
#[derive(Debug, Clone)]
pub(crate) struct XrefTable {
    entries: HashMap<u32, XrefEntry>,
    trailer: Dictionary,
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

    let mut pages = Vec::new();
    let mut visited = HashSet::new();
    walk_pages(data, &xref, pages_id, None, None, &mut visited, &mut pages)?;

    Ok(Structure {
        root_id,
        trailer_size,
        declared_page_count,
        pages,
        xref,
    })
}

/// Return the concatenated, unfiltered content bytes for `page`.
///
/// Each `/Contents` reference is loaded through the cross-reference table and
/// must resolve to a stream object. Streams carrying a `/Filter` are rejected
/// with [`Error::FilteredContent`] so callers never receive encoded bytes by
/// mistake.
pub(crate) fn page_content(data: &[u8], structure: &Structure, page: &Page) -> Result<Vec<u8>> {
    let mut out = Vec::new();
    for id in &page.contents {
        let object = load_object(data, &structure.xref, *id)?;
        match object {
            Object::Stream { dictionary, data } => {
                if has_filter(&dictionary) {
                    return Err(Error::FilteredContent(*id));
                }
                if !out.is_empty() {
                    out.push(b'\n');
                }
                out.extend_from_slice(&data);
            }
            _ => return Err(Error::ContentNotStream(*id)),
        }
    }
    Ok(out)
}

/// Return `true` when a stream dictionary declares at least one real filter.
fn has_filter(dictionary: &Dictionary) -> bool {
    match dictionary.get("Filter") {
        None | Some(Object::Null) => false,
        Some(Object::Array(items)) => items.iter().any(|item| !matches!(item, Object::Null)),
        Some(_) => true,
    }
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
    let prev = match trailer.get("Prev") {
        Some(Object::Integer(value)) if *value >= 0 => usize::try_from(*value).ok(),
        _ => None,
    };
    Ok(XrefSection {
        entries,
        trailer,
        prev,
    })
}

/// Merge all xref sections reachable through `/Prev`.
fn load_xref(data: &[u8], start: usize) -> Result<XrefTable> {
    let mut sections = Vec::new();
    let mut next = Some(start);
    let mut seen = HashSet::new();

    while let Some(offset) = next {
        if !seen.insert(offset) {
            return Err(Error::XrefCycle(offset));
        }
        if sections.len() >= MAX_XREF_SECTIONS {
            return Err(Error::MalformedXref(
                "too many chained xref sections".to_owned(),
            ));
        }
        let section = parse_xref_section(data, offset)?;
        next = section.prev;
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
    Ok(XrefTable { entries, trailer })
}

/// Parse and return the indirect object identified by `id`.
fn load_object(data: &[u8], xref: &XrefTable, id: ObjectId) -> Result<Object> {
    let entry = xref
        .entries
        .get(&id.number)
        .ok_or(Error::ObjectNotFound(id.number))?;
    let offset = match entry {
        XrefEntry::Free => return Err(Error::FreeObject(id.number)),
        XrefEntry::InUse { offset, .. } => *offset,
    };

    let mut lexer = Lexer::new(data, offset)?;
    lexer.skip_ws();
    let number_token = lexer.read_regular().ok_or_else(|| Error::MalformedObject {
        number: id.number,
        reason: "missing object number".to_owned(),
    })?;
    let number =
        parse_decimal(number_token, "object number").map_err(|_| Error::MalformedObject {
            number: id.number,
            reason: "invalid object number".to_owned(),
        })?;
    if number != id.number as usize {
        return Err(Error::MalformedObject {
            number: id.number,
            reason: format!("xref points to object {number}"),
        });
    }

    lexer.skip_ws();
    let generation_token = lexer.read_regular().ok_or_else(|| Error::MalformedObject {
        number: id.number,
        reason: "missing generation number".to_owned(),
    })?;
    let generation = parse_decimal(generation_token, "object generation").map_err(|_| {
        Error::MalformedObject {
            number: id.number,
            reason: "invalid generation number".to_owned(),
        }
    })?;
    if generation != usize::from(id.generation) {
        return Err(Error::MalformedObject {
            number: id.number,
            reason: "generation does not match the xref entry".to_owned(),
        });
    }

    lexer
        .expect_keyword(b"obj")
        .map_err(|_| Error::MalformedObject {
            number: id.number,
            reason: "missing obj keyword".to_owned(),
        })?;
    lexer.parse_value(0)
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

/// Recursively walk one `/Pages` node, collecting leaf pages in order.
fn walk_pages(
    data: &[u8],
    xref: &XrefTable,
    id: ObjectId,
    inherited_media: Option<Rectangle>,
    inherited_resources: Option<ObjectId>,
    visited: &mut HashSet<u32>,
    pages: &mut Vec<Page>,
) -> Result<()> {
    if !visited.insert(id.number) {
        return Err(Error::MalformedPageTree(format!(
            "cycle in page tree at object {}",
            id.number
        )));
    }

    let node = resolve_id(data, xref, id)?;
    let dictionary = as_dictionary(&node, "page tree node")?;

    let media_box = match dictionary.get("MediaBox") {
        Some(value) => Some(parse_media_box(value, id.number)?),
        None => inherited_media,
    };
    let resources = match dictionary.get("Resources") {
        Some(Object::Reference(reference)) => Some(*reference),
        _ => inherited_resources,
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
            walk_pages(data, xref, kid_id, media_box, resources, visited, pages)?;
        }
        return Ok(());
    }

    if pages.len() >= MAX_PAGES {
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
    pages.push(Page {
        index: pages.len(),
        id,
        media_box: media_box.unwrap_or_default(),
        contents,
        resources,
    });
    Ok(())
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
