//! Concrete PDF serialization of emitted form dictionaries.
//!
//! [`crate::acroform_dictionary`] and [`crate::FieldSpec`] emit *models* of the
//! `/AcroForm` field nodes and widget annotations using
//! [`PdfValue`](crate::acroform::PdfValue). This module turns those models into
//! an actual, loadable PDF byte stream: a catalogue, a page tree, a page with
//! `/Annots`, an `/AcroForm` with `/Fields`, one indirect object per field and
//! widget, a classic cross-reference table and a trailer.
//!
//! # Contract
//!
//! [`serialize_form_pdf`] takes the emitted field dictionaries, the emitted
//! widget dictionaries and the *real* page object reference. Each field must
//! carry a non-empty `/T`, an `/FT` name and exactly one widget, either inline
//! under `/Kids` (the shape produced by
//! [`FieldSpec::field_dictionary`](crate::FieldSpec::field_dictionary)) or as a
//! matching entry in the `widgets` slice. The serializer:
//!
//! - promotes every widget into its own indirect object;
//! - rewrites the field's `/Kids` to reference that object;
//! - overwrites the widget's `/P` with the supplied page reference, replacing
//!   the placeholder index emitted by the modelling layer;
//! - fills the page's `/Annots` with the widget references; and
//! - points the catalogue's `/AcroForm` at a dictionary whose `/Fields` lists
//!   the field objects in the input order.
//!
//! Object numbers are assigned deterministically. The caller's page number is
//! reserved; the catalogue, pages, `/AcroForm`, field and widget objects take
//! the next free numbers. Numbers are bounded by [`MAX_OBJECT_NUMBER`] so a
//! hostile page reference cannot allocate an enormous xref table.
//!
//! The companion [`parse_pdf`] is a bounded reader for the subset of PDF syntax
//! this writer emits. It is public so callers (and the round-trip tests) can
//! verify a produced document without a PDF library. No dependency, filesystem
//! access or JavaScript is involved, and no function here panics on hostile
//! input.

use crate::acroform::{ObjectRef, PdfValue};
use std::collections::{BTreeMap, BTreeSet};

/// Largest object number accepted for the owning page reference.
pub const MAX_OBJECT_NUMBER: u32 = 1_000_000;
/// Largest number of indirect objects emitted in a single document.
pub const MAX_OBJECTS: usize = 65_535;
/// Maximum nesting depth accepted by the value writer and reader.
const MAX_DEPTH: usize = 64;

/// A structured error produced while serializing a form into a PDF.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SerializeError {
    /// No owning page reference was supplied.
    #[error("no owning page reference was supplied")]
    MissingPageReference,
    /// The supplied page reference cannot address a PDF object.
    #[error("invalid owning page reference {0}")]
    InvalidPageReference(ObjectRef),
    /// Explicit widget dictionaries did not match the field dictionaries.
    #[error("field count {fields} does not match widget count {widgets}")]
    LengthMismatch {
        /// Number of field dictionaries.
        fields: usize,
        /// Number of widget dictionaries.
        widgets: usize,
    },
    /// Two fields resolved to the same fully qualified name.
    #[error("duplicate field name '{0}'")]
    DuplicateFieldName(String),
    /// A supplied dictionary had the wrong shape or a missing required entry.
    #[error("malformed {context}: {detail}")]
    MalformedDictionary {
        /// The entry being read, for example `field /T`.
        context: &'static str,
        /// A human-readable description of the problem.
        detail: String,
    },
    /// A numeric value was not finite and cannot be written to a PDF.
    #[error("a non-finite number cannot be serialized")]
    NonFiniteNumber,
    /// The document would exceed [`MAX_OBJECTS`] indirect objects.
    #[error("too many PDF objects: {0}")]
    TooManyObjects(usize),
}

/// A structured error produced while reading a serialized PDF back in.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ParseError {
    /// The input did not start with a `%PDF-` header.
    #[error("missing %PDF header")]
    MissingHeader,
    /// No `startxref` marker was found.
    #[error("startxref offset not found")]
    MissingStartXref,
    /// The xref table or trailer was malformed.
    #[error("malformed xref: {0}")]
    MalformedXref(String),
    /// The input ended while a value was being read.
    #[error("unexpected end of input while reading {0}")]
    UnexpectedEof(&'static str),
    /// A token had an unexpected value.
    #[error("unexpected token: {0}")]
    UnexpectedToken(String),
    /// A number was not finite.
    #[error("non-finite number in PDF")]
    NonFiniteNumber,
    /// A reference cycle or excessive nesting was detected.
    #[error("nesting limit exceeded")]
    NestingTooDeep,
    /// An xref entry pointed at a missing object.
    #[error("object {0} is missing")]
    MissingObject(u32),
}

/// A PDF document parsed by [`parse_pdf`].
#[derive(Debug, Clone, PartialEq)]
pub struct PdfDocument {
    /// Indirect objects keyed by object number.
    pub objects: BTreeMap<u32, PdfValue>,
    /// The catalogue reference read from the trailer `/Root`.
    pub root: ObjectRef,
}

impl PdfDocument {
    /// Follows indirect references until a direct value is reached.
    ///
    /// Resolution is bounded by 64 hops and rejects reference cycles.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError::MissingObject`] for a dangling reference,
    /// [`ParseError::NestingTooDeep`] for a cycle or an over-long chain.
    pub fn resolve(&self, value: &PdfValue) -> Result<PdfValue, ParseError> {
        let mut current = value.clone();
        let mut seen = BTreeSet::new();
        for _ in 0..MAX_DEPTH {
            if let PdfValue::Reference(id) = current {
                if !seen.insert(id) {
                    return Err(ParseError::NestingTooDeep);
                }
                current = self
                    .objects
                    .get(&id.number)
                    .cloned()
                    .ok_or(ParseError::MissingObject(id.number))?;
            } else {
                return Ok(current);
            }
        }
        Err(ParseError::NestingTooDeep)
    }

    /// Returns the catalogue dictionary referenced by the trailer.
    ///
    /// # Errors
    ///
    /// Returns [`ParseError`] when the root object is absent or is not a
    /// dictionary.
    pub fn catalog(&self) -> Result<BTreeMap<String, PdfValue>, ParseError> {
        let root = self
            .objects
            .get(&self.root.number)
            .cloned()
            .ok_or(ParseError::MissingObject(self.root.number))?;
        root.as_dict()
            .cloned()
            .ok_or_else(|| ParseError::UnexpectedToken("catalog is not a dictionary".to_owned()))
    }
}

/// Assigns deterministic, collision-free object numbers.
struct Allocator {
    next: u32,
    reserved: u32,
}

impl Allocator {
    fn new(reserved: u32) -> Self {
        Self { next: 1, reserved }
    }

    fn alloc(&mut self, objects: &mut BTreeMap<u32, PdfValue>) -> Result<u32, SerializeError> {
        loop {
            if self.next == self.reserved || objects.contains_key(&self.next) {
                self.next = self
                    .next
                    .checked_add(1)
                    .ok_or(SerializeError::TooManyObjects(MAX_OBJECTS))?;
                continue;
            }
            break;
        }
        let number = self.next;
        objects.insert(number, PdfValue::Null);
        self.next = self
            .next
            .checked_add(1)
            .ok_or(SerializeError::TooManyObjects(MAX_OBJECTS))?;
        if objects.len() > MAX_OBJECTS {
            return Err(SerializeError::TooManyObjects(objects.len()));
        }
        Ok(number)
    }
}

/// Serializes emitted field and widget dictionaries into a complete PDF.
///
/// `fields` holds the field-node dictionaries, each with a non-empty `/T`, an
/// `/FT` name and one widget under `/Kids`. `widgets` may be empty, in which
/// case each widget is taken from its field's `/Kids`; otherwise it must contain
/// exactly one widget dictionary per field, in the same order. `page` is the
/// real page object reference that owns the widgets; `None` is a typed
/// [`SerializeError::MissingPageReference`] error.
///
/// # Errors
///
/// Returns [`SerializeError`] for a missing or invalid page reference, a field
/// count mismatch, a malformed field/widget dictionary, a duplicate field name,
/// a non-finite number or an object-count overflow.
pub fn serialize_form_pdf(
    fields: &[PdfValue],
    widgets: &[PdfValue],
    page: Option<ObjectRef>,
) -> Result<Vec<u8>, SerializeError> {
    let page = page.ok_or(SerializeError::MissingPageReference)?;
    if page.number == 0 || page.number > MAX_OBJECT_NUMBER {
        return Err(SerializeError::InvalidPageReference(page));
    }
    if fields.is_empty() {
        return Err(SerializeError::MalformedDictionary {
            context: "Fields",
            detail: "no field dictionaries were supplied".to_owned(),
        });
    }
    if !widgets.is_empty() && widgets.len() != fields.len() {
        return Err(SerializeError::LengthMismatch {
            fields: fields.len(),
            widgets: widgets.len(),
        });
    }

    let mut field_dicts = Vec::with_capacity(fields.len());
    let mut seen = BTreeSet::new();
    for (index, field) in fields.iter().enumerate() {
        let dict = field
            .as_dict()
            .ok_or_else(|| SerializeError::MalformedDictionary {
                context: "Fields entry",
                detail: format!("entry {index} is not a dictionary"),
            })?;
        let name = dict
            .get("T")
            .and_then(PdfValue::as_text)
            .filter(|name| !name.is_empty())
            .ok_or_else(|| SerializeError::MalformedDictionary {
                context: "field /T",
                detail: format!("entry {index} has no non-empty /T text"),
            })?;
        if dict.get("FT").and_then(PdfValue::as_name).is_none() {
            return Err(SerializeError::MalformedDictionary {
                context: "field /FT",
                detail: format!("field '{name}' has no /FT name"),
            });
        }
        if !seen.insert(name.to_owned()) {
            return Err(SerializeError::DuplicateFieldName(name.to_owned()));
        }
        field_dicts.push(dict.clone());
    }

    let widget_dicts: Vec<BTreeMap<String, PdfValue>> = if widgets.is_empty() {
        let mut collected = Vec::with_capacity(field_dicts.len());
        for (index, dict) in field_dicts.iter().enumerate() {
            let kids = dict
                .get("Kids")
                .and_then(PdfValue::as_array)
                .ok_or_else(|| SerializeError::MalformedDictionary {
                    context: "field /Kids",
                    detail: format!("field {index} has no /Kids array"),
                })?;
            let kid = kids
                .first()
                .ok_or_else(|| SerializeError::MalformedDictionary {
                    context: "field /Kids",
                    detail: format!("field {index} has no widget"),
                })?;
            collected.push(check_widget(kid, index)?);
        }
        collected
    } else {
        widgets
            .iter()
            .enumerate()
            .map(|(index, widget)| check_widget(widget, index))
            .collect::<Result<Vec<_>, SerializeError>>()?
    };

    let mut objects = BTreeMap::new();
    let mut allocator = Allocator::new(page.number);
    let catalog_number = allocator.alloc(&mut objects)?;
    let pages_number = allocator.alloc(&mut objects)?;
    let acroform_number = allocator.alloc(&mut objects)?;
    let mut field_numbers = Vec::with_capacity(field_dicts.len());
    for _ in 0..field_dicts.len() {
        field_numbers.push(allocator.alloc(&mut objects)?);
    }
    let mut widget_numbers = Vec::with_capacity(widget_dicts.len());
    for _ in 0..widget_dicts.len() {
        widget_numbers.push(allocator.alloc(&mut objects)?);
    }

    for (index, (dict, &number)) in field_dicts.iter().zip(&field_numbers).enumerate() {
        let mut emitted = dict.clone();
        let widget_ref = PdfValue::Reference(ObjectRef::new(widget_numbers[index], 0));
        emitted.insert("Kids".to_owned(), PdfValue::Array(vec![widget_ref]));
        objects.insert(number, PdfValue::Dictionary(emitted));
    }
    for (dict, &number) in widget_dicts.iter().zip(&widget_numbers) {
        let mut emitted = dict.clone();
        emitted.insert("P".to_owned(), PdfValue::Reference(page));
        objects.insert(number, PdfValue::Dictionary(emitted));
    }

    let widget_refs: Vec<PdfValue> = widget_numbers
        .iter()
        .map(|&number| PdfValue::Reference(ObjectRef::new(number, 0)))
        .collect();
    let field_refs: Vec<PdfValue> = field_numbers
        .iter()
        .map(|&number| PdfValue::Reference(ObjectRef::new(number, 0)))
        .collect();

    let mut page_dict = BTreeMap::new();
    page_dict.insert("Type".to_owned(), PdfValue::Name("Page".to_owned()));
    page_dict.insert(
        "Parent".to_owned(),
        PdfValue::Reference(ObjectRef::new(pages_number, 0)),
    );
    page_dict.insert(
        "MediaBox".to_owned(),
        PdfValue::Array(vec![
            PdfValue::Number(0.0),
            PdfValue::Number(0.0),
            PdfValue::Number(612.0),
            PdfValue::Number(792.0),
        ]),
    );
    page_dict.insert(
        "Resources".to_owned(),
        PdfValue::Dictionary(BTreeMap::new()),
    );
    page_dict.insert("Annots".to_owned(), PdfValue::Array(widget_refs));
    objects.insert(page.number, PdfValue::Dictionary(page_dict));

    let mut pages_dict = BTreeMap::new();
    pages_dict.insert("Type".to_owned(), PdfValue::Name("Pages".to_owned()));
    pages_dict.insert(
        "Kids".to_owned(),
        PdfValue::Array(vec![PdfValue::Reference(page)]),
    );
    pages_dict.insert("Count".to_owned(), PdfValue::Number(1.0));
    objects.insert(pages_number, PdfValue::Dictionary(pages_dict));

    let mut acroform_dict = BTreeMap::new();
    acroform_dict.insert("Fields".to_owned(), PdfValue::Array(field_refs));
    acroform_dict.insert("NeedAppearances".to_owned(), PdfValue::Bool(false));
    acroform_dict.insert("DA".to_owned(), PdfValue::Text("Helv 0 Tf 0 g".to_owned()));
    objects.insert(acroform_number, PdfValue::Dictionary(acroform_dict));

    let mut catalog = BTreeMap::new();
    catalog.insert("Type".to_owned(), PdfValue::Name("Catalog".to_owned()));
    catalog.insert(
        "Pages".to_owned(),
        PdfValue::Reference(ObjectRef::new(pages_number, 0)),
    );
    catalog.insert(
        "AcroForm".to_owned(),
        PdfValue::Reference(ObjectRef::new(acroform_number, 0)),
    );
    objects.insert(catalog_number, PdfValue::Dictionary(catalog));

    write_pdf(&objects, catalog_number)
}

fn check_widget(
    value: &PdfValue,
    index: usize,
) -> Result<BTreeMap<String, PdfValue>, SerializeError> {
    let dict = value
        .as_dict()
        .ok_or_else(|| SerializeError::MalformedDictionary {
            context: "widget",
            detail: format!("widget {index} is not a dictionary"),
        })?;
    if dict.get("Subtype").and_then(PdfValue::as_name) != Some("Widget") {
        return Err(SerializeError::MalformedDictionary {
            context: "widget /Subtype",
            detail: format!("widget {index} is not a /Widget annotation"),
        });
    }
    Ok(dict.clone())
}

fn write_pdf(objects: &BTreeMap<u32, PdfValue>, root: u32) -> Result<Vec<u8>, SerializeError> {
    let mut out = Vec::new();
    out.extend_from_slice(b"%PDF-1.7\n%\xE2\xE3\xCF\xD3\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (&number, value) in objects {
        let offset = out.len();
        offsets.push((number, offset));
        out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
        write_value(&mut out, value, 0)?;
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = out.len();
    let max = objects.last_key_value().map_or(0, |(number, _)| *number);
    out.extend_from_slice(format!("xref\n0 {}\n", max + 1).as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    let mut cursor = offsets.iter().peekable();
    for number in 1..=max {
        match cursor.peek() {
            Some(&(candidate, offset)) if *candidate == number => {
                out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
                cursor.next();
            }
            _ => out.extend_from_slice(b"0000000000 00000 f \n"),
        }
    }

    out.extend_from_slice(b"trailer\n");
    let mut trailer = BTreeMap::new();
    trailer.insert("Size".to_owned(), PdfValue::Number(f64::from(max + 1)));
    trailer.insert(
        "Root".to_owned(),
        PdfValue::Reference(ObjectRef::new(root, 0)),
    );
    write_dict(&mut out, &trailer, 0)?;
    out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(out)
}

fn format_number(value: f64) -> Result<String, SerializeError> {
    if !value.is_finite() {
        return Err(SerializeError::NonFiniteNumber);
    }
    if value == 0.0 {
        return Ok("0".to_owned());
    }
    if value.fract() == 0.0 && value.abs() < 9_007_199_254_740_992.0 {
        return Ok(format!("{}", value as i64));
    }
    Ok(format!("{value}"))
}

fn write_value(out: &mut Vec<u8>, value: &PdfValue, depth: usize) -> Result<(), SerializeError> {
    if depth > MAX_DEPTH {
        return Err(SerializeError::MalformedDictionary {
            context: "PDF value",
            detail: "nesting limit exceeded".to_owned(),
        });
    }
    match value {
        PdfValue::Null => out.extend_from_slice(b"null"),
        PdfValue::Bool(flag) => {
            out.extend_from_slice(if *flag { b"true" } else { b"false" });
        }
        PdfValue::Number(number) => out.extend_from_slice(format_number(*number)?.as_bytes()),
        PdfValue::Text(text) => write_string(out, text),
        PdfValue::Name(name) => write_name(out, name),
        PdfValue::Reference(id) => {
            out.extend_from_slice(format!("{} {} R", id.number, id.generation).as_bytes());
        }
        PdfValue::Array(items) => {
            out.push(b'[');
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push(b' ');
                }
                write_value(out, item, depth + 1)?;
            }
            out.push(b']');
        }
        PdfValue::Dictionary(entries) => write_dict(out, entries, depth)?,
        PdfValue::Stream { dict, data } => {
            let mut entries = dict.clone();
            entries.insert("Length".to_owned(), PdfValue::Number(data.len() as f64));
            write_dict(out, &entries, depth)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
    }
    Ok(())
}

fn write_dict(
    out: &mut Vec<u8>,
    entries: &BTreeMap<String, PdfValue>,
    depth: usize,
) -> Result<(), SerializeError> {
    out.extend_from_slice(b"<<");
    for (key, value) in entries {
        out.push(b' ');
        write_name(out, key);
        out.push(b' ');
        write_value(out, value, depth + 1)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

fn write_name(out: &mut Vec<u8>, name: &str) {
    out.push(b'/');
    for &byte in name.as_bytes() {
        let printable = (0x21..=0x7e).contains(&byte)
            && !matches!(
                byte,
                b'#' | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
            );
        if printable {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

fn write_string(out: &mut Vec<u8>, text: &str) {
    out.push(b'(');
    for &byte in text.as_bytes() {
        match byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(byte);
            }
            b'\n' => out.extend_from_slice(b"\\n"),
            b'\r' => out.extend_from_slice(b"\\r"),
            b'\t' => out.extend_from_slice(b"\\t"),
            0x20..=0x7e => out.push(byte),
            other => out.extend_from_slice(format!("\\{other:03o}").as_bytes()),
        }
    }
    out.push(b')');
}

/// Parses a PDF byte stream produced by this module.
///
/// The reader understands the object syntax emitted by [`serialize_form_pdf`]:
/// booleans, numbers, literal strings, names, arrays, dictionaries, streams and
/// indirect references, plus a classic `xref` table and `trailer`. All loops are
/// bounded by the input length and a fixed nesting limit, so hostile input cannot make
/// the reader run unbounded or recurse without limit.
///
/// # Errors
///
/// Returns [`ParseError`] for a missing header or `startxref`, a malformed xref
/// or trailer, an invalid token, a dangling reference target or excessive
/// nesting.
pub fn parse_pdf(bytes: &[u8]) -> Result<PdfDocument, ParseError> {
    if !bytes.starts_with(b"%PDF-") {
        return Err(ParseError::MissingHeader);
    }
    let marker = b"startxref";
    let marker_pos = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)
        .ok_or(ParseError::MissingStartXref)?;
    let mut cursor = Cursor::new(bytes, marker_pos + marker.len());
    let offset_token = cursor.read_token()?;
    let xref_offset: usize = parse_usize(&offset_token)
        .ok_or_else(|| ParseError::MalformedXref("invalid startxref offset".to_owned()))?;
    if xref_offset >= bytes.len() {
        return Err(ParseError::MalformedXref(
            "startxref offset out of range".to_owned(),
        ));
    }
    cursor.pos = xref_offset;
    if cursor.read_token()? != b"xref" {
        return Err(ParseError::MalformedXref("missing xref keyword".to_owned()));
    }

    let mut offsets = BTreeMap::new();
    loop {
        let token = cursor.read_token()?;
        if token == b"trailer" {
            break;
        }
        let start = parse_u32(&token)
            .ok_or_else(|| ParseError::MalformedXref("invalid subsection start".to_owned()))?;
        let count_token = cursor.read_token()?;
        let count = parse_usize(&count_token)
            .ok_or_else(|| ParseError::MalformedXref("invalid subsection count".to_owned()))?;
        for index in 0..count {
            let offset_token = cursor.read_token()?;
            let _generation = cursor.read_token()?;
            let kind = cursor.read_token()?;
            if kind == b"n" {
                let offset = parse_usize(&offset_token)
                    .ok_or_else(|| ParseError::MalformedXref("invalid object offset".to_owned()))?;
                let number = start.checked_add(index as u32).ok_or_else(|| {
                    ParseError::MalformedXref("object number overflow".to_owned())
                })?;
                offsets.insert(number, offset);
            }
        }
    }

    let trailer = parse_value(&mut cursor, 0)?;
    let trailer = trailer
        .as_dict()
        .ok_or_else(|| ParseError::MalformedXref("trailer is not a dictionary".to_owned()))?;
    let root = trailer
        .get("Root")
        .and_then(PdfValue::as_reference)
        .ok_or_else(|| ParseError::MalformedXref("trailer has no /Root reference".to_owned()))?;

    let mut objects = BTreeMap::new();
    for &offset in offsets.values() {
        if offset >= bytes.len() {
            return Err(ParseError::MalformedXref(
                "object offset out of range".to_owned(),
            ));
        }
        let mut object_cursor = Cursor::new(bytes, offset);
        let number_token = object_cursor.read_token()?;
        let declared = parse_u32(&number_token)
            .ok_or_else(|| ParseError::MalformedXref("invalid object number".to_owned()))?;
        let _generation = object_cursor.read_token()?;
        if object_cursor.read_token()? != b"obj" {
            return Err(ParseError::MalformedXref("missing obj keyword".to_owned()));
        }
        let value = parse_value(&mut object_cursor, 0)?;
        objects.insert(declared, value);
    }
    Ok(PdfDocument { objects, root })
}

fn parse_usize(token: &[u8]) -> Option<usize> {
    std::str::from_utf8(token).ok()?.parse().ok()
}

fn parse_u32(token: &[u8]) -> Option<u32> {
    std::str::from_utf8(token).ok()?.parse().ok()
}

struct Cursor<'a> {
    data: &'a [u8],
    pos: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8], pos: usize) -> Self {
        Self { data, pos }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.data.len()
    }

    fn peek(&self) -> Option<u8> {
        self.data.get(self.pos).copied()
    }

    fn skip_ws(&mut self) {
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace() {
                self.pos += 1;
            } else if byte == b'%' {
                while let Some(next) = self.peek() {
                    if next == b'\n' || next == b'\r' {
                        break;
                    }
                    self.pos += 1;
                }
            } else {
                break;
            }
        }
    }

    fn read_token(&mut self) -> Result<Vec<u8>, ParseError> {
        self.skip_ws();
        let start = self.pos;
        while let Some(byte) = self.peek() {
            if byte.is_ascii_whitespace()
                || matches!(
                    byte,
                    b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%'
                )
            {
                break;
            }
            self.pos += 1;
        }
        if start == self.pos {
            return Err(ParseError::UnexpectedToken("empty token".to_owned()));
        }
        Ok(self.data[start..self.pos].to_vec())
    }
}

fn parse_value(cursor: &mut Cursor<'_>, depth: usize) -> Result<PdfValue, ParseError> {
    if depth > MAX_DEPTH {
        return Err(ParseError::NestingTooDeep);
    }
    cursor.skip_ws();
    let byte = cursor.peek().ok_or(ParseError::UnexpectedEof("value"))?;
    match byte {
        b'/' => {
            cursor.pos += 1;
            let raw = cursor.read_token()?;
            Ok(PdfValue::Name(decode_name(&raw)))
        }
        b'(' => parse_string(cursor),
        b'[' => {
            cursor.pos += 1;
            let mut items = Vec::new();
            loop {
                cursor.skip_ws();
                if cursor.peek() == Some(b']') {
                    cursor.pos += 1;
                    break;
                }
                if cursor.at_end() {
                    return Err(ParseError::UnexpectedEof("array"));
                }
                items.push(parse_value(cursor, depth + 1)?);
            }
            Ok(PdfValue::Array(items))
        }
        b'<' if cursor.data.get(cursor.pos + 1) == Some(&b'<') => {
            cursor.pos += 2;
            let mut entries = BTreeMap::new();
            loop {
                cursor.skip_ws();
                if cursor.data.get(cursor.pos) == Some(&b'>')
                    && cursor.data.get(cursor.pos + 1) == Some(&b'>')
                {
                    cursor.pos += 2;
                    break;
                }
                if cursor.peek() != Some(b'/') {
                    return Err(ParseError::UnexpectedToken(
                        "dictionary key is not a name".to_owned(),
                    ));
                }
                let key_value = parse_value(cursor, depth + 1)?;
                let key = key_value.as_name().ok_or_else(|| {
                    ParseError::UnexpectedToken("dictionary key is not a name".to_owned())
                })?;
                let key = key.to_owned();
                let value = parse_value(cursor, depth + 1)?;
                entries.insert(key, value);
            }
            let saved = cursor.pos;
            cursor.skip_ws();
            let is_stream = cursor
                .read_token()
                .is_ok_and(|token| token.as_slice() == b"stream".as_slice());
            if !is_stream {
                cursor.pos = saved;
                return Ok(PdfValue::Dictionary(entries));
            }
            if cursor.peek() == Some(b'\r') {
                cursor.pos += 1;
            }
            if cursor.peek() == Some(b'\n') {
                cursor.pos += 1;
            }
            let length = entries
                .get("Length")
                .and_then(PdfValue::as_i64)
                .and_then(|value| usize::try_from(value).ok())
                .ok_or_else(|| ParseError::MalformedXref("stream has no /Length".to_owned()))?;
            let end = cursor
                .pos
                .checked_add(length)
                .ok_or_else(|| ParseError::MalformedXref("stream length overflow".to_owned()))?;
            if end > cursor.data.len() {
                return Err(ParseError::UnexpectedEof("stream data"));
            }
            let data = cursor.data[cursor.pos..end].to_vec();
            cursor.pos = end;
            cursor.skip_ws();
            if !cursor
                .read_token()
                .is_ok_and(|token| token.as_slice() == b"endstream".as_slice())
            {
                return Err(ParseError::UnexpectedToken("missing endstream".to_owned()));
            }
            Ok(PdfValue::Stream {
                dict: entries,
                data,
            })
        }
        b'0'..=b'9' | b'+' | b'-' | b'.' => parse_number_or_reference(cursor),
        b't' if cursor.data.get(cursor.pos..cursor.pos + 4) == Some(b"true".as_slice()) => {
            cursor.pos += 4;
            Ok(PdfValue::Bool(true))
        }
        b'f' if cursor.data.get(cursor.pos..cursor.pos + 5) == Some(b"false".as_slice()) => {
            cursor.pos += 5;
            Ok(PdfValue::Bool(false))
        }
        b'n' if cursor.data.get(cursor.pos..cursor.pos + 4) == Some(b"null".as_slice()) => {
            cursor.pos += 4;
            Ok(PdfValue::Null)
        }
        _ => Err(ParseError::UnexpectedToken(format!(
            "unexpected byte 0x{byte:02X}"
        ))),
    }
}

fn parse_number_or_reference(cursor: &mut Cursor<'_>) -> Result<PdfValue, ParseError> {
    let first = cursor.read_token()?;
    let first_text = std::str::from_utf8(&first)
        .map_err(|_| ParseError::UnexpectedToken("non-UTF-8 number".to_owned()))?;
    let number = first_text
        .parse::<f64>()
        .map_err(|_| ParseError::UnexpectedToken(format!("invalid number '{first_text}'")))?;
    if !number.is_finite() {
        return Err(ParseError::NonFiniteNumber);
    }

    let saved = cursor.pos;
    if let Ok(generation_token) = cursor.read_token()
        && let Some(generation) = std::str::from_utf8(&generation_token)
            .ok()
            .and_then(|text| text.parse::<u16>().ok())
        && cursor
            .read_token()
            .is_ok_and(|token| token.as_slice() == b"R".as_slice())
    {
        let object_number = first_text
            .parse::<u32>()
            .map_err(|_| ParseError::UnexpectedToken("invalid reference number".to_owned()))?;
        return Ok(PdfValue::Reference(ObjectRef::new(
            object_number,
            generation,
        )));
    }
    cursor.pos = saved;
    Ok(PdfValue::Number(number))
}

fn parse_string(cursor: &mut Cursor<'_>) -> Result<PdfValue, ParseError> {
    cursor.pos += 1;
    let mut bytes = Vec::new();
    let mut depth = 1usize;
    while let Some(byte) = cursor.peek() {
        cursor.pos += 1;
        match byte {
            b'\\' => {
                let escape = cursor
                    .peek()
                    .ok_or(ParseError::UnexpectedEof("string escape"))?;
                cursor.pos += 1;
                match escape {
                    b'n' => bytes.push(b'\n'),
                    b'r' => bytes.push(b'\r'),
                    b't' => bytes.push(b'\t'),
                    b'b' => bytes.push(0x08),
                    b'f' => bytes.push(0x0c),
                    b'(' | b')' | b'\\' => bytes.push(escape),
                    b'\n' => {}
                    b'\r' => {
                        if cursor.peek() == Some(b'\n') {
                            cursor.pos += 1;
                        }
                    }
                    b'0'..=b'7' => {
                        let mut value = u16::from(escape - b'0');
                        for _ in 0..2 {
                            match cursor.peek() {
                                Some(digit @ b'0'..=b'7') => {
                                    value = value * 8 + u16::from(digit - b'0');
                                    cursor.pos += 1;
                                }
                                _ => break,
                            }
                        }
                        bytes.push(value as u8);
                    }
                    other => bytes.push(other),
                }
            }
            b'(' => {
                depth += 1;
                bytes.push(byte);
            }
            b')' => {
                depth -= 1;
                if depth == 0 {
                    return Ok(PdfValue::Text(String::from_utf8_lossy(&bytes).into_owned()));
                }
                bytes.push(byte);
            }
            other => bytes.push(other),
        }
    }
    Err(ParseError::UnexpectedEof("string"))
}

fn decode_name(raw: &[u8]) -> String {
    let mut decoded = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'#'
            && index + 2 < raw.len()
            && let Some(high) = hex_digit(raw[index + 1])
            && let Some(low) = hex_digit(raw[index + 2])
        {
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&decoded).into_owned()
}

fn hex_digit(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::WidgetRect;
    use crate::build::{FieldSpec, acroform_dictionary};

    fn fields() -> Vec<PdfValue> {
        let specs = [
            FieldSpec::text("order.name", WidgetRect::new(10.0, 20.0, 100.0, 24.0), 5)
                .with_tooltip("Customer name"),
            FieldSpec::checkbox("agree", WidgetRect::new(10.0, 60.0, 14.0, 14.0), 5),
        ];
        let form = acroform_dictionary(&specs).unwrap();
        form.as_dict()
            .unwrap()
            .get("Fields")
            .and_then(PdfValue::as_array)
            .unwrap()
            .to_vec()
    }

    fn resolve_dict(document: &PdfDocument, value: &PdfValue) -> BTreeMap<String, PdfValue> {
        document.resolve(value).unwrap().as_dict().unwrap().clone()
    }

    #[test]
    fn serialize_round_trip_verifies_names_types_and_placement() {
        let page = ObjectRef::new(5, 0);
        let bytes = serialize_form_pdf(&fields(), &[], Some(page)).unwrap();
        let document = parse_pdf(&bytes).unwrap();
        let catalog = document.catalog().unwrap();
        let acroform = resolve_dict(&document, catalog.get("AcroForm").unwrap());
        let field_refs = acroform.get("Fields").unwrap().as_array().unwrap();
        assert_eq!(field_refs.len(), 2);

        let names: Vec<String> = field_refs
            .iter()
            .map(|reference| {
                let dict = resolve_dict(&document, reference);
                dict.get("T").unwrap().as_text().unwrap().to_owned()
            })
            .collect();
        assert_eq!(names, ["order.name", "agree"]);
        let types: Vec<String> = field_refs
            .iter()
            .map(|reference| {
                let dict = resolve_dict(&document, reference);
                dict.get("FT").unwrap().as_name().unwrap().to_owned()
            })
            .collect();
        assert_eq!(types, ["Tx", "Btn"]);

        let first = resolve_dict(&document, &field_refs[0]);
        let widget_reference = &first.get("Kids").unwrap().as_array().unwrap()[0];
        let widget = resolve_dict(&document, widget_reference);
        assert_eq!(widget.get("Subtype").unwrap().as_name(), Some("Widget"));
        assert_eq!(
            widget.get("Rect").unwrap().as_array().unwrap(),
            &vec![
                PdfValue::Number(10.0),
                PdfValue::Number(20.0),
                PdfValue::Number(110.0),
                PdfValue::Number(44.0),
            ]
        );
        assert_eq!(widget.get("P").unwrap().as_reference(), Some(page));
        assert_eq!(document.root.number, 1);
    }

    #[test]
    fn page_annots_reference_all_widgets() {
        let page = ObjectRef::new(42, 0);
        let bytes = serialize_form_pdf(&fields(), &[], Some(page)).unwrap();
        let document = parse_pdf(&bytes).unwrap();
        let page_object = document.resolve(&PdfValue::Reference(page)).unwrap();
        let page_dict = page_object.as_dict().unwrap();
        assert_eq!(page_dict.get("Type").unwrap().as_name(), Some("Page"));
        let annots = page_dict.get("Annots").unwrap().as_array().unwrap();
        assert_eq!(annots.len(), 2);
        for reference in annots {
            let widget = document.resolve(reference).unwrap();
            assert_eq!(
                widget.as_dict().unwrap().get("Subtype").unwrap().as_name(),
                Some("Widget")
            );
        }
        let pages = resolve_dict(&document, page_dict.get("Parent").unwrap());
        assert_eq!(pages.get("Type").unwrap().as_name(), Some("Pages"));
        assert_eq!(
            pages.get("Kids").unwrap().as_array().unwrap(),
            &vec![PdfValue::Reference(page)]
        );
    }

    #[test]
    fn acroform_fields_reference_field_objects() {
        let page = ObjectRef::new(3, 0);
        let bytes = serialize_form_pdf(&fields(), &[], Some(page)).unwrap();
        let document = parse_pdf(&bytes).unwrap();
        let catalog = document.catalog().unwrap();
        assert_eq!(catalog.get("Type").unwrap().as_name(), Some("Catalog"));
        let acroform = resolve_dict(&document, catalog.get("AcroForm").unwrap());
        assert_eq!(
            acroform.get("NeedAppearances").unwrap().as_bool(),
            Some(false)
        );
        let field_refs = acroform.get("Fields").unwrap().as_array().unwrap();
        for reference in field_refs {
            let field = document.resolve(reference).unwrap();
            let dict = field.as_dict().unwrap();
            assert!(
                dict.get("T")
                    .unwrap()
                    .as_text()
                    .is_some_and(|n| !n.is_empty())
            );
            assert!(dict.get("FT").unwrap().as_name().is_some());
            assert_eq!(dict.get("Kids").unwrap().as_array().unwrap().len(), 1);
        }
    }

    #[test]
    fn missing_page_reference_is_a_typed_error() {
        assert_eq!(
            serialize_form_pdf(&fields(), &[], None),
            Err(SerializeError::MissingPageReference)
        );
    }

    #[test]
    fn invalid_page_reference_is_rejected() {
        let result = serialize_form_pdf(&fields(), &[], Some(ObjectRef::new(0, 0)));
        assert_eq!(
            result,
            Err(SerializeError::InvalidPageReference(ObjectRef::new(0, 0)))
        );
        let too_large = ObjectRef::new(MAX_OBJECT_NUMBER + 1, 0);
        assert!(matches!(
            serialize_form_pdf(&fields(), &[], Some(too_large)),
            Err(SerializeError::InvalidPageReference(_))
        ));
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        let mut duplicated = fields();
        duplicated.push(fields()[0].clone());
        assert_eq!(
            serialize_form_pdf(&duplicated, &[], Some(ObjectRef::new(3, 0))),
            Err(SerializeError::DuplicateFieldName("order.name".to_owned()))
        );
    }

    #[test]
    fn malformed_field_dictionary_is_rejected() {
        let page = Some(ObjectRef::new(3, 0));
        assert!(matches!(
            serialize_form_pdf(&[PdfValue::Number(1.0)], &[], page),
            Err(SerializeError::MalformedDictionary { .. })
        ));
        assert!(matches!(
            serialize_form_pdf(&[PdfValue::Dictionary(BTreeMap::new())], &[], page),
            Err(SerializeError::MalformedDictionary { .. })
        ));
        let mut no_type = BTreeMap::new();
        no_type.insert("T".to_owned(), PdfValue::Text("x".to_owned()));
        assert!(matches!(
            serialize_form_pdf(&[PdfValue::Dictionary(no_type)], &[], page),
            Err(SerializeError::MalformedDictionary { .. })
        ));
    }

    #[test]
    fn empty_field_list_is_rejected() {
        assert!(matches!(
            serialize_form_pdf(&[], &[], Some(ObjectRef::new(3, 0))),
            Err(SerializeError::MalformedDictionary { .. })
        ));
    }

    #[test]
    fn widget_count_mismatch_is_rejected() {
        let fields = fields();
        let widget = fields[0]
            .as_dict()
            .unwrap()
            .get("Kids")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .clone();
        assert_eq!(
            serialize_form_pdf(&fields, &[widget], Some(ObjectRef::new(3, 0))),
            Err(SerializeError::LengthMismatch {
                fields: 2,
                widgets: 1
            })
        );
    }

    #[test]
    fn explicit_widgets_override_embedded_kids() {
        let fields = fields();
        let mut widget = fields[0]
            .as_dict()
            .unwrap()
            .get("Kids")
            .unwrap()
            .as_array()
            .unwrap()[0]
            .as_dict()
            .unwrap()
            .clone();
        widget.insert(
            "Rect".to_owned(),
            PdfValue::Array(vec![PdfValue::Number(1.0)]),
        );
        let widgets = vec![
            PdfValue::Dictionary(widget),
            fields[1]
                .as_dict()
                .unwrap()
                .get("Kids")
                .unwrap()
                .as_array()
                .unwrap()[0]
                .clone(),
        ];
        let page = ObjectRef::new(3, 0);
        let bytes = serialize_form_pdf(&fields, &widgets, Some(page)).unwrap();
        let document = parse_pdf(&bytes).unwrap();
        let catalog = document.catalog().unwrap();
        let acroform = resolve_dict(&document, catalog.get("AcroForm").unwrap());
        let first_ref = &acroform.get("Fields").unwrap().as_array().unwrap()[0];
        let first = resolve_dict(&document, first_ref);
        let widget_ref = &first.get("Kids").unwrap().as_array().unwrap()[0];
        let emitted = resolve_dict(&document, widget_ref);
        assert_eq!(
            emitted.get("Rect").unwrap().as_array().unwrap(),
            &vec![PdfValue::Number(1.0)]
        );
        assert_eq!(emitted.get("P").unwrap().as_reference(), Some(page));
    }

    #[test]
    fn serialization_is_deterministic() {
        let first = serialize_form_pdf(&fields(), &[], Some(ObjectRef::new(7, 0))).unwrap();
        let second = serialize_form_pdf(&fields(), &[], Some(ObjectRef::new(7, 0))).unwrap();
        assert_eq!(first, second);
        assert!(first.starts_with(b"%PDF-1.7\n"));
        assert!(first.ends_with(b"\n%%EOF\n"));
    }

    #[test]
    fn parser_rejects_non_pdf_input() {
        assert_eq!(parse_pdf(b"not a pdf"), Err(ParseError::MissingHeader));
        assert_eq!(
            parse_pdf(b"%PDF-1.7\nno xref here"),
            Err(ParseError::MissingStartXref)
        );
    }
}
