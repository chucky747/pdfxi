//! Document metadata: the classic `/Info` dictionary and XMP packets.
//!
//! ISO 32000 defines two complementary places for document properties. The
//! older one is the trailer's `/Info` entry, an indirect reference to a
//! dictionary holding eight standard text-string keys. The newer one is the
//! catalog's `/Metadata` entry, an indirect reference to a stream whose payload
//! is an XMP packet (RDF/XML). This module reads and writes both without any UI
//! or rendering dependency.
//!
//! # Text strings
//!
//! PDF text strings are byte sequences whose encoding is determined by a BOM:
//! a leading `FE FF` means UTF-16BE, everything else is interpreted as
//! PDFDocEncoding (ISO 32000-1 Annex D). [`encode_text_string`] and
//! [`decode_text_string`] implement that rule. Encoding chooses PDFDocEncoding
//! when every character is representable and UTF-16BE otherwise, so decoding
//! an encoded string always yields the original text.
//!
//! # Dates
//!
//! [`PdfDate`] keeps the exact raw string alongside best-effort parsed fields.
//! The strict PDF form is `D:YYYYMMDDHHmmSSOHH'mm'` (the `D:` prefix and all
//! components after the year are optional). Values that do not match that form
//! are parsed with a relaxed digit-extraction fallback and are flagged with
//! [`PdfDate::well_formed`] set to `false`; their raw bytes are never discarded.
//!
//! # XMP
//!
//! [`parse_xmp`] is a deliberately small, namespace-prefix-tolerant RDF/XML
//! reader. It matches properties by *local* name, so `dc:title`,
//! `dcterms:title` and `<title>` are equivalent. It is not a general XML
//! library: it supports elements, attributes, comments, processing
//! instructions, CDATA and the five predefined entities plus numeric character
//! references, and rejects malformed input with [`Error::MalformedXmp`] rather
//! than panicking. [`XmpMetadata::to_xml`] emits a deterministic packet with a
//! fixed, documented layout so repeated writes are byte-stable.

use crate::object::{Dictionary, Object, ObjectId};
use crate::{Document, Error, Result};

/// Byte-order mark that identifies a UTF-16BE PDF text string.
const UTF16BE_BOM: [u8; 2] = [0xFE, 0xFF];
/// Upper bound on an XMP packet accepted by [`parse_xmp`].
///
/// The stream is already bounded by the decoder cap, but XMP is small by
/// nature; an eight-megabyte packet is far beyond any real document and keeps
/// the parser's allocations bounded.
const MAX_XMP_BYTES: usize = 8 * 1024 * 1024;
/// Maximum element nesting accepted by the XMP XML parser.
const MAX_XML_DEPTH: usize = 64;

// ---------------------------------------------------------------------------
// PDF text strings
// ---------------------------------------------------------------------------

/// Decode a PDF text string into a Rust string.
///
/// A leading `FE FF` byte-order mark selects UTF-16BE; all other byte sequences
/// are decoded as PDFDocEncoding (ISO 32000-1 Annex D). Unpaired UTF-16
/// surrogates and odd trailing bytes are replaced with U+FFFD so hostile input
/// can never cause a panic.
pub fn decode_text_string(bytes: &[u8]) -> String {
    if bytes.starts_with(&UTF16BE_BOM) {
        decode_utf16_be(&bytes[UTF16BE_BOM.len()..])
    } else {
        bytes.iter().copied().map(pdfdoc_decode).collect()
    }
}

/// Encode a Rust string as a PDF text string.
///
/// When every character is representable in PDFDocEncoding the compact
/// single-byte encoding is used. Otherwise the result is UTF-16BE prefixed with
/// the `FE FF` BOM. The returned bytes are suitable for an
/// [`Object::String`]; the writer serializes such
/// objects as hexadecimal strings, which needs no further escaping.
pub fn encode_text_string(text: &str) -> Vec<u8> {
    if text.chars().all(|ch| pdfdoc_encode(ch).is_some()) {
        text.chars().filter_map(pdfdoc_encode).collect()
    } else {
        let mut out = Vec::with_capacity(UTF16BE_BOM.len() + text.len() * 2);
        out.extend_from_slice(&UTF16BE_BOM);
        for unit in text.encode_utf16() {
            out.extend_from_slice(&unit.to_be_bytes());
        }
        out
    }
}

/// Decode big-endian UTF-16 code units, tolerating malformed sequences.
fn decode_utf16_be(bytes: &[u8]) -> String {
    let mut units = Vec::with_capacity(bytes.len() / 2 + 1);
    let mut position = 0usize;
    while position + 1 < bytes.len() {
        units.push(u16::from_be_bytes([bytes[position], bytes[position + 1]]));
        position += 2;
    }
    if position < bytes.len() {
        // An odd trailing byte is treated as a single PDFDocEncoding character
        // lifted into the BMP. This is a tolerant form, not a valid UTF-16
        // string, but keeps decoding total for hostile files.
        let ch = pdfdoc_decode(bytes[position]);
        let mut buffer = [0u16; 2];
        units.extend_from_slice(ch.encode_utf16(&mut buffer));
    }
    char::decode_utf16(units)
        .map(|decoded| decoded.unwrap_or('\u{FFFD}'))
        .collect()
}

/// Map one byte to its Unicode code point under PDFDocEncoding.
fn pdfdoc_decode(byte: u8) -> char {
    match byte {
        0x18 => '\u{02D8}',
        0x19 => '\u{02C7}',
        0x1A => '\u{02C6}',
        0x1B => '\u{02D9}',
        0x1C => '\u{02DD}',
        0x1D => '\u{02DB}',
        0x1E => '\u{02DA}',
        0x1F => '\u{02DC}',
        0x80 => '\u{2022}',
        0x81 => '\u{2020}',
        0x82 => '\u{2021}',
        0x83 => '\u{2026}',
        0x84 => '\u{2014}',
        0x85 => '\u{2013}',
        0x86 => '\u{0192}',
        0x87 => '\u{2044}',
        0x88 => '\u{2039}',
        0x89 => '\u{203A}',
        0x8A => '\u{2212}',
        0x8B => '\u{2030}',
        0x8C => '\u{201E}',
        0x8D => '\u{201C}',
        0x8E => '\u{201D}',
        0x8F => '\u{2018}',
        0x90 => '\u{2019}',
        0x91 => '\u{201A}',
        0x92 => '\u{2122}',
        0x93 => '\u{FB01}',
        0x94 => '\u{FB02}',
        0x95 => '\u{0141}',
        0x96 => '\u{0152}',
        0x97 => '\u{0160}',
        0x98 => '\u{0178}',
        0x99 => '\u{017D}',
        0x9A => '\u{0131}',
        0x9B => '\u{0142}',
        0x9C => '\u{0153}',
        0x9D => '\u{0161}',
        0x9E => '\u{017E}',
        0x9F => '\u{FFFD}',
        0xA0 => '\u{20AC}',
        other => char::from_u32(u32::from(other)).unwrap_or('\u{FFFD}'),
    }
}

/// Return the PDFDocEncoding byte for `ch`, if one exists.
fn pdfdoc_encode(ch: char) -> Option<u8> {
    if let Ok(byte) = u8::try_from(u32::from(ch))
        && pdfdoc_decode(byte) == ch
    {
        return Some(byte);
    }
    (0u8..=0xFF).find(|&candidate| pdfdoc_decode(candidate) == ch)
}

// ---------------------------------------------------------------------------
// Dates
// ---------------------------------------------------------------------------

/// A parsed PDF date, preserving the original raw string.
///
/// The strict grammar is `D:YYYYMMDDHHmmSSOHH'mm'` where `O` is `+`, `-` or
/// `Z`. The `D:` prefix and every component after the year may be omitted.
/// Values that do not fit that grammar are parsed with a relaxed fallback that
/// extracts digit groups; in that case [`PdfDate::well_formed`] is `false` and
/// the parsed fields are best-effort. The [`PdfDate::raw`] field always holds
/// the exact decoded source string and is what gets written back to the file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfDate {
    /// The exact source text of the date.
    pub raw: String,
    /// Four-digit year, when at least four digits were present.
    pub year: Option<i32>,
    /// Month (1-12), when present.
    pub month: Option<u32>,
    /// Day of month (1-31), when present.
    pub day: Option<u32>,
    /// Hour (0-23), when present.
    pub hour: Option<u32>,
    /// Minute (0-59), when present.
    pub minute: Option<u32>,
    /// Second (0-59), when present.
    pub second: Option<u32>,
    /// Offset from UTC in minutes east of UTC; `Some(0)` for a trailing `Z`.
    pub utc_offset_minutes: Option<i32>,
    /// `true` only when the source matched the strict PDF date grammar fully.
    pub well_formed: bool,
}

/// Internal, fully parsed date before the raw string is attached.
#[derive(Debug, Clone, Copy)]
struct ParsedDate {
    year: i32,
    month: Option<u32>,
    day: Option<u32>,
    hour: Option<u32>,
    minute: Option<u32>,
    second: Option<u32>,
    utc_offset_minutes: Option<i32>,
}

impl ParsedDate {
    fn new(year: i32) -> Self {
        Self {
            year,
            month: None,
            day: None,
            hour: None,
            minute: None,
            second: None,
            utc_offset_minutes: None,
        }
    }
}

/// Parse a PDF date string with tolerant fallbacks.
///
/// Strictly formatted values set [`PdfDate::well_formed`]; malformed values
/// keep their raw text and receive best-effort numeric fields extracted from
/// digit runs. No input can cause a panic.
pub fn parse_pdf_date(raw: &str) -> PdfDate {
    let mut result = PdfDate {
        raw: raw.to_owned(),
        year: None,
        month: None,
        day: None,
        hour: None,
        minute: None,
        second: None,
        utc_offset_minutes: None,
        well_formed: false,
    };
    let trimmed = raw.trim();
    let body = trimmed
        .strip_prefix("D:")
        .or_else(|| trimmed.strip_prefix("d:"))
        .unwrap_or(trimmed);
    if let Some(parsed) = parse_strict_date(body) {
        copy_parsed(&mut result, parsed);
        result.well_formed = true;
        return result;
    }
    if let Some(parsed) = parse_relaxed_date(body) {
        copy_parsed(&mut result, parsed);
    }
    result
}

/// Copy parsed fields into the public result type.
fn copy_parsed(target: &mut PdfDate, parsed: ParsedDate) {
    target.year = Some(parsed.year);
    target.month = parsed.month;
    target.day = parsed.day;
    target.hour = parsed.hour;
    target.minute = parsed.minute;
    target.second = parsed.second;
    target.utc_offset_minutes = parsed.utc_offset_minutes;
}

/// Parse the strict `YYYYMMDDHHmmSSOHH'mm'` grammar after the optional `D:`.
fn parse_strict_date(body: &str) -> Option<ParsedDate> {
    let bytes = body.as_bytes();
    let mut position = 0usize;
    let year = take_digits(bytes, &mut position, 4)? as i32;

    let mut groups = Vec::new();
    while groups.len() < 5
        && position + 2 <= bytes.len()
        && bytes[position].is_ascii_digit()
        && bytes[position + 1].is_ascii_digit()
    {
        groups.push(take_digits(bytes, &mut position, 2)?);
    }

    let mut offset = None;
    if position < bytes.len() {
        if bytes.get(position) == Some(&b'Z') && position + 1 == bytes.len() {
            offset = Some(0);
            position += 1;
        } else if matches!(bytes.get(position), Some(b'+') | Some(b'-')) {
            let sign = if bytes[position] == b'+' { 1i32 } else { -1i32 };
            position += 1;
            let hours = take_digits(bytes, &mut position, 2)?;
            if bytes.get(position) != Some(&b'\'') {
                return None;
            }
            position += 1;
            let minutes = take_digits(bytes, &mut position, 2)?;
            if bytes.get(position) != Some(&b'\'') {
                return None;
            }
            position += 1;
            offset = Some(sign * (hours as i32 * 60 + minutes as i32));
        } else {
            return None;
        }
    }
    if position != bytes.len() {
        return None;
    }

    let mut parsed = ParsedDate::new(year);
    apply_groups(&mut parsed, &groups);
    parsed.utc_offset_minutes = offset;
    Some(parsed)
}

/// Best-effort fallback: concatenate all ASCII digits and read groups of two.
fn parse_relaxed_date(body: &str) -> Option<ParsedDate> {
    let digits: String = body.chars().filter(|ch| ch.is_ascii_digit()).collect();
    if digits.len() < 4 {
        return None;
    }
    let year = digits[..4].parse::<i32>().ok()?;
    let rest = &digits.as_bytes()[4..];
    let mut groups = Vec::new();
    let mut position = 0usize;
    while position + 2 <= rest.len() {
        groups.push(u32::from(rest[position] - b'0') * 10 + u32::from(rest[position + 1] - b'0'));
        position += 2;
    }
    let mut parsed = ParsedDate::new(year);
    apply_groups(&mut parsed, &groups);
    Some(parsed)
}

/// Fill month through second from consecutive two-digit groups.
fn apply_groups(target: &mut ParsedDate, groups: &[u32]) {
    let mut values = groups.iter().copied();
    if let Some(value) = values.next() {
        target.month = Some(value);
    }
    if let Some(value) = values.next() {
        target.day = Some(value);
    }
    if let Some(value) = values.next() {
        target.hour = Some(value);
    }
    if let Some(value) = values.next() {
        target.minute = Some(value);
    }
    if let Some(value) = values.next() {
        target.second = Some(value);
    }
}

/// Consume exactly `count` ASCII digits, advancing `position` on success.
fn take_digits(bytes: &[u8], position: &mut usize, count: usize) -> Option<u32> {
    let end = position.checked_add(count)?;
    let slice = bytes.get(*position..end)?;
    if !slice.iter().all(u8::is_ascii_digit) {
        return None;
    }
    let text = std::str::from_utf8(slice).ok()?;
    let value = text.parse::<u32>().ok()?;
    *position = end;
    Some(value)
}

// ---------------------------------------------------------------------------
// Classic Info dictionary
// ---------------------------------------------------------------------------

/// The eight standard keys of the trailer's `/Info` dictionary.
///
/// Missing or non-string entries become `None`. Dates keep their raw text in a
/// [`PdfDate`]; see [`parse_pdf_date`] for the tolerant parsing rules.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct DocumentInfo {
    /// Document title (`/Title`).
    pub title: Option<String>,
    /// Author (`/Author`).
    pub author: Option<String>,
    /// Subject (`/Subject`).
    pub subject: Option<String>,
    /// Search keywords (`/Keywords`).
    pub keywords: Option<String>,
    /// Application that created the original document (`/Creator`).
    pub creator: Option<String>,
    /// Application that produced this PDF (`/Producer`).
    pub producer: Option<String>,
    /// Creation date (`/CreationDate`).
    pub creation_date: Option<PdfDate>,
    /// Most recent modification date (`/ModDate`).
    pub modification_date: Option<PdfDate>,
}

impl DocumentInfo {
    /// Read the standard keys from a decoded Info dictionary.
    ///
    /// Only string values are read; a key with any other object type is treated
    /// as absent. This is deliberately tolerant of malformed metadata.
    pub fn from_dictionary(dictionary: &Dictionary) -> Self {
        Self {
            title: read_text(dictionary, "Title"),
            author: read_text(dictionary, "Author"),
            subject: read_text(dictionary, "Subject"),
            keywords: read_text(dictionary, "Keywords"),
            creator: read_text(dictionary, "Creator"),
            producer: read_text(dictionary, "Producer"),
            creation_date: read_date(dictionary, "CreationDate"),
            modification_date: read_date(dictionary, "ModDate"),
        }
    }

    /// Build an Info dictionary object suitable for [`Document::set_object`].
    ///
    /// Text values are encoded with [`encode_text_string`]; dates are written
    /// from their [`PdfDate::raw`] text so that even malformed values survive a
    /// save/reparse cycle unchanged.
    pub fn to_object(&self) -> Object {
        let mut dictionary = Dictionary::new();
        insert_text(&mut dictionary, "Title", self.title.as_deref());
        insert_text(&mut dictionary, "Author", self.author.as_deref());
        insert_text(&mut dictionary, "Subject", self.subject.as_deref());
        insert_text(&mut dictionary, "Keywords", self.keywords.as_deref());
        insert_text(&mut dictionary, "Creator", self.creator.as_deref());
        insert_text(&mut dictionary, "Producer", self.producer.as_deref());
        if let Some(date) = &self.creation_date {
            dictionary.insert(
                "CreationDate".to_owned(),
                Object::String(encode_text_string(&date.raw)),
            );
        }
        if let Some(date) = &self.modification_date {
            dictionary.insert(
                "ModDate".to_owned(),
                Object::String(encode_text_string(&date.raw)),
            );
        }
        Object::Dictionary(dictionary)
    }
}

/// Read and decode one text-string key, returning `None` when it is absent.
fn read_text(dictionary: &Dictionary, key: &str) -> Option<String> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Some(decode_text_string(bytes)),
        _ => None,
    }
}

/// Read and parse one date key, returning `None` when it is absent.
fn read_date(dictionary: &Dictionary, key: &str) -> Option<PdfDate> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Some(parse_pdf_date(&decode_text_string(bytes))),
        _ => None,
    }
}

/// Insert one text-string key into a dictionary when the value is present.
fn insert_text(dictionary: &mut Dictionary, key: &str, value: Option<&str>) {
    if let Some(value) = value {
        dictionary.insert(key.to_owned(), Object::String(encode_text_string(value)));
    }
}

// ---------------------------------------------------------------------------
// XMP model
// ---------------------------------------------------------------------------

/// The subset of XMP properties exposed by this crate.
///
/// Every field is stored as a string. For structured properties the parser
/// concatenates the `rdf:li` text items with `", "`. [`XmpMetadata::to_xml`]
/// reverses the subject join when writing a Bag.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmpMetadata {
    /// `dc:title` (prefers the `x-default` alternative when present).
    pub title: Option<String>,
    /// `dc:creator`, joined from an `rdf:Seq` when structured.
    pub creator: Option<String>,
    /// `dc:description` (prefers the `x-default` alternative).
    pub description: Option<String>,
    /// `dc:subject`, joined from an `rdf:Bag` with `", "`.
    pub subject: Option<String>,
    /// `pdf:Producer`.
    pub producer: Option<String>,
    /// `xmp:CreateDate`.
    pub create_date: Option<String>,
    /// `xmp:ModifyDate`.
    pub modify_date: Option<String>,
}

impl XmpMetadata {
    /// Serialize this metadata into a deterministic XMP packet.
    ///
    /// The layout is fixed (no timestamps or random identifiers): an
    /// `<?xpacket?>` header, one `<x:xmpmeta>` containing one `<rdf:RDF>` with a
    /// single `<rdf:Description>`, then a `<?xpacket end="w"?>` trailer.
    /// Present fields are emitted in the order title, creator, description,
    /// subject, producer, create date, modify date. Two-space indentation is
    /// used throughout, so repeated writes are byte-identical.
    pub fn to_xml(&self) -> String {
        let mut out = String::new();
        out.push_str("<?xpacket begin=\"&#xFEFF;\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n");
        out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n");
        out.push_str("  <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
        out.push_str("    <rdf:Description rdf:about=\"\"\n");
        out.push_str("        xmlns:dc=\"http://purl.org/dc/elements/1.1/\"\n");
        out.push_str("        xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\"\n");
        out.push_str("        xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">\n");
        if let Some(value) = &self.title {
            out.push_str(&alt_element("dc:title", value));
        }
        if let Some(value) = &self.creator {
            out.push_str(&sequence_element("dc:creator", value, false));
        }
        if let Some(value) = &self.description {
            out.push_str(&alt_element("dc:description", value));
        }
        if let Some(value) = &self.subject {
            out.push_str(&sequence_element("dc:subject", value, true));
        }
        if let Some(value) = &self.producer {
            out.push_str(&simple_element("pdf:Producer", value));
        }
        if let Some(value) = &self.create_date {
            out.push_str(&simple_element("xmp:CreateDate", value));
        }
        if let Some(value) = &self.modify_date {
            out.push_str(&simple_element("xmp:ModifyDate", value));
        }
        out.push_str("    </rdf:Description>\n");
        out.push_str("  </rdf:RDF>\n");
        out.push_str("</x:xmpmeta>\n");
        out.push_str("<?xpacket end=\"w\"?>\n");
        out
    }

    /// Build a `/Metadata` stream object carrying this packet.
    fn to_stream_object(&self) -> Object {
        let mut dictionary = Dictionary::new();
        dictionary.insert("Type".to_owned(), Object::Name("Metadata".to_owned()));
        dictionary.insert("Subtype".to_owned(), Object::Name("XML".to_owned()));
        Object::Stream {
            dictionary,
            data: self.to_xml().into_bytes(),
        }
    }
}

/// Render a simple property element.
fn simple_element(name: &str, value: &str) -> String {
    format!("      <{name}>{}</{name}>\n", xml_escape(value))
}

/// Render an `rdf:Alt` property with one `x-default` item.
fn alt_element(name: &str, value: &str) -> String {
    format!(
        "      <{name}>\n        <rdf:Alt>\n          <rdf:li xml:lang=\"x-default\">{}</rdf:li>\n        </rdf:Alt>\n      </{name}>\n",
        xml_escape(value)
    )
}

/// Render an `rdf:Seq` or `rdf:Bag` property.
///
/// When `split` is true the value is split on commas into separate items.
fn sequence_element(name: &str, value: &str, split: bool) -> String {
    let items: Vec<&str> = if split {
        value
            .split(',')
            .map(str::trim)
            .filter(|item| !item.is_empty())
            .collect()
    } else {
        vec![value]
    };
    let kind = if split { "Bag" } else { "Seq" };
    let mut out = format!("      <{name}>\n        <rdf:{kind}>\n");
    for item in items {
        out.push_str(&format!(
            "          <rdf:li>{}</rdf:li>\n",
            xml_escape(item)
        ));
    }
    out.push_str(&format!("        </rdf:{kind}>\n      </{name}>\n"));
    out
}

/// Escape the five XML markup characters in text content.
fn xml_escape(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Minimal RDF/XML reader
// ---------------------------------------------------------------------------

/// A parsed XML element with its local name, attributes, children and text.
#[derive(Debug, Clone)]
struct XmlNode {
    local_name: String,
    #[allow(dead_code)]
    attributes: Vec<(String, String)>,
    children: Vec<XmlNode>,
    text: String,
}

/// A bounds-checked, recursive-descent XML reader.
struct XmlParser<'a> {
    source: &'a str,
    bytes: &'a [u8],
    position: usize,
}

impl<'a> XmlParser<'a> {
    fn new(source: &'a str) -> Self {
        Self {
            source,
            bytes: source.as_bytes(),
            position: 0,
        }
    }

    fn error<T>(&self, message: &str) -> Result<T> {
        Err(Error::MalformedXmp(format!(
            "{message} at byte {}",
            self.position
        )))
    }

    fn skip_whitespace(&mut self) {
        while self.position < self.bytes.len() && self.bytes[self.position].is_ascii_whitespace() {
            self.position += 1;
        }
    }

    fn starts_with(&self, pattern: &[u8]) -> bool {
        self.bytes
            .get(self.position..)
            .is_some_and(|rest| rest.starts_with(pattern))
    }

    /// Skip whitespace, processing instructions and comments.
    fn skip_misc(&mut self) -> Result<()> {
        loop {
            self.skip_whitespace();
            if self.starts_with(b"<?") {
                self.skip_processing_instruction()?;
            } else if self.starts_with(b"<!--") {
                self.skip_comment()?;
            } else {
                return Ok(());
            }
        }
    }

    fn skip_processing_instruction(&mut self) -> Result<()> {
        self.position += 2;
        let rest = &self.bytes[self.position..];
        let Some(index) = find_subslice(rest, b"?>") else {
            return self.error("unterminated processing instruction");
        };
        self.position += index + 2;
        Ok(())
    }

    fn skip_comment(&mut self) -> Result<()> {
        self.position += 4;
        let rest = &self.bytes[self.position..];
        let Some(index) = find_subslice(rest, b"-->") else {
            return self.error("unterminated XML comment");
        };
        self.position += index + 3;
        Ok(())
    }

    fn read_name(&mut self) -> Result<String> {
        let start = self.position;
        while self.position < self.bytes.len() && is_name_byte(self.bytes[self.position]) {
            self.position += 1;
        }
        if self.position == start {
            return self.error("expected an XML name");
        }
        Ok(self.source[start..self.position].to_owned())
    }

    fn parse_element(&mut self, depth: usize) -> Result<XmlNode> {
        if depth > MAX_XML_DEPTH {
            return self.error("XML nesting is too deep");
        }
        if self.bytes.get(self.position) != Some(&b'<') {
            return self.error("expected '<'");
        }
        self.position += 1;
        let full_name = self.read_name()?;
        let local_name = local_part(&full_name).to_owned();
        let mut attributes = Vec::new();

        loop {
            self.skip_whitespace();
            if self.starts_with(b"/>") {
                self.position += 2;
                return Ok(XmlNode {
                    local_name,
                    attributes,
                    children: Vec::new(),
                    text: String::new(),
                });
            }
            if self.bytes.get(self.position) == Some(&b'>') {
                self.position += 1;
                break;
            }
            let attribute_name = self.read_name()?;
            self.skip_whitespace();
            if self.bytes.get(self.position) != Some(&b'=') {
                return self.error("expected '=' after an attribute name");
            }
            self.position += 1;
            self.skip_whitespace();
            let Some(quote) = self.bytes.get(self.position).copied() else {
                return self.error("unterminated attribute");
            };
            if quote != b'"' && quote != b'\'' {
                return self.error("attribute values must be quoted");
            }
            self.position += 1;
            let start = self.position;
            while self.position < self.bytes.len() && self.bytes[self.position] != quote {
                self.position += 1;
            }
            if self.position >= self.bytes.len() {
                return self.error("unterminated attribute value");
            }
            let raw_value = &self.source[start..self.position];
            self.position += 1;
            attributes.push((attribute_name, decode_entities(raw_value)?));
        }

        let mut node = XmlNode {
            local_name,
            attributes,
            children: Vec::new(),
            text: String::new(),
        };
        loop {
            if self.position >= self.bytes.len() {
                return self.error("unclosed XML element");
            }
            if self.starts_with(b"</") {
                self.position += 2;
                let closing = self.read_name()?;
                self.skip_whitespace();
                if self.bytes.get(self.position) != Some(&b'>') {
                    return self.error("expected '>' after a closing tag");
                }
                self.position += 1;
                if closing != full_name {
                    return self.error("mismatched XML closing tag");
                }
                break;
            }
            if self.starts_with(b"<!--") {
                self.skip_comment()?;
                continue;
            }
            if self.starts_with(b"<?") {
                self.skip_processing_instruction()?;
                continue;
            }
            if self.starts_with(b"<![CDATA[") {
                self.position += 9;
                let rest = &self.bytes[self.position..];
                let Some(index) = find_subslice(rest, b"]]>") else {
                    return self.error("unterminated CDATA section");
                };
                node.text
                    .push_str(&self.source[self.position..self.position + index]);
                self.position += index + 3;
                continue;
            }
            if self.bytes[self.position] == b'<' {
                let child = self.parse_element(depth + 1)?;
                node.children.push(child);
                continue;
            }
            let start = self.position;
            while self.position < self.bytes.len() && self.bytes[self.position] != b'<' {
                self.position += 1;
            }
            node.text
                .push_str(&decode_entities(&self.source[start..self.position])?);
        }
        Ok(node)
    }
}

/// Return true when a byte may appear in an XML qualified name.
fn is_name_byte(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || matches!(byte, b':' | b'_' | b'-' | b'.')
}

/// Strip the namespace prefix from a qualified XML name.
fn local_part(qualified: &str) -> &str {
    qualified
        .rsplit_once(':')
        .map_or(qualified, |(_, local)| local)
}

/// Find the first occurrence of `needle` in `haystack`.
fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Decode the five predefined XML entities and numeric character references.
fn decode_entities(raw: &str) -> Result<String> {
    if !raw.contains('&') {
        return Ok(raw.to_owned());
    }
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while let Some(index) = rest.find('&') {
        out.push_str(&rest[..index]);
        rest = &rest[index + 1..];
        let Some(end) = rest.find(';') else {
            return Err(Error::MalformedXmp(
                "unterminated XML entity reference".to_owned(),
            ));
        };
        let entity = &rest[..end];
        rest = &rest[end + 1..];
        let decoded = match entity {
            "amp" => '&',
            "lt" => '<',
            "gt" => '>',
            "quot" => '"',
            "apos" => '\'',
            _ if entity.starts_with("#x") || entity.starts_with("#X") => {
                let value = u32::from_str_radix(&entity[2..], 16).map_err(|_| {
                    Error::MalformedXmp(format!("invalid numeric XML entity &{entity};"))
                })?;
                char::from_u32(value).ok_or_else(|| {
                    Error::MalformedXmp(format!("invalid XML code point &{entity};"))
                })?
            }
            _ if entity.starts_with('#') => {
                let value = entity[1..].parse::<u32>().map_err(|_| {
                    Error::MalformedXmp(format!("invalid numeric XML entity &{entity};"))
                })?;
                char::from_u32(value).ok_or_else(|| {
                    Error::MalformedXmp(format!("invalid XML code point &{entity};"))
                })?
            }
            _ => {
                return Err(Error::MalformedXmp(format!(
                    "unknown XML entity &{entity};"
                )));
            }
        };
        out.push(decoded);
    }
    out.push_str(rest);
    Ok(out)
}

/// Parse an XMP packet from RDF/XML text.
///
/// Property elements are matched by local name, so any namespace prefix (or no
/// prefix) is accepted. Text is taken from descendant `rdf:li` items when
/// present and joined with `", "`; otherwise the element's character data is
/// used. Malformed XML yields [`Error::MalformedXmp`] and never panics.
pub fn parse_xmp(xml: &str) -> Result<XmpMetadata> {
    if xml.len() > MAX_XMP_BYTES {
        return Err(Error::MalformedXmp("XMP packet is too large".to_owned()));
    }
    let mut parser = XmlParser::new(xml);
    parser.skip_misc()?;
    if parser.position >= parser.bytes.len() {
        return Err(Error::MalformedXmp(
            "XMP packet has no root element".to_owned(),
        ));
    }
    let root = parser.parse_element(0)?;
    parser.skip_misc()?;
    if parser.position != parser.bytes.len() {
        return Err(Error::MalformedXmp(
            "trailing content after the XMP root element".to_owned(),
        ));
    }

    let mut descriptions = Vec::new();
    collect_nodes(&root, "Description", &mut descriptions);
    let mut result = XmpMetadata::default();
    for description in descriptions {
        for child in &description.children {
            let value = property_text(child);
            match child.local_name.as_str() {
                "title" => result.title = value.or(result.title),
                "creator" => result.creator = value.or(result.creator),
                "description" => result.description = value.or(result.description),
                "subject" => result.subject = value.or(result.subject),
                "Producer" => result.producer = value.or(result.producer),
                "CreateDate" => result.create_date = value.or(result.create_date),
                "ModifyDate" => result.modify_date = value.or(result.modify_date),
                _ => {}
            }
        }
    }
    Ok(result)
}

/// Collect references to all descendants with the given local name.
fn collect_nodes<'a>(node: &'a XmlNode, local_name: &str, out: &mut Vec<&'a XmlNode>) {
    if node.local_name == local_name {
        out.push(node);
    }
    for child in &node.children {
        collect_nodes(child, local_name, out);
    }
}

/// Extract the string value of an XMP property element.
fn property_text(node: &XmlNode) -> Option<String> {
    let mut items = Vec::new();
    collect_nodes(node, "li", &mut items);
    if !items.is_empty() {
        let joined = items
            .iter()
            .map(|item| item.text.trim())
            .filter(|text| !text.is_empty())
            .collect::<Vec<_>>()
            .join(", ");
        return if joined.is_empty() {
            None
        } else {
            Some(joined)
        };
    }
    let direct = node.text.trim();
    if !direct.is_empty() {
        return Some(direct.to_owned());
    }
    None
}

// ---------------------------------------------------------------------------
// Document integration
// ---------------------------------------------------------------------------

impl Document {
    /// Read the document's classic `/Info` dictionary, when present.
    ///
    /// The trailer's `/Info` entry is resolved through the cross-reference table
    /// (or a staged change) and decoded into a [`DocumentInfo`]. A document with
    /// no `/Info` entry returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns a structural error when the document body is damaged,
    /// [`Error::InvalidMetadata`] when `/Info` does not resolve to a dictionary,
    /// or an object-loading error for a broken reference.
    pub fn info(&self) -> Result<Option<DocumentInfo>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        let trailer = structure.xref.trailer();
        let Some(reference) = trailer.get("Info").and_then(Object::as_reference) else {
            return Ok(None);
        };
        let object = self.effective_object(structure, reference)?;
        let dictionary = object.as_dictionary().ok_or_else(|| {
            Error::InvalidMetadata("trailer /Info does not resolve to a dictionary".to_owned())
        })?;
        Ok(Some(DocumentInfo::from_dictionary(dictionary)))
    }

    /// Write a classic `/Info` dictionary through a copy-on-write change.
    ///
    /// When the trailer already carries an `/Info` reference the referenced
    /// object is replaced in place; otherwise a new object number is allocated.
    /// The trailer entry is staged and emitted by both [`Document::save`] and
    /// [`Document::save_incremental`], so the metadata survives a save.
    ///
    /// # Errors
    ///
    /// Returns a structural error when the document body is damaged, or any
    /// error from [`Document::set_object`].
    pub fn set_info(&mut self, info: &DocumentInfo) -> Result<()> {
        let id = {
            let structure = self.structure.as_ref().map_err(Clone::clone)?;
            match structure
                .xref
                .trailer()
                .get("Info")
                .and_then(Object::as_reference)
            {
                Some(id) => id,
                None => self.next_object_id(structure),
            }
        };
        self.set_object(id, info.to_object())?;
        self.trailer_entries
            .insert("Info".to_owned(), Object::Reference(id));
        Ok(())
    }

    /// Read the catalog's XMP `/Metadata` stream, when present.
    ///
    /// The catalog's `/Metadata` reference is resolved, the stream is decoded
    /// through the normal filter chain (`FlateDecode` is transparent), and the
    /// UTF-8 payload is parsed by [`parse_xmp`]. A catalog with no `/Metadata`
    /// entry returns `Ok(None)`.
    ///
    /// # Errors
    ///
    /// Returns a structural error when the document body is damaged,
    /// [`Error::InvalidMetadata`] when `/Metadata` is not a stream, a filter
    /// error for an undecodable stream, or [`Error::MalformedXmp`] for an
    /// invalid packet.
    pub fn xmp_metadata(&self) -> Result<Option<XmpMetadata>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        let catalog = self.effective_object(structure, structure.root_id)?;
        let catalog_dict = catalog.as_dictionary().ok_or_else(|| {
            Error::InvalidMetadata("document catalog is not a dictionary".to_owned())
        })?;
        let Some(id) = catalog_dict.get("Metadata").and_then(Object::as_reference) else {
            return Ok(None);
        };
        let object = self.effective_object(structure, id)?;
        let (dictionary, data) = match object {
            Object::Stream { dictionary, data } => (dictionary, data),
            _ => {
                return Err(Error::InvalidMetadata(
                    "catalog /Metadata does not resolve to a stream".to_owned(),
                ));
            }
        };
        let filters = crate::filter::filters_from_dictionary(&dictionary)?;
        let decoded = crate::filter::decode_stream(&data, &filters)?;
        let xml = std::str::from_utf8(&decoded)
            .map_err(|_| Error::MalformedXmp("XMP packet is not valid UTF-8".to_owned()))?;
        Ok(Some(parse_xmp(xml)?))
    }

    /// Write an XMP `/Metadata` stream, creating or updating the catalog entry.
    ///
    /// The packet is produced by [`XmpMetadata::to_xml`] and stored in a stream
    /// with `/Type /Metadata` and `/Subtype /XML`. The catalog dictionary is
    /// updated through [`Document::set_object`] and the stream is written as a
    /// new object, so the result is visible after [`Document::save`] and
    /// [`Document::save_incremental`].
    ///
    /// # Errors
    ///
    /// Returns a structural error when the document body is damaged,
    /// [`Error::InvalidMetadata`] when the catalog is not a dictionary, or any
    /// error from [`Document::set_object`].
    pub fn set_xmp_metadata(&mut self, xmp: &XmpMetadata) -> Result<()> {
        let (root_id, metadata_id) = {
            let structure = self.structure.as_ref().map_err(Clone::clone)?;
            let catalog = self.effective_object(structure, structure.root_id)?;
            let id = match catalog
                .as_dictionary()
                .and_then(|dictionary| dictionary.get("Metadata"))
                .and_then(Object::as_reference)
            {
                Some(id) => id,
                None => self.next_object_id(structure),
            };
            (structure.root_id, id)
        };
        self.set_object(metadata_id, xmp.to_stream_object())?;

        let catalog = {
            let structure = self.structure.as_ref().map_err(Clone::clone)?;
            self.effective_object(structure, root_id)?
        };
        let mut catalog_dict = match catalog {
            Object::Dictionary(dictionary) => dictionary,
            Object::Stream { dictionary, .. } => dictionary,
            _ => {
                return Err(Error::InvalidMetadata(
                    "document catalog is not a dictionary".to_owned(),
                ));
            }
        };
        catalog_dict.insert("Metadata".to_owned(), Object::Reference(metadata_id));
        self.set_object(root_id, Object::Dictionary(catalog_dict))?;
        Ok(())
    }

    /// Return the staged value of `id` when present, else load it from the xref.
    fn effective_object(
        &self,
        structure: &crate::parser::Structure,
        id: ObjectId,
    ) -> Result<Object> {
        match self.changes.get(&id) {
            Some(object) => Ok(object.clone()),
            None => crate::parser::load_object(&self.bytes, &structure.xref, id),
        }
    }

    /// Allocate the next unused object number for a newly created metadata object.
    fn next_object_id(&self, structure: &crate::parser::Structure) -> ObjectId {
        let mut highest = structure
            .xref
            .in_use_entries()
            .last()
            .map_or(0, |(number, _)| *number);
        for id in self.changes.keys() {
            highest = highest.max(id.number);
        }
        ObjectId::new(highest.saturating_add(1), 0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Document;

    /// Render raw bytes as an uppercase PDF hexadecimal string.
    fn hex(bytes: &[u8]) -> String {
        let mut out = String::from("<");
        for byte in bytes {
            out.push_str(&format!("{byte:02X}"));
        }
        out.push('>');
        out
    }

    /// A minimal one-page document with four contiguous objects.
    fn base_objects() -> Vec<(u32, Vec<u8>)> {
        vec![
            (1, b"<< /Type /Catalog /Pages 2 0 R >>".to_vec()),
            (2, b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec()),
            (
                3,
                b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
            ),
            (
                4,
                b"<< /Length 13 >>\nstream\nBT (Hi) Tj ET\nendstream".to_vec(),
            ),
        ]
    }

    /// Assemble a classic-table PDF from `(number, body)` pairs.
    ///
    /// Object numbers must be contiguous starting at one, which every fixture
    /// here satisfies.
    fn build_pdf(objects: &[(u32, Vec<u8>)], trailer_extra: &str) -> Vec<u8> {
        let mut sorted = objects.to_vec();
        sorted.sort_by_key(|(number, _)| *number);
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (number, body) in &sorted {
            assert_eq!(
                *number,
                offsets.len() as u32 + 1,
                "fixture object numbers must be contiguous"
            );
            offsets.push(out.len());
            out.extend_from_slice(format!("{number} 0 obj\n").as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        let count = sorted.len() + 1;
        out.extend_from_slice(format!("xref\n0 {count}\n0000000000 65535 f \n").as_bytes());
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {count} /Root 1 0 R{trailer_extra} >>\nstartxref\n{xref}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    /// Wrap arbitrary bytes in a valid zlib stream using stored DEFLATE blocks.
    /// Independent of the inflate implementation under test.
    fn zlib_stored(data: &[u8]) -> Vec<u8> {
        let mut out = vec![0x78, 0x01];
        let mut chunks = data.chunks(0xffff).peekable();
        while let Some(chunk) = chunks.next() {
            let last = chunks.peek().is_none();
            out.push(u8::from(last));
            let length = chunk.len() as u16;
            out.extend_from_slice(&length.to_le_bytes());
            out.extend_from_slice(&(!length).to_le_bytes());
            out.extend_from_slice(chunk);
        }
        out.extend_from_slice(&adler32(data).to_be_bytes());
        out
    }

    /// Compute the Adler-32 checksum of `data` (RFC 1950).
    fn adler32(data: &[u8]) -> u32 {
        let mut a = 1u32;
        let mut b = 0u32;
        for &byte in data {
            a = (a + u32::from(byte)) % 65521;
            b = (b + a) % 65521;
        }
        (b << 16) | a
    }

    /// A canonical XMP packet using the conventional prefixes.
    fn canonical_xmp() -> String {
        concat!(
            "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>",
            "<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">",
            "<rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">",
            "<rdf:Description rdf:about=\"\" xmlns:dc=\"http://purl.org/dc/elements/1.1/\" ",
            "xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\" xmlns:xmp=\"http://ns.adobe.com/xap/1.0/\">",
            "<dc:title><rdf:Alt><rdf:li xml:lang=\"x-default\">A Title</rdf:li></rdf:Alt></dc:title>",
            "<dc:creator><rdf:Seq><rdf:li>Jane</rdf:li><rdf:li>John</rdf:li></rdf:Seq></dc:creator>",
            "<dc:description><rdf:Alt><rdf:li xml:lang=\"x-default\">A &amp; B</rdf:li></rdf:Alt></dc:description>",
            "<dc:subject><rdf:Bag><rdf:li>one</rdf:li><rdf:li>two</rdf:li></rdf:Bag></dc:subject>",
            "<pdf:Producer>PDFXI</pdf:Producer>",
            "<xmp:CreateDate>2024-01-02T03:04:05Z</xmp:CreateDate>",
            "<xmp:ModifyDate>2024-02-03T04:05:06Z</xmp:ModifyDate>",
            "</rdf:Description></rdf:RDF></x:xmpmeta>",
            "<?xpacket end=\"w\"?>"
        )
        .to_owned()
    }

    #[test]
    fn utf16_and_pdfdoc_round_trip() {
        for text in ["", "Plain ASCII", "Café €", "日本語 ☕"] {
            let encoded = encode_text_string(text);
            assert_eq!(
                decode_text_string(&encoded),
                text,
                "round trip for {text:?}"
            );
        }
        assert!(!encode_text_string("ASCII").starts_with(&UTF16BE_BOM));
        assert!(encode_text_string("日本語").starts_with(&UTF16BE_BOM));
        assert_eq!(decode_text_string(&[0x92]), "\u{2122}");
        assert_eq!(decode_text_string(&[0xA0]), "\u{20AC}");
    }

    #[test]
    fn pdfdoc_and_utf16_decode_specific_vectors() {
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0x00, 0x48, 0x00, 0x69]),
            "Hi"
        );
        assert_eq!(
            decode_text_string(&[0xFE, 0xFF, 0xD8, 0x3D, 0xDE, 0x00, 0x00, 0x21]),
            "\u{1F600}!"
        );
        // A lone high surrogate is replaced rather than panicking.
        assert_eq!(decode_text_string(&[0xFE, 0xFF, 0xD8, 0x00]), "\u{FFFD}");
    }

    #[test]
    fn strict_date_parses_all_components_and_offset() {
        let date = parse_pdf_date("D:20240102123456+02'30'");
        assert_eq!(date.year, Some(2024));
        assert_eq!(date.month, Some(1));
        assert_eq!(date.day, Some(2));
        assert_eq!(date.hour, Some(12));
        assert_eq!(date.minute, Some(34));
        assert_eq!(date.second, Some(56));
        assert_eq!(date.utc_offset_minutes, Some(150));
        assert!(date.well_formed);
        let utc = parse_pdf_date("D:20240102123456Z");
        assert_eq!(utc.utc_offset_minutes, Some(0));
        assert!(utc.well_formed);
        let short = parse_pdf_date("D:2024");
        assert_eq!(short.year, Some(2024));
        assert!(short.month.is_none());
        assert!(short.well_formed);
    }

    #[test]
    fn malformed_date_is_preserved_raw() {
        let bad = parse_pdf_date("D:yesterday");
        assert_eq!(bad.raw, "D:yesterday");
        assert!(!bad.well_formed);
        assert!(bad.year.is_none());

        let iso = parse_pdf_date("2020-01-02T03:04:05");
        assert_eq!(iso.raw, "2020-01-02T03:04:05");
        assert!(!iso.well_formed);
        assert_eq!(iso.year, Some(2020));
        assert_eq!(iso.month, Some(1));
        assert_eq!(iso.day, Some(2));
        assert_eq!(iso.hour, Some(3));
    }

    #[test]
    fn info_reads_hand_built_dictionary() {
        let mut objects = base_objects();
        let info = format!(
            "<< /Title {} /Author {} /Subject {} /Keywords {} /Creator {} /Producer {} /CreationDate {} /ModDate {} >>",
            hex(&encode_text_string("Café ☕ Report")),
            hex(&encode_text_string("Jane Doe")),
            hex(&encode_text_string("A subject")),
            hex(&encode_text_string("one,two")),
            hex(&encode_text_string("Writer")),
            hex(&encode_text_string("PDFXI")),
            hex(b"D:20240102123456+02'30'"),
            hex(b"not-a-date"),
        );
        objects.push((5, info.into_bytes()));
        let pdf = build_pdf(&objects, " /Info 5 0 R");
        let document = Document::from_bytes(pdf).expect("fixture parses");
        let info = document.info().expect("info read").expect("info present");
        assert_eq!(info.title.as_deref(), Some("Café ☕ Report"));
        assert_eq!(info.author.as_deref(), Some("Jane Doe"));
        assert_eq!(info.subject.as_deref(), Some("A subject"));
        assert_eq!(info.keywords.as_deref(), Some("one,two"));
        assert_eq!(info.creator.as_deref(), Some("Writer"));
        assert_eq!(info.producer.as_deref(), Some("PDFXI"));
        let creation = info.creation_date.expect("creation date");
        assert_eq!(creation.raw, "D:20240102123456+02'30'");
        assert_eq!(creation.year, Some(2024));
        assert!(creation.well_formed);
        let modified = info.modification_date.expect("mod date");
        assert_eq!(modified.raw, "not-a-date");
        assert!(!modified.well_formed);
    }

    #[test]
    fn info_missing_returns_none() {
        let document = Document::from_bytes(build_pdf(&base_objects(), "")).expect("fixture");
        assert!(document.info().expect("info read").is_none());
    }

    #[test]
    fn info_write_and_reparse() {
        let document = Document::from_bytes(build_pdf(&base_objects(), "")).expect("fixture");
        let info = DocumentInfo {
            title: Some("Written Title".to_owned()),
            producer: Some("PDFXI".to_owned()),
            creation_date: Some(parse_pdf_date("D:20240102")),
            ..Default::default()
        };
        let mut document = document;
        document.set_info(&info).expect("set info");
        let saved = document.save().expect("save");
        let reparsed = Document::from_bytes(saved).expect("reparse");
        let read = reparsed.info().expect("info read").expect("info present");
        assert_eq!(read.title.as_deref(), Some("Written Title"));
        assert_eq!(read.producer.as_deref(), Some("PDFXI"));
        assert_eq!(read.creation_date.expect("date").raw, "D:20240102");
    }

    #[test]
    fn parse_canonical_xmp_packet() {
        let xmp = parse_xmp(&canonical_xmp()).expect("canonical packet parses");
        assert_eq!(xmp.title.as_deref(), Some("A Title"));
        assert_eq!(xmp.creator.as_deref(), Some("Jane, John"));
        assert_eq!(xmp.description.as_deref(), Some("A & B"));
        assert_eq!(xmp.subject.as_deref(), Some("one, two"));
        assert_eq!(xmp.producer.as_deref(), Some("PDFXI"));
        assert_eq!(xmp.create_date.as_deref(), Some("2024-01-02T03:04:05Z"));
        assert_eq!(xmp.modify_date.as_deref(), Some("2024-02-03T04:05:06Z"));
    }

    #[test]
    fn xmp_parser_is_namespace_prefix_tolerant() {
        let xml = concat!(
            "<?xml version=\"1.0\"?>",
            "<m:meta xmlns:m=\"adobe:ns:meta/\">",
            "<r:RDF xmlns:r=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">",
            "<r:Description xmlns:d=\"http://purl.org/dc/elements/1.1/\">",
            "<d:title><r:Alt><r:li>Prefix Free</r:li></r:Alt></d:title>",
            "<Producer>Inline</Producer>",
            "</r:Description></r:RDF></m:meta>"
        );
        let xmp = parse_xmp(xml).expect("alternate prefixes parse");
        assert_eq!(xmp.title.as_deref(), Some("Prefix Free"));
        assert_eq!(xmp.producer.as_deref(), Some("Inline"));
    }

    #[test]
    fn malformed_xml_is_structured_error() {
        for bad in [
            "<x:xmpmeta><rdf:RDF>",
            "<a><b></a>",
            "<a attr=noquotes/>",
            "<a>&unknown;</a>",
            "",
        ] {
            let error = parse_xmp(bad).expect_err("malformed XML rejected");
            assert!(
                matches!(error, Error::MalformedXmp(_)),
                "expected structured XMP error, got {error}"
            );
        }
    }

    #[test]
    fn xmp_missing_returns_none() {
        let document = Document::from_bytes(build_pdf(&base_objects(), "")).expect("fixture");
        assert!(document.xmp_metadata().expect("xmp read").is_none());
    }

    #[test]
    fn xmp_parse_of_canonical_packet_from_document() {
        let mut objects = base_objects();
        objects[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_vec();
        objects.push((
            5,
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                canonical_xmp().len(),
                canonical_xmp()
            )
            .into_bytes(),
        ));
        let document = Document::from_bytes(build_pdf(&objects, "")).expect("fixture");
        let xmp = document
            .xmp_metadata()
            .expect("xmp read")
            .expect("xmp present");
        assert_eq!(xmp.title.as_deref(), Some("A Title"));
        assert_eq!(xmp.producer.as_deref(), Some("PDFXI"));
    }

    #[test]
    fn flate_compressed_metadata_stream_decodes() {
        let packet = canonical_xmp();
        let compressed = zlib_stored(packet.as_bytes());
        let mut objects = base_objects();
        objects[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Metadata 5 0 R >>".to_vec();
        let mut stream = format!(
            "<< /Length {} /Filter /FlateDecode >>\nstream\n",
            compressed.len()
        )
        .into_bytes();
        stream.extend_from_slice(&compressed);
        stream.extend_from_slice(b"\nendstream");
        objects.push((5, stream));
        let document = Document::from_bytes(build_pdf(&objects, "")).expect("fixture");
        let xmp = document
            .xmp_metadata()
            .expect("compressed xmp read")
            .expect("xmp present");
        assert_eq!(xmp.title.as_deref(), Some("A Title"));
        assert_eq!(xmp.subject.as_deref(), Some("one, two"));
    }

    #[test]
    fn xmp_write_and_reparse() {
        let document = Document::from_bytes(build_pdf(&base_objects(), "")).expect("fixture");
        let xmp = XmpMetadata {
            title: Some("Written <Title> & More".to_owned()),
            subject: Some("alpha, beta".to_owned()),
            producer: Some("PDFXI".to_owned()),
            create_date: Some("2024-01-02".to_owned()),
            ..Default::default()
        };
        let mut document = document;
        document.set_xmp_metadata(&xmp).expect("set xmp");
        let saved = document.save().expect("save");
        let reparsed = Document::from_bytes(saved).expect("reparse");
        let read = reparsed
            .xmp_metadata()
            .expect("xmp read")
            .expect("xmp present");
        assert_eq!(xmp, read);
    }

    #[test]
    fn xmp_write_is_deterministic() {
        let xmp = XmpMetadata {
            title: Some("Stable".to_owned()),
            ..Default::default()
        };
        assert_eq!(xmp.to_xml(), xmp.to_xml());
        assert!(xmp.to_xml().starts_with("<?xpacket"));
        assert!(xmp.to_xml().ends_with("<?xpacket end=\"w\"?>\n"));
    }

    #[test]
    fn save_round_trips_metadata() {
        // Build a document with both Info and XMP already present.
        let mut objects = base_objects();
        objects[0].1 = b"<< /Type /Catalog /Pages 2 0 R /Metadata 6 0 R >>".to_vec();
        let info = format!(
            "<< /Title {} /CreationDate {} >>",
            hex(&encode_text_string("Round Trip")),
            hex(b"D:20240102")
        );
        objects.push((5, info.into_bytes()));
        objects.push((
            6,
            format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                canonical_xmp().len(),
                canonical_xmp()
            )
            .into_bytes(),
        ));
        let source = build_pdf(&objects, " /Info 5 0 R");
        let document = Document::from_bytes(source.clone()).expect("source parses");
        let info_before = document.info().expect("info").expect("info present");
        let xmp_before = document.xmp_metadata().expect("xmp").expect("xmp present");

        let saved = document.save().expect("full save");
        let reparsed = Document::from_bytes(saved).expect("reparse");
        assert_eq!(reparsed.page_text(0).expect("page text"), "Hi");
        assert_eq!(
            reparsed.info().expect("info").expect("info present"),
            info_before
        );
        assert_eq!(
            reparsed.xmp_metadata().expect("xmp").expect("xmp present"),
            xmp_before
        );
    }
}
