//! FDF 1.7 field export/import and a small XFDF export subset.
//!
//! [`export_fdf`] writes an FDF 1.7 file containing the current value of every
//! exportable field. [`import_fdf`] parses that format back into an
//! [`FdfDocument`] whose fields can be applied to a [`Form`] through
//! [`crate::FormFill::fill_from_fdf`]. The parser is a small, bounds-checked
//! recursive-descent reader over bytes; malformed input produces a structured
//! [`FdfError`] with a best-effort byte offset.
//!
//! # String encoding
//!
//! Values are written as PDF literal strings. `(`, `)`, and `\` are
//! backslash-escaped, common control characters use PDF's short escapes, and
//! every other non-printable or non-ASCII *byte* is written as a three-digit
//! octal escape (`\ddd`). UTF-8 text therefore round-trips byte-for-byte.
//!
//! # XFDF scope
//!
//! [`export_xfdf`] emits a small, schema-compatible XML subset (`<xfdf>`,
//! `<fields>`, `<field name="...">`, `<value>`). The matching importer lives in
//! [`crate::xfdf`]: [`crate::import_xfdf`] parses that XML and maps its values
//! onto the target form's field types, while FDF remains the byte-oriented
//! round-trip format.

use crate::field::{Field, FieldValue};
use crate::form::Form;
use std::fmt;

/// A structured error produced while exporting or importing form data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FdfError {
    /// A numeric field holds a value that PDF numbers cannot represent.
    NonFiniteNumber {
        /// Fully qualified name of the offending field.
        field: String,
    },
    /// The input does not begin with an `%FDF-` header.
    MissingHeader {
        /// Byte offset where the header was expected.
        offset: usize,
    },
    /// The header names an FDF version this parser does not accept.
    UnsupportedVersion {
        /// The version text that was found, for example `2.0`.
        version: String,
        /// Byte offset of the header.
        offset: usize,
    },
    /// The input ended before a required token was read.
    UnexpectedEof {
        /// Byte offset where more input was needed.
        offset: usize,
    },
    /// A specific byte or token was expected.
    Expected {
        /// Human-readable description of the expected token.
        expected: &'static str,
        /// Byte offset where it was expected.
        offset: usize,
    },
    /// An octal string escape produced a value greater than 255.
    InvalidOctal {
        /// Byte offset of the backslash that started the escape.
        offset: usize,
    },
    /// A literal string could not be decoded as UTF-8.
    InvalidUtf8 {
        /// Byte offset where the string started.
        offset: usize,
    },
    /// A literal string was not closed before end of input.
    UnterminatedString {
        /// Byte offset where the string started.
        offset: usize,
    },
    /// An array was not closed before end of input.
    UnterminatedArray {
        /// Byte offset of the opening bracket.
        offset: usize,
    },
    /// A dictionary was not closed before end of input.
    UnterminatedDictionary {
        /// Byte offset of the opening `<<`.
        offset: usize,
    },
    /// A numeric token could not be parsed as a finite number.
    InvalidNumber {
        /// The offending token.
        token: String,
        /// Byte offset of the token.
        offset: usize,
    },
    /// A field dictionary had no literal-string `/T` entry.
    MissingFieldName {
        /// Zero-based position of the field in the `/Fields` array.
        index: usize,
        /// Byte offset of the enclosing object.
        offset: usize,
    },
    /// No `/FDF` dictionary with a `/Fields` array was found.
    MissingFieldsArray,
    /// A document had unexpected data after the `%%EOF` marker.
    TrailingData {
        /// Byte offset of the first unexpected byte.
        offset: usize,
    },
    /// The object structure was not a valid FDF object.
    MalformedStructure {
        /// Description of the structural problem.
        detail: &'static str,
        /// Byte offset where the problem was detected.
        offset: usize,
    },
    /// A `/V` entry used a value kind this importer does not map.
    UnsupportedValue {
        /// Byte offset of the enclosing object.
        offset: usize,
    },
}

impl fmt::Display for FdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NonFiniteNumber { field } => {
                write!(f, "field '{field}' has a non-finite numeric value")
            }
            Self::MissingHeader { offset } => write!(f, "missing %FDF- header at byte {offset}"),
            Self::UnsupportedVersion { version, offset } => {
                write!(f, "unsupported FDF version '{version}' at byte {offset}")
            }
            Self::UnexpectedEof { offset } => write!(f, "unexpected end of FDF at byte {offset}"),
            Self::Expected { expected, offset } => {
                write!(f, "expected {expected} at byte {offset}")
            }
            Self::InvalidOctal { offset } => {
                write!(f, "octal escape exceeds 255 at byte {offset}")
            }
            Self::InvalidUtf8 { offset } => {
                write!(f, "string at byte {offset} is not valid UTF-8")
            }
            Self::UnterminatedString { offset } => {
                write!(f, "unterminated literal string at byte {offset}")
            }
            Self::UnterminatedArray { offset } => {
                write!(f, "unterminated array at byte {offset}")
            }
            Self::UnterminatedDictionary { offset } => {
                write!(f, "unterminated dictionary at byte {offset}")
            }
            Self::InvalidNumber { token, offset } => {
                write!(f, "invalid number '{token}' at byte {offset}")
            }
            Self::MissingFieldName { index, offset } => write!(
                f,
                "FDF field {index} in object at byte {offset} has no literal-string /T name"
            ),
            Self::MissingFieldsArray => {
                write!(f, "FDF has no /FDF dictionary with a /Fields array")
            }
            Self::TrailingData { offset } => {
                write!(f, "unexpected data after %%EOF at byte {offset}")
            }
            Self::MalformedStructure { detail, offset } => {
                write!(f, "malformed FDF object at byte {offset}: {detail}")
            }
            Self::UnsupportedValue { offset } => {
                write!(f, "unsupported /V value in object at byte {offset}")
            }
        }
    }
}

impl std::error::Error for FdfError {}

/// One exported/imported field: a fully qualified name and its value.
#[derive(Debug, Clone, PartialEq)]
pub struct FdfField {
    name: String,
    value: FieldValue,
}

impl FdfField {
    /// Builds an FDF field from a name and value.
    pub fn new(name: impl Into<String>, value: FieldValue) -> Self {
        Self {
            name: name.into(),
            value,
        }
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field's value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }
}

/// The field values carried by an FDF document.
///
/// Fields are kept in the order they appeared in the `/Fields` array. That order
/// is deterministic for a given input but is *not* used for change reports;
/// [`crate::FillReport`] always sorts by field name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FdfDocument {
    fields: Vec<FdfField>,
}

impl FdfDocument {
    /// Builds a document from a list of fields.
    pub fn new(fields: Vec<FdfField>) -> Self {
        Self { fields }
    }

    /// Returns the fields in document order.
    pub fn fields(&self) -> &[FdfField] {
        &self.fields
    }

    /// Returns the first field with the given name, if present.
    pub fn field(&self, name: &str) -> Option<&FdfField> {
        self.fields.iter().find(|field| field.name == name)
    }

    /// Returns the number of fields.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` when the document has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }
}

/// Exports the current values of a form as an FDF 1.7 document.
///
/// Fields are emitted in ascending name order for canonical output. Fields with
/// the [`no-export`](crate::FieldFlags::NO_EXPORT) flag are skipped. Empty
/// fields carry only a `/T` entry; checkboxes use `/On` and `/Off` names,
/// multi-select choices use an array of literal strings, and numeric values use
/// a bare PDF number.
///
/// Returns [`FdfError::NonFiniteNumber`] if a numeric field holds NaN or an
/// infinity, since those cannot be written as PDF numbers.
pub fn export_fdf(form: &Form) -> Result<String, FdfError> {
    let mut fields: Vec<&Field> = form
        .fields()
        .filter(|field| !field.flags().is_no_export())
        .collect();
    fields.sort_by(|a, b| a.name().cmp(b.name()));

    let mut out = String::from("%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [\n");
    for field in fields {
        out.push_str("<< /T ");
        write_pdf_string(&mut out, field.name());
        match field.value() {
            FieldValue::Empty => {}
            FieldValue::Text(text) => {
                out.push_str(" /V ");
                write_pdf_string(&mut out, text);
            }
            FieldValue::Bool(on) => {
                out.push_str(if *on { " /V /On" } else { " /V /Off" });
            }
            FieldValue::Choice(values) => {
                out.push_str(" /V [");
                for (index, value) in values.iter().enumerate() {
                    if index > 0 {
                        out.push(' ');
                    }
                    write_pdf_string(&mut out, value);
                }
                out.push(']');
            }
            FieldValue::Number(number) => {
                if !number.is_finite() {
                    return Err(FdfError::NonFiniteNumber {
                        field: field.name().to_owned(),
                    });
                }
                out.push_str(" /V ");
                out.push_str(&number.to_string());
            }
        }
        out.push_str(" >>\n");
    }
    out.push_str("] >> >>\nendobj\ntrailer\n<< /Root 1 0 R >>\n%%EOF\n");
    Ok(out)
}

/// Exports a small XFDF-compatible XML subset.
///
/// Text values are XML-escaped, booleans become `On`/`Off`, and each choice
/// selection becomes its own `<value>` element. The companion importer is
/// [`crate::import_xfdf`], which reverses this mapping using the target form's
/// field types.
pub fn export_xfdf(form: &Form) -> Result<String, FdfError> {
    let mut fields: Vec<&Field> = form
        .fields()
        .filter(|field| !field.flags().is_no_export())
        .collect();
    fields.sort_by(|a, b| a.name().cmp(b.name()));

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <xfdf xmlns=\"http://ns.adobe.com/xfdf/\">\n<fields>\n",
    );
    for field in fields {
        out.push_str("<field name=\"");
        write_xml_escaped(&mut out, field.name(), true);
        out.push_str("\">\n");
        match field.value() {
            FieldValue::Empty => {}
            FieldValue::Text(text) => write_xml_value(&mut out, text),
            FieldValue::Bool(on) => write_xml_value(&mut out, if *on { "On" } else { "Off" }),
            FieldValue::Choice(values) => {
                for value in values {
                    write_xml_value(&mut out, value);
                }
            }
            FieldValue::Number(number) => {
                if !number.is_finite() {
                    return Err(FdfError::NonFiniteNumber {
                        field: field.name().to_owned(),
                    });
                }
                write_xml_value(&mut out, &number.to_string());
            }
        }
        out.push_str("</field>\n");
    }
    out.push_str("</fields>\n</xfdf>\n");
    Ok(out)
}

fn write_xml_value(out: &mut String, value: &str) {
    out.push_str("<value>");
    write_xml_escaped(out, value, false);
    out.push_str("</value>\n");
}

fn write_xml_escaped(out: &mut String, text: &str, attribute: bool) {
    for ch in text.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' if attribute => out.push_str("&quot;"),
            '\'' if attribute => out.push_str("&apos;"),
            c if c.is_control() => out.push_str(&format!("&#x{:X};", c as u32)),
            c => out.push(c),
        }
    }
}

/// Writes `text` as a PDF literal string with deterministic escaping.
fn write_pdf_string(out: &mut String, text: &str) {
    out.push('(');
    for &byte in text.as_bytes() {
        match byte {
            b'\\' => out.push_str("\\\\"),
            b'(' => out.push_str("\\("),
            b')' => out.push_str("\\)"),
            b'\n' => out.push_str("\\n"),
            b'\r' => out.push_str("\\r"),
            b'\t' => out.push_str("\\t"),
            0x08 => out.push_str("\\b"),
            0x0c => out.push_str("\\f"),
            0x20..=0x7e => out.push(byte as char),
            other => {
                out.push('\\');
                out.push(digit((other >> 6) & 0o7));
                out.push(digit((other >> 3) & 0o7));
                out.push(digit(other & 0o7));
            }
        }
    }
    out.push(')');
}

fn digit(value: u8) -> char {
    // The octal writer only passes 0..=7; masking keeps this conversion total.
    char::from(b'0' + (value & 0x07))
}

/// Parses FDF 1.7 text into an [`FdfDocument`].
///
/// The accepted subset covers the files produced by [`export_fdf`] plus common
/// FDF variations: PDF whitespace and `%` comments, multiple indirect objects,
/// `true`/`false`/`null` values, nested arrays/dictionaries, indirect references
/// (ignored), and literal strings with nested parentheses and standard escapes.
/// Hex strings and streams are not supported and produce a structured error.
pub fn import_fdf(input: &str) -> Result<FdfDocument, FdfError> {
    let mut parser = Parser::new(input.as_bytes());
    parser.parse_document()
}

// ---------------------------------------------------------------------------
// Internal byte-level parser
// ---------------------------------------------------------------------------

/// A generic PDF value used only while walking the FDF object graph.
#[derive(Debug, Clone, PartialEq)]
enum FdfValue {
    Null,
    Bool(bool),
    Number(f64),
    Name(String),
    String(String),
    Array(Vec<FdfValue>),
    Dictionary(Vec<(String, FdfValue)>),
}

impl FdfValue {
    fn dictionary(&self) -> Option<&Vec<(String, FdfValue)>> {
        match self {
            Self::Dictionary(entries) => Some(entries),
            _ => None,
        }
    }
}

fn find_entry<'a>(entries: &'a [(String, FdfValue)], key: &str) -> Option<&'a FdfValue> {
    entries
        .iter()
        .rev()
        .find(|(name, _)| name == key)
        .map(|(_, value)| value)
}

fn is_delimiter(byte: u8) -> bool {
    matches!(
        byte,
        0 | 9
            | 10
            | 12
            | 13
            | 32
            | b'('
            | b')'
            | b'<'
            | b'>'
            | b'['
            | b']'
            | b'{'
            | b'}'
            | b'/'
            | b'%'
    )
}

fn is_integer_token(token: &[u8]) -> bool {
    !token.is_empty()
        && token
            .iter()
            .all(|byte| byte.is_ascii_digit() || *byte == b'+' || *byte == b'-')
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn decode_name(raw: &[u8]) -> String {
    let mut bytes = Vec::with_capacity(raw.len());
    let mut index = 0;
    while index < raw.len() {
        if raw[index] == b'#'
            && index + 2 < raw.len()
            && let (Some(hi), Some(lo)) = (hex_value(raw[index + 1]), hex_value(raw[index + 2]))
        {
            bytes.push((hi << 4) | lo);
            index += 3;
        } else {
            bytes.push(raw[index]);
            index += 1;
        }
    }
    String::from_utf8_lossy(&bytes).into_owned()
}

struct Parser<'a> {
    src: &'a [u8],
    pos: usize,
}

impl<'a> Parser<'a> {
    fn new(src: &'a [u8]) -> Self {
        Self { src, pos: 0 }
    }

    fn at_end(&self) -> bool {
        self.pos >= self.src.len()
    }

    fn peek(&self) -> Option<u8> {
        self.src.get(self.pos).copied()
    }

    fn starts_with(&self, needle: &[u8]) -> bool {
        self.src
            .get(self.pos..)
            .is_some_and(|tail| tail.starts_with(needle))
    }

    fn skip_ws(&mut self) {
        loop {
            while let Some(byte) = self.peek()
                && matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
            {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') {
                while let Some(byte) = self.peek()
                    && byte != b'\n'
                    && byte != b'\r'
                {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    /// Skips whitespace and ordinary comments but stops before `%%EOF`.
    ///
    /// `%%EOF` is itself a `%` comment, so `skip_ws` would consume the end-of-
    /// file marker when it appears at the top level.
    fn skip_layout(&mut self) {
        loop {
            while let Some(byte) = self.peek()
                && matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
            {
                self.pos += 1;
            }
            if self.peek() == Some(b'%') && !self.starts_with(b"%%EOF") {
                while let Some(byte) = self.peek()
                    && byte != b'\n'
                    && byte != b'\r'
                {
                    self.pos += 1;
                }
                continue;
            }
            break;
        }
    }

    fn expect_byte(&mut self, byte: u8, what: &'static str) -> Result<(), FdfError> {
        self.skip_ws();
        if self.peek() == Some(byte) {
            self.pos += 1;
            Ok(())
        } else {
            Err(FdfError::Expected {
                expected: what,
                offset: self.pos,
            })
        }
    }

    fn read_token(&mut self) -> Result<&'a [u8], FdfError> {
        self.skip_ws();
        let start = self.pos;
        while let Some(byte) = self.peek()
            && !is_delimiter(byte)
        {
            self.pos += 1;
        }
        if self.pos == start {
            return Err(FdfError::Expected {
                expected: "token",
                offset: start,
            });
        }
        Ok(&self.src[start..self.pos])
    }

    fn read_header_line(&mut self) -> &'a [u8] {
        let start = self.pos;
        while let Some(byte) = self.peek()
            && byte != b'\n'
            && byte != b'\r'
        {
            self.pos += 1;
        }
        &self.src[start..self.pos]
    }

    fn parse_name(&mut self) -> Result<String, FdfError> {
        self.skip_ws();
        if self.peek() != Some(b'/') {
            return Err(FdfError::Expected {
                expected: "name starting with '/'",
                offset: self.pos,
            });
        }
        self.pos += 1;
        let start = self.pos;
        while let Some(byte) = self.peek()
            && !is_delimiter(byte)
        {
            self.pos += 1;
        }
        Ok(decode_name(&self.src[start..self.pos]))
    }

    fn parse_literal_string(&mut self) -> Result<String, FdfError> {
        self.skip_ws();
        let start = self.pos;
        if self.peek() != Some(b'(') {
            return Err(FdfError::Expected {
                expected: "literal string starting with '('",
                offset: self.pos,
            });
        }
        self.pos += 1;
        let mut bytes = Vec::new();
        let mut depth = 1usize;

        while let Some(byte) = self.peek() {
            self.pos += 1;
            match byte {
                b'(' => {
                    depth += 1;
                    bytes.push(byte);
                }
                b')' => {
                    depth -= 1;
                    if depth == 0 {
                        return String::from_utf8(bytes)
                            .map_err(|_| FdfError::InvalidUtf8 { offset: start });
                    }
                    bytes.push(byte);
                }
                b'\\' => {
                    let escape_offset = self.pos - 1;
                    let Some(escaped) = self.peek() else {
                        return Err(FdfError::UnexpectedEof { offset: self.pos });
                    };
                    self.pos += 1;
                    match escaped {
                        b'n' => bytes.push(b'\n'),
                        b'r' => bytes.push(b'\r'),
                        b't' => bytes.push(b'\t'),
                        b'b' => bytes.push(0x08),
                        b'f' => bytes.push(0x0c),
                        b'(' | b')' | b'\\' => bytes.push(escaped),
                        b'\n' => {}
                        b'\r' => {
                            if self.peek() == Some(b'\n') {
                                self.pos += 1;
                            }
                        }
                        b'0'..=b'7' => {
                            let mut value = u16::from(escaped - b'0');
                            let mut count = 1;
                            while count < 3
                                && let Some(next) = self.peek()
                                && matches!(next, b'0'..=b'7')
                            {
                                value = value * 8 + u16::from(next - b'0');
                                self.pos += 1;
                                count += 1;
                            }
                            if value > 255 {
                                return Err(FdfError::InvalidOctal {
                                    offset: escape_offset,
                                });
                            }
                            bytes.push(value as u8);
                        }
                        other => bytes.push(other),
                    }
                }
                b'\r' => {
                    if self.peek() == Some(b'\n') {
                        self.pos += 1;
                    }
                    bytes.push(b'\n');
                }
                other => bytes.push(other),
            }
        }
        Err(FdfError::UnterminatedString { offset: start })
    }

    fn parse_array(&mut self) -> Result<FdfValue, FdfError> {
        let offset = self.pos;
        self.expect_byte(b'[', "opening '['")?;
        let mut values = Vec::new();
        loop {
            self.skip_ws();
            match self.peek() {
                Some(b']') => {
                    self.pos += 1;
                    return Ok(FdfValue::Array(values));
                }
                None => return Err(FdfError::UnterminatedArray { offset }),
                _ => values.push(self.parse_value()?),
            }
        }
    }

    fn parse_dictionary(&mut self) -> Result<FdfValue, FdfError> {
        let offset = self.pos;
        self.expect_byte(b'<', "opening '<<'")?;
        self.expect_byte(b'<', "opening '<<'")?;
        let mut entries = Vec::new();
        loop {
            self.skip_ws();
            if self.peek() == Some(b'>') {
                self.pos += 1;
                if self.peek() == Some(b'>') {
                    self.pos += 1;
                    return Ok(FdfValue::Dictionary(entries));
                }
                return Err(FdfError::Expected {
                    expected: "closing '>>'",
                    offset: self.pos,
                });
            }
            if self.at_end() {
                return Err(FdfError::UnterminatedDictionary { offset });
            }
            let key = self.parse_name()?;
            let value = self.parse_value()?;
            entries.push((key, value));
        }
    }

    /// Consumes a `generation R` tail when `first` looks like an object number.
    fn consume_reference_tail(&mut self, first: &[u8]) -> Result<bool, FdfError> {
        if !is_integer_token(first) {
            return Ok(false);
        }
        let saved = self.pos;
        let second = match self.read_token() {
            Ok(token) => token,
            Err(_) => {
                self.pos = saved;
                return Ok(false);
            }
        };
        let third = match self.read_token() {
            Ok(token) => token,
            Err(_) => {
                self.pos = saved;
                return Ok(false);
            }
        };
        if third == b"R" && is_integer_token(second) {
            Ok(true)
        } else {
            self.pos = saved;
            Ok(false)
        }
    }

    fn parse_value(&mut self) -> Result<FdfValue, FdfError> {
        self.skip_ws();
        match self.peek() {
            Some(b'(') => return Ok(FdfValue::String(self.parse_literal_string()?)),
            Some(b'/') => return Ok(FdfValue::Name(self.parse_name()?)),
            Some(b'[') => return self.parse_array(),
            Some(b'<') if self.src.get(self.pos + 1) == Some(&b'<') => {
                return self.parse_dictionary();
            }
            None => {
                return Err(FdfError::UnexpectedEof { offset: self.pos });
            }
            _ => {}
        }

        let start = self.pos;
        let token = self.read_token()?;
        match token {
            b"true" => Ok(FdfValue::Bool(true)),
            b"false" => Ok(FdfValue::Bool(false)),
            b"null" => Ok(FdfValue::Null),
            _ => {
                if self.consume_reference_tail(token)? {
                    return Ok(FdfValue::Null);
                }
                let text = std::str::from_utf8(token).map_err(|_| FdfError::InvalidNumber {
                    token: format!("<{}>", token.len()),
                    offset: start,
                })?;
                let number = text.parse::<f64>().map_err(|_| FdfError::InvalidNumber {
                    token: text.to_owned(),
                    offset: start,
                })?;
                if !number.is_finite() {
                    return Err(FdfError::InvalidNumber {
                        token: text.to_owned(),
                        offset: start,
                    });
                }
                Ok(FdfValue::Number(number))
            }
        }
    }

    fn parse_document(&mut self) -> Result<FdfDocument, FdfError> {
        // Only PDF whitespace may precede the header: `skip_ws` would treat the
        // `%FDF-1.7` line itself as a comment and swallow it.
        while let Some(byte) = self.peek()
            && matches!(byte, 0 | 9 | 10 | 12 | 13 | 32)
        {
            self.pos += 1;
        }
        let header_offset = self.pos;
        if self.peek() != Some(b'%') {
            return Err(FdfError::MissingHeader {
                offset: header_offset,
            });
        }
        let header_bytes = self.read_header_line();
        let header = std::str::from_utf8(header_bytes).map_err(|_| FdfError::MissingHeader {
            offset: header_offset,
        })?;
        let version_text = header
            .strip_prefix("%FDF-")
            .ok_or(FdfError::MissingHeader {
                offset: header_offset,
            })?
            .trim_end_matches(['\r', '\n']);
        let valid_version = version_text.split_once('.').is_some_and(|(major, minor)| {
            major == "1" && minor.len() == 1 && matches!(minor.as_bytes()[0], b'2'..=b'7')
        });
        if !valid_version {
            return Err(FdfError::UnsupportedVersion {
                version: version_text.to_owned(),
                offset: header_offset,
            });
        }

        let mut objects: Vec<(usize, FdfValue)> = Vec::new();
        loop {
            self.skip_layout();
            if self.at_end() {
                return Err(FdfError::Expected {
                    expected: "trailer or %%EOF",
                    offset: self.pos,
                });
            }
            if self.starts_with(b"%%EOF") {
                self.pos += 5;
                break;
            }
            if self.starts_with(b"trailer") {
                self.pos += 7;
                let _trailer = self.parse_value()?;
                self.skip_layout();
                if !self.starts_with(b"%%EOF") {
                    return Err(FdfError::Expected {
                        expected: "%%EOF after trailer",
                        offset: self.pos,
                    });
                }
                self.pos += 5;
                break;
            }

            let object_offset = self.pos;
            let _number = self.read_token()?;
            let _generation = self.read_token()?;
            let keyword = self.read_token()?;
            if keyword != b"obj" {
                return Err(FdfError::MalformedStructure {
                    detail: "expected 'obj'",
                    offset: object_offset,
                });
            }
            let value = self.parse_value()?;
            let end = self.read_token()?;
            if end != b"endobj" {
                return Err(FdfError::MalformedStructure {
                    detail: "expected 'endobj'",
                    offset: object_offset,
                });
            }
            objects.push((object_offset, value));
        }

        self.skip_layout();
        if !self.at_end() {
            return Err(FdfError::TrailingData { offset: self.pos });
        }

        let mut fields_source = None;
        for (offset, object) in &objects {
            let Some(root) = object.dictionary() else {
                continue;
            };
            let Some(fdf) = find_entry(root, "FDF").and_then(FdfValue::dictionary) else {
                continue;
            };
            if let Some(FdfValue::Array(items)) = find_entry(fdf, "Fields") {
                fields_source = Some((*offset, items));
                break;
            }
        }
        let (object_offset, items) = fields_source.ok_or(FdfError::MissingFieldsArray)?;

        let mut fields = Vec::with_capacity(items.len());
        for (index, item) in items.iter().enumerate() {
            let Some(entries) = item.dictionary() else {
                return Err(FdfError::MalformedStructure {
                    detail: "expected a field dictionary",
                    offset: object_offset,
                });
            };
            let name = match find_entry(entries, "T") {
                Some(FdfValue::String(name)) => name.clone(),
                _ => {
                    return Err(FdfError::MissingFieldName {
                        index,
                        offset: object_offset,
                    });
                }
            };
            let value = match find_entry(entries, "V") {
                None | Some(FdfValue::Null) => FieldValue::Empty,
                Some(FdfValue::String(text)) => FieldValue::Text(text.clone()),
                Some(FdfValue::Name(name)) => FieldValue::Bool(!matches!(
                    name.as_str(),
                    "Off" | "No" | "False" | "false" | "null"
                )),
                Some(FdfValue::Number(number)) => FieldValue::Number(*number),
                Some(FdfValue::Array(values)) => {
                    let mut choices = Vec::with_capacity(values.len());
                    for entry in values {
                        match entry {
                            FdfValue::String(text) => choices.push(text.clone()),
                            FdfValue::Name(name) => choices.push(name.clone()),
                            _ => {
                                return Err(FdfError::UnsupportedValue {
                                    offset: object_offset,
                                });
                            }
                        }
                    }
                    FieldValue::Choice(choices)
                }
                Some(_) => {
                    return Err(FdfError::UnsupportedValue {
                        offset: object_offset,
                    });
                }
            };
            fields.push(FdfField { name, value });
        }

        Ok(FdfDocument { fields })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{FieldFlags, FieldType};
    use crate::fill::{ChangedField, FormFill};

    fn form_with_values() -> Form {
        Form::from_fields([
            Field::new("text", FieldType::Text)
                .with_value(FieldValue::Text("a(b)\\c\nline2\ttab".into())),
            Field::new("multi", FieldType::Text)
                .with_flags(FieldFlags::MULTILINE)
                .with_value(FieldValue::Text("first\r\nsecond\nthird".into())),
            Field::new("unicode", FieldType::Text)
                .with_value(FieldValue::Text("café 你好 🌍".into())),
            Field::new("check", FieldType::CheckBox).with_value(FieldValue::Bool(true)),
            Field::new("uncheck", FieldType::CheckBox).with_value(FieldValue::Bool(false)),
            Field::new("single", FieldType::Choice)
                .with_choices(["red", "green"])
                .with_value(FieldValue::Choice(vec!["green".into()])),
            Field::new("many", FieldType::Choice)
                .with_choices(["a", "b", "c"])
                .with_value(FieldValue::Choice(vec!["a".into(), "c".into()])),
            Field::new("number", FieldType::Text).with_value(FieldValue::number(-12.75)),
            Field::new("empty", FieldType::Text),
        ])
        .unwrap()
    }

    #[test]
    fn fdf_round_trip_preserves_every_value_kind() {
        let form = form_with_values();
        let fdf = export_fdf(&form).unwrap();
        let document = import_fdf(&fdf).unwrap();

        assert_eq!(document.len(), form.len());
        let by_name: Vec<(&str, &FieldValue)> = document
            .fields()
            .iter()
            .map(|field| (field.name(), field.value()))
            .collect();
        for field in form.fields() {
            let found = by_name
                .iter()
                .find(|(name, _)| *name == field.name())
                .map(|(_, value)| *value);
            assert_eq!(found, Some(field.value()), "field {}", field.name());
        }
    }

    #[test]
    fn fdf_escapes_parentheses_backslash_and_newlines() {
        let form = form_with_values();
        let fdf = export_fdf(&form).unwrap();
        assert!(fdf.contains("\\("), "{fdf}");
        assert!(fdf.contains("\\)"), "{fdf}");
        assert!(fdf.contains("\\\\"), "{fdf}");
        assert!(fdf.contains("\\n"), "{fdf}");
        assert!(fdf.contains("\\t"), "{fdf}");
        assert!(fdf.contains("\\r"), "{fdf}");
    }

    #[test]
    fn fdf_escapes_non_ascii_bytes_as_octal() {
        let form = Form::from_fields([
            Field::new("u", FieldType::Text).with_value(FieldValue::Text("café".into()))
        ])
        .unwrap();
        let fdf = export_fdf(&form).unwrap();
        assert!(fdf.contains("\\303\\251"), "{fdf}");

        let document = import_fdf(&fdf).unwrap();
        assert_eq!(
            document.field("u").map(|field| field.value()),
            Some(&FieldValue::Text("café".into()))
        );
    }

    #[test]
    fn fdf_multiline_text_round_trips_exactly() {
        let form = form_with_values();
        let document = import_fdf(&export_fdf(&form).unwrap()).unwrap();

        assert_eq!(
            document.field("multi").map(|field| field.value()),
            Some(&FieldValue::Text("first\r\nsecond\nthird".into()))
        );
    }

    #[test]
    fn fdf_multi_choice_round_trips_as_a_string_array() {
        let form = form_with_values();
        let fdf = export_fdf(&form).unwrap();
        assert!(fdf.contains("/V [(a) (c)]"), "{fdf}");

        let document = import_fdf(&fdf).unwrap();
        assert_eq!(
            document.field("many").map(|field| field.value()),
            Some(&FieldValue::Choice(vec!["a".into(), "c".into()]))
        );
    }

    #[test]
    fn fdf_applies_to_a_form_fill_session() {
        let mut form = Form::from_fields([
            Field::new("name", FieldType::Text),
            Field::new("pick", FieldType::Choice).with_choices(["x", "y"]),
        ])
        .unwrap();
        form.set_value("name", FieldValue::Text("old".into()))
            .unwrap();

        let document = FdfDocument::new(vec![
            FdfField::new("name", FieldValue::Text("new".into())),
            FdfField::new("pick", FieldValue::Choice(vec!["y".into()])),
        ]);
        let mut fill = FormFill::new(form);
        fill.fill_from_fdf(&document).unwrap();

        let (filled, report) = fill.finish();
        assert_eq!(filled.value("name"), Ok(&FieldValue::Text("new".into())));
        assert_eq!(
            report.entries(),
            &[
                ChangedField {
                    name: "name".to_owned(),
                    old: FieldValue::Text("old".into()),
                    new: FieldValue::Text("new".into()),
                },
                ChangedField {
                    name: "pick".to_owned(),
                    old: FieldValue::Empty,
                    new: FieldValue::Choice(vec!["y".into()]),
                },
            ]
        );
    }

    #[test]
    fn fdf_skips_no_export_fields() {
        let form = Form::from_fields([
            Field::new("keep", FieldType::Text).with_value(FieldValue::Text("yes".into())),
            Field::new("skip", FieldType::Text)
                .with_flags(FieldFlags::NO_EXPORT)
                .with_value(FieldValue::Text("no".into())),
        ])
        .unwrap();
        let document = import_fdf(&export_fdf(&form).unwrap()).unwrap();

        assert_eq!(document.len(), 1);
        assert!(document.field("keep").is_some());
        assert!(document.field("skip").is_none());
    }

    #[test]
    fn export_rejects_non_finite_numbers() {
        let form = Form::from_fields([
            Field::new("nan", FieldType::Text).with_value(FieldValue::number(f64::NAN))
        ])
        .unwrap();

        assert_eq!(
            export_fdf(&form),
            Err(FdfError::NonFiniteNumber {
                field: "nan".to_owned()
            })
        );
        assert_eq!(
            export_xfdf(&form),
            Err(FdfError::NonFiniteNumber {
                field: "nan".to_owned()
            })
        );
    }

    #[test]
    fn fdf_rejects_missing_header() {
        assert_eq!(
            import_fdf("not an fdf\n1 0 obj\n<< >>\nendobj\n%%EOF\n"),
            Err(FdfError::MissingHeader { offset: 0 })
        );
    }

    #[test]
    fn fdf_rejects_unsupported_version() {
        let error = import_fdf("%FDF-2.0\n%%EOF\n").unwrap_err();
        assert!(matches!(
            error,
            FdfError::UnsupportedVersion { ref version, .. } if version == "2.0"
        ));
    }

    #[test]
    fn fdf_rejects_missing_fields_array() {
        let input = "%FDF-1.7\n1 0 obj\n<< /FDF << >> >>\nendobj\n%%EOF\n";
        assert_eq!(import_fdf(input), Err(FdfError::MissingFieldsArray));
    }

    #[test]
    fn fdf_rejects_unterminated_string() {
        let input = "%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [<< /T (oops >> >>\nendobj\n%%EOF\n";
        assert!(matches!(
            import_fdf(input),
            Err(FdfError::UnterminatedString { .. })
        ));
    }

    #[test]
    fn fdf_rejects_invalid_utf8_escape() {
        let input =
            "%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [<< /T (\\377) >>] >> >>\nendobj\n%%EOF\n";
        assert!(matches!(
            import_fdf(input),
            Err(FdfError::InvalidUtf8 { .. })
        ));
    }

    #[test]
    fn fdf_rejects_octal_escape_above_255() {
        let input =
            "%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [<< /T (\\777) >>] >> >>\nendobj\n%%EOF\n";
        assert!(matches!(
            import_fdf(input),
            Err(FdfError::InvalidOctal { .. })
        ));
    }

    #[test]
    fn fdf_rejects_trailing_data_after_eof() {
        let mut fdf = export_fdf(&Form::new()).unwrap();
        fdf.push_str("unexpected\n");
        assert!(matches!(
            import_fdf(&fdf),
            Err(FdfError::TrailingData { .. })
        ));
    }

    #[test]
    fn fdf_rejects_field_without_a_name() {
        let input = "%FDF-1.7\n1 0 obj\n<< /FDF << /Fields [<< /V (x) >>] >> >>\nendobj\n%%EOF\n";
        assert!(matches!(
            import_fdf(input),
            Err(FdfError::MissingFieldName { index: 0, .. })
        ));
    }

    #[test]
    fn fdf_export_is_canonical_and_handles_an_empty_form() {
        let form = Form::from_fields([
            Field::new("z", FieldType::Text),
            Field::new("a", FieldType::Text),
        ])
        .unwrap();
        let fdf = export_fdf(&form).unwrap();
        assert!(fdf.find("(a)").unwrap() < fdf.find("(z)").unwrap());
        assert!(fdf.starts_with("%FDF-1.7\n"));
        assert!(fdf.ends_with("%%EOF\n"));

        let document = import_fdf(&export_fdf(&Form::new()).unwrap()).unwrap();
        assert!(document.is_empty());
    }

    #[test]
    fn xfdf_export_escapes_xml_and_lists_choice_values() {
        let form = form_with_values();
        let xml = export_xfdf(&form).unwrap();
        assert!(xml.contains("<?xml version=\"1.0\" encoding=\"UTF-8\"?>"));
        assert!(xml.contains("xmlns=\"http://ns.adobe.com/xfdf/\""));
        assert!(xml.contains("a(b)\\c"));
        assert!(xml.contains("<value>a</value>"));
        assert!(xml.contains("<value>c</value>"));
        assert!(xml.contains("<value>On</value>"));
        assert!(xml.contains("<value>Off</value>"));
        assert!(xml.contains("caf\u{e9}"));
    }
}
