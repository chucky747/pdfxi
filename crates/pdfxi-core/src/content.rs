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
//! malicious file from exhausting memory. Other content-stream operators (for
//! example `BT`, `ET`, `Tf`, and `Td`) are parsed but deliberately ignored: this
//! module extracts *what* is shown, not *where* or *how* it is drawn.

use crate::lexer::Lexer;
use crate::object::Object;
use crate::{Error, Result};

/// Maximum number of operands retained before an operator is applied.
const MAX_OPERANDS: usize = 64;

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
    let mut lexer = Lexer::new(content, 0)?;
    let mut operands: Vec<Object> = Vec::new();
    let mut out = String::new();

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

        // Anything else is an operator keyword. Only the four text-showing
        // operators consume operands; all others are parsed and discarded.
        apply_operator(token, &operands, &mut out);
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
fn apply_operator(operator: &[u8], operands: &[Object], out: &mut String) {
    match operator {
        b"Tj" => {
            if let Some(bytes) = last_string(operands) {
                out.push_str(&decode_string(bytes));
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
                        out.push_str(&decode_string(bytes));
                    }
                }
            }
        }
        b"'" | b"\"" => {
            out.push('\n');
            if let Some(bytes) = last_string(operands) {
                out.push_str(&decode_string(bytes));
            }
        }
        _ => {}
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
