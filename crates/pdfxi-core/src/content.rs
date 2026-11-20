//! Bounds-checked extraction of text from PDF content streams.
//!
//! This module implements the smallest useful subset of the PDF content-stream
//! grammar that is needed for a headless text smoke test. It recognises the four
//! standard text-showing operators described by ISO 32000-1 section 9.4.3:
//!
//! - `Tj` — show a single string;
//! - `TJ` — show an array of strings and numeric kerning adjustments;
//! - `'` — move to the next line and show a string;
//! - `"` — set word/character spacing, move to the next line, and show a string.
//!
//! Every read goes through the same bounds-checked `Lexer` used by the object
//! parser, so truncated or hostile streams produce an [`Error`] instead of a
//! panic. Nesting and the number of pending operands are bounded to prevent a
//! malicious file from exhausting memory.
//!
//! When a [`FontMap`] is supplied through [`extract_text_with_fonts`], `Tf`
//! selects the active font and its `/ToUnicode` CMap (ISO 32000-1 section
//! 9.10.3) decodes every shown string, so a subset font yields its real Unicode
//! text instead of raw character codes. Fonts without a `/ToUnicode` entry keep
//! the byte-for-byte fallback of [`extract_text`]. Layout operators such as
//! `BT`, `ET`, `Td` and `Tm` are parsed but deliberately ignored: this module
//! extracts *what* is shown, not *where* or *how* it is drawn.

use std::collections::BTreeMap;
use std::sync::Arc;

use crate::lexer::Lexer;
use crate::object::Object;
use crate::tounicode::ToUnicodeMap;
use crate::{Error, Result};

/// Maximum number of operands retained before an operator is applied.
const MAX_OPERANDS: usize = 64;

/// The `/ToUnicode` mappings of the fonts named in one page's resources.
///
/// Build the map from a page resource dictionary (the core library does this in
/// [`crate::Document::page_text`]) and pass it to
/// [`extract_text_with_fonts`]. Fonts without a `/ToUnicode` entry are simply
/// absent, which selects the legacy byte-for-byte fallback.
#[derive(Debug, Clone, Default)]
pub struct FontMap {
    fonts: BTreeMap<String, Arc<ToUnicodeMap>>,
}

impl FontMap {
    /// Create an empty font map.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add the decoded `/ToUnicode` CMap for the resource name `name`.
    ///
    /// `name` is the key used by the `Tf` operator without its leading slash,
    /// for example `F4`.
    pub fn insert(&mut self, name: impl Into<String>, map: Arc<ToUnicodeMap>) {
        self.fonts.insert(name.into(), map);
    }

    /// Return the mapping registered for `name`, if any.
    pub fn get(&self, name: &str) -> Option<&ToUnicodeMap> {
        self.fonts.get(name).map(Arc::as_ref)
    }

    /// Return the number of fonts with a `/ToUnicode` mapping.
    pub fn len(&self) -> usize {
        self.fonts.len()
    }

    /// Return `true` when no font carries a `/ToUnicode` mapping.
    pub fn is_empty(&self) -> bool {
        self.fonts.is_empty()
    }
}

/// Extract the text shown by the four text-showing operators in `content`.
///
/// The returned string concatenates the strings supplied to `Tj`, `TJ`, `'` and
/// `"` in draw order. Numeric kerning adjustments inside a `TJ` array are
/// ignored because they carry no textual content. The `'` and `"` operators
/// additionally emit a line feed because they move to the next line before
/// showing their string.
///
/// Strings are decoded as UTF-16BE when they begin with the `FE FF` byte-order
/// mark; otherwise each byte is mapped directly to the Unicode code point of
/// the same value (Latin-1). ASCII text is therefore preserved verbatim, and
/// decoding can never fail on arbitrary input bytes.
///
/// # Errors
///
/// Returns an error when the stream is truncated in the middle of a token or
/// string, contains an unbalanced delimiter, or supplies more than 64 pending
/// operands.
pub fn extract_text(content: &[u8]) -> Result<String> {
    extract_text_inner(content, None)
}

/// Extract text using per-font `/ToUnicode` CMaps (ISO 32000-1 section 9.10.3).
///
/// Behaves like [`extract_text`], but tracks the font selected by `Tf` and
/// decodes each shown string through the matching [`FontMap`] entry before
/// falling back to the byte-for-byte decoding described on [`extract_text`].
/// A font that is absent from the map, or a code with no mapping, uses that
/// fallback or a U+FFFD replacement respectively; extraction never fails
/// because of an unmapped code.
///
/// # Errors
///
/// Returns the same errors as [`extract_text`].
pub fn extract_text_with_fonts(content: &[u8], fonts: &FontMap) -> Result<String> {
    extract_text_inner(content, Some(fonts))
}

/// Shared implementation for the public extraction entry points.
fn extract_text_inner(content: &[u8], fonts: Option<&FontMap>) -> Result<String> {
    let mut lexer = Lexer::new(content, 0)?;
    let mut operands: Vec<Object> = Vec::new();
    let mut out = String::new();
    let mut current_font: Option<String> = None;

    loop {
        lexer.skip_ws();
        let Some(first) = lexer.byte() else {
            break;
        };

        // Delimiter-led tokens are composite PDF values (strings, hex strings,
        // arrays, names and dictionaries). The object lexer already parses all
        // of them with bounds checks, so reuse it here.
        if matches!(first, b'(' | b'<' | b'[' | b'/') {
            let value = lexer.parse_value(0)?;
            push_operand(&mut operands, value)?;
            continue;
        }

        let token = lexer
            .read_regular()
            .ok_or_else(|| Error::UnexpectedToken("empty content-stream token".to_owned()))?;

        // A leading '-', '+' or '.' and the digits are all "regular" bytes, so
        // numbers arrive here as tokens rather than through `parse_value`.
        if let Some(number) = parse_number(token) {
            push_operand(&mut operands, number)?;
            continue;
        }

        // `Tf` sets the active font; every other operator is parsed and
        // discarded unless it is one of the four text-showing operators.
        if token == b"Tf" {
            if let Some(Object::Name(name)) = operands.first() {
                current_font = Some(name.clone());
            }
        } else {
            let map = current_font
                .as_deref()
                .and_then(|name| fonts.and_then(|map| map.get(name)));
            apply_operator(token, &operands, &mut out, map);
        }
        operands.clear();
    }

    Ok(out)
}

/// Parse a regular token as a numeric operand, if it is one.
fn parse_number(token: &[u8]) -> Option<Object> {
    let text = std::str::from_utf8(token).ok()?;
    if let Ok(value) = text.parse::<i64>() {
        return Some(Object::Integer(value));
    }
    text.parse::<f64>().ok().map(Object::Real)
}

/// Append an operand without allowing unbounded operand-stack growth.
fn push_operand(operands: &mut Vec<Object>, value: Object) -> Result<()> {
    if operands.len() >= MAX_OPERANDS {
        return Err(Error::NestingTooDeep(MAX_OPERANDS));
    }
    operands.push(value);
    Ok(())
}

/// Apply one content-stream operator using the pending operands.
fn apply_operator(
    operator: &[u8],
    operands: &[Object],
    out: &mut String,
    font: Option<&ToUnicodeMap>,
) {
    match operator {
        b"Tj" => {
            if let Some(bytes) = last_string(operands) {
                out.push_str(&decode_with(bytes, font));
            }
        }
        b"TJ" => {
            if let Some(Object::Array(items)) = operands
                .iter()
                .rev()
                .find(|value| matches!(value, Object::Array(_)))
            {
                for item in items {
                    if let Object::String(bytes) = item {
                        out.push_str(&decode_with(bytes, font));
                    }
                }
            }
        }
        b"'" | b"\"" => {
            out.push('\n');
            if let Some(bytes) = last_string(operands) {
                out.push_str(&decode_with(bytes, font));
            }
        }
        _ => {}
    }
}

/// Decode a string through the active font's CMap, or use the fallback.
fn decode_with(bytes: &[u8], font: Option<&ToUnicodeMap>) -> String {
    match font {
        Some(map) => map.decode(bytes),
        None => decode_string(bytes),
    }
}

/// Return the raw bytes of the most recent string operand, if any.
fn last_string(operands: &[Object]) -> Option<&[u8]> {
    operands.iter().rev().find_map(|value| match value {
        Object::String(bytes) => Some(bytes.as_slice()),
        _ => None,
    })
}

/// Decode a PDF string object's raw bytes into a Rust string.
fn decode_string(bytes: &[u8]) -> String {
    if bytes.len() >= 2 && bytes[0] == 0xFE && bytes[1] == 0xFF {
        let units = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extracts_tj_strings_and_ignores_layout_operators() {
        let content = b"BT /F1 12 Tf 72 720 Td (Hello) Tj (, world!) Tj ET";
        assert_eq!(extract_text(content).expect("text"), "Hello, world!");
    }

    #[test]
    fn extracts_tj_array_and_ignores_kerning_numbers() {
        let content = b"BT [(A) -120 (B) 250 (C)] TJ ET";
        assert_eq!(extract_text(content).expect("text"), "ABC");
    }

    #[test]
    fn extracts_quote_and_double_quote_operators() {
        let content = b"BT (first) Tj (second) ' 1 2 (third) \" ET";
        assert_eq!(extract_text(content).expect("text"), "first\nsecond\nthird");
    }

    #[test]
    fn handles_comments_escapes_and_hex_strings() {
        let content = b"% a comment\nBT (a\\(b\\)\\n c) Tj <48656C6C6F> Tj ET";
        assert_eq!(extract_text(content).expect("text"), "a(b)\n cHello");
    }

    #[test]
    fn decodes_utf16be_strings() {
        let content = b"BT <FEFF0048006900> Tj ET";
        assert_eq!(extract_text(content).expect("text"), "Hi");
    }

    #[test]
    fn decodes_arbitrary_bytes_without_failure() {
        let content = b"BT (\xFF\x80) Tj ET";
        assert_eq!(extract_text(content).expect("text"), "\u{00FF}\u{0080}");
    }

    #[test]
    fn rejects_truncated_literal_string() {
        let content = b"BT (unterminated Tj ET";
        assert!(matches!(extract_text(content), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn rejects_unbalanced_array() {
        let content = b"BT [(oops";
        assert!(matches!(extract_text(content), Err(Error::UnexpectedEof)));
    }

    #[test]
    fn bounds_the_operand_stack() {
        let content = b"1 2 3 4 5 6 7 8 9 10 11 12 13 14 15 16 17 18 19 20 \
                        21 22 23 24 25 26 27 28 29 30 31 32 33 34 35 36 37 38 \
                        39 40 41 42 43 44 45 46 47 48 49 50 51 52 53 54 55 56 \
                        57 58 59 60 61 62 63 64 65 66 Tj";
        assert!(matches!(
            extract_text(content),
            Err(Error::NestingTooDeep(MAX_OPERANDS))
        ));
    }
}
