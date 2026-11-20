//! Parsing and application of `/ToUnicode` CMaps (ISO 32000-1 section 9.10.3).
//!
//! A `/ToUnicode` entry maps the character codes of a font to Unicode text so a
//! consumer can copy or search the text even when the font is a subset whose
//! glyphs use arbitrary codes. This module parses the CMap stream constructs
//! that appear in practice — `begincodespacerange`, `beginbfchar` and
//! `beginbfrange` — and decodes byte strings through the resulting mapping.
//!
//! Handled forms:
//!
//! - `beginbfchar` pairs `<src> <dst>`;
//! - `beginbfrange` triples `<lo> <hi> <dst>`, where the destination is
//!   incremented once per code;
//! - `beginbfrange` with an array destination `[<dst> <dst> …]`;
//! - destinations of any length, decoded as UTF-16BE, including surrogate
//!   pairs;
//! - code-space ranges of one to four bytes, which determine how each code is
//!   segmented;
//! - unmapped codes, which become U+FFFD replacements rather than errors.
//!
//! Parsing is bounded: a hostile CMap can allocate at most
//! [`MAX_CMAP_ENTRIES`] mappings and [`MAX_CODESPACE_RANGES`] code-space
//! ranges, and every read goes through the same bounds-checked lexer used by
//! the object parser.

use std::collections::{BTreeMap, BTreeSet};

use crate::lexer::Lexer;
use crate::object::Object;
use crate::{Error, Result};

/// Maximum number of code-to-Unicode mappings accepted from one CMap.
pub const MAX_CMAP_ENTRIES: usize = 65_536;
/// Maximum number of code-space ranges accepted from one CMap.
pub const MAX_CODESPACE_RANGES: usize = 1_024;
/// Maximum number of bytes in one source character code.
pub const MAX_CODE_BYTES: usize = 4;
/// Maximum number of operands retained while parsing one CMap block.
const MAX_CMAP_OPERANDS: usize = 8;

/// One code-space range: codes of `len` bytes from `low` through `high`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CodeSpace {
    len: u8,
    low: u32,
    high: u32,
}

/// A parsed `/ToUnicode` CMap with a per-code-length Unicode mapping.
///
/// The map is immutable after parsing and can be shared between pages. Codes of
/// different widths are kept separate, so a one-byte `0x41` and a two-byte
/// `0x0041` never collide.
#[derive(Debug, Clone, Default)]
pub struct ToUnicodeMap {
    codespaces: Vec<CodeSpace>,
    entries: BTreeMap<(u8, u32), String>,
    fallback_len: u8,
}

impl ToUnicodeMap {
    /// Parse a decoded `/ToUnicode` CMap stream.
    ///
    /// The input must be the *decoded* stream payload: filters such as
    /// `FlateDecode` are applied by the caller through the stream dictionary's
    /// filter chain and [`crate::decode_stream`].
    ///
    /// A CMap with no `bfchar` or `bfrange` entries is accepted and behaves as
    /// an empty mapping.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnexpectedEof`] when the stream ends inside a block,
    /// [`Error::UnexpectedToken`] for a malformed block or a mapping that
    /// exceeds [`MAX_CMAP_ENTRIES`], and [`Error::NestingTooDeep`] when more
    /// operands than a block can consume are pending at once.
    pub fn parse(data: &[u8]) -> Result<Self> {
        let mut lexer = Lexer::new(data, 0)?;
        let mut map = Self::default();
        let mut mode = Mode::Idle;
        let mut operands: Vec<Object> = Vec::new();

        loop {
            lexer.skip_ws();
            let Some(first) = lexer.byte() else {
                break;
            };

            if matches!(first, b'(' | b'<' | b'[' | b'/') {
                let value = lexer.parse_value(0)?;
                if mode.accepts_operands() {
                    push_operand(&mut operands, value)?;
                    process_pending(mode, &mut operands, &mut map)?;
                }
                continue;
            }

            let token = lexer
                .read_regular()
                .ok_or_else(|| Error::UnexpectedToken("empty CMap token".to_owned()))?;

            if let Some(number) = parse_number(token) {
                if mode.accepts_operands() {
                    push_operand(&mut operands, number)?;
                    process_pending(mode, &mut operands, &mut map)?;
                }
                continue;
            }

            match token {
                b"begincodespacerange" => {
                    mode = Mode::CodeSpace;
                    operands.clear();
                }
                b"beginbfchar" => {
                    mode = Mode::BfChar;
                    operands.clear();
                }
                b"beginbfrange" => {
                    mode = Mode::BfRange;
                    operands.clear();
                }
                b"endcodespacerange" | b"endbfchar" | b"endbfrange" => {
                    mode = Mode::Idle;
                    operands.clear();
                }
                _ => {
                    // Names, booleans and other operator keywords either belong
                    // to the surrounding CMap dictionary or to the PostScript
                    // wrapper; none of them contribute mappings.
                    operands.clear();
                }
            }
        }

        if mode != Mode::Idle {
            return Err(Error::UnexpectedEof);
        }
        map.codespaces.sort_by_key(|space| space.len);
        map.fallback_len = map.infer_fallback_len();
        Ok(map)
    }

    /// Return the number of code-to-text mappings.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Return `true` when the CMap contained no mappings.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Decode one string of character codes into Unicode text.
    ///
    /// Codes are segmented with the CMap's code-space ranges. A code with no
    /// mapping contributes a single U+FFFD replacement character; decoding
    /// always consumes at least one byte, so arbitrary input terminates.
    pub fn decode(&self, bytes: &[u8]) -> String {
        let mut out = String::new();
        let mut index = 0;
        while index < bytes.len() {
            let remaining = &bytes[index..];
            let (len, code) = self.next_code(remaining);
            let consumed = len.max(1).min(remaining.len());
            match self.entries.get(&(consumed as u8, code)) {
                Some(text) => out.push_str(text),
                None => out.push('\u{FFFD}'),
            }
            index += consumed;
        }
        out
    }

    /// Choose the code width for the next bytes and return its numeric value.
    fn next_code(&self, remaining: &[u8]) -> (usize, u32) {
        for space in &self.codespaces {
            let len = usize::from(space.len);
            if remaining.len() >= len {
                let value = big_endian_value(&remaining[..len]);
                if value >= space.low && value <= space.high {
                    return (len, value);
                }
            }
        }
        let len = usize::from(self.fallback_len).clamp(1, remaining.len().max(1));
        (len, big_endian_value(&remaining[..len]))
    }

    /// Derive a code width for CMaps that omit `begincodespacerange`.
    fn infer_fallback_len(&self) -> u8 {
        let lengths: BTreeSet<u8> = self.entries.keys().map(|(len, _)| *len).collect();
        if lengths.len() == 1 {
            lengths.into_iter().next().unwrap_or(1)
        } else {
            1
        }
    }

    /// Insert one source-code mapping, enforcing the entry cap.
    fn insert(&mut self, len: u8, code: u32, text: String) -> Result<()> {
        if self.entries.len() >= MAX_CMAP_ENTRIES && !self.entries.contains_key(&(len, code)) {
            return Err(Error::UnexpectedToken(format!(
                "ToUnicode CMap exceeds {MAX_CMAP_ENTRIES} mappings"
            )));
        }
        self.entries.insert((len, code), text);
        Ok(())
    }
}

/// Parser state for the CMap block that is currently open.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    Idle,
    CodeSpace,
    BfChar,
    BfRange,
}

impl Mode {
    /// Return `true` when operands inside this block form a complete record.
    fn accepts_operands(self) -> bool {
        !matches!(self, Self::Idle)
    }
}

/// Parse a regular token as an integer, if it is one.
fn parse_number(token: &[u8]) -> Option<Object> {
    let text = std::str::from_utf8(token).ok()?;
    text.parse::<i64>().ok().map(Object::Integer)
}

/// Append a pending operand without allowing unbounded growth.
fn push_operand(operands: &mut Vec<Object>, value: Object) -> Result<()> {
    if operands.len() >= MAX_CMAP_OPERANDS {
        return Err(Error::NestingTooDeep(MAX_CMAP_OPERANDS));
    }
    operands.push(value);
    Ok(())
}

/// Consume one complete code-space, `bfchar` or `bfrange` record.
fn process_pending(mode: Mode, operands: &mut Vec<Object>, map: &mut ToUnicodeMap) -> Result<()> {
    match mode {
        Mode::CodeSpace if operands.len() >= 2 => {
            let low = string_operand(&operands[0]).map(<[u8]>::to_vec);
            let high = string_operand(&operands[1]).map(<[u8]>::to_vec);
            operands.clear();
            if let (Some(low), Some(high)) = (low, high) {
                map.add_codespace(&low, &high)?;
            }
        }
        Mode::BfChar if operands.len() >= 2 => {
            let source = string_operand(&operands[0]).map(<[u8]>::to_vec);
            let destination = string_operand(&operands[1]).map(<[u8]>::to_vec);
            operands.clear();
            if let (Some(source), Some(destination)) = (source, destination) {
                map.add_bfchar(&source, &destination)?;
            }
        }
        Mode::BfRange if operands.len() >= 3 => {
            let low = string_operand(&operands[0]).map(<[u8]>::to_vec);
            let high = string_operand(&operands[1]).map(<[u8]>::to_vec);
            let destination = operands[2].clone();
            operands.clear();
            if let (Some(low), Some(high)) = (low, high) {
                map.add_bfrange(&low, &high, &destination)?;
            }
        }
        _ => {}
    }
    Ok(())
}

/// Borrow the bytes of a string operand.
fn string_operand(value: &Object) -> Option<&[u8]> {
    match value {
        Object::String(bytes) => Some(bytes),
        _ => None,
    }
}

/// Parse a source code into its width and numeric value.
fn source_code(bytes: &[u8]) -> Option<(u8, u32)> {
    if bytes.is_empty() || bytes.len() > MAX_CODE_BYTES {
        return None;
    }
    Some((bytes.len() as u8, big_endian_value(bytes)))
}

/// Interpret big-endian bytes as an unsigned integer.
fn big_endian_value(bytes: &[u8]) -> u32 {
    bytes
        .iter()
        .fold(0u32, |value, &byte| (value << 8) | u32::from(byte))
}

impl ToUnicodeMap {
    /// Add one `begincodespacerange` pair.
    fn add_codespace(&mut self, low: &[u8], high: &[u8]) -> Result<()> {
        let (Some((len, low)), Some((high_len, high))) = (source_code(low), source_code(high))
        else {
            return Ok(());
        };
        if len != high_len || high < low {
            return Ok(());
        }
        if self.codespaces.len() >= MAX_CODESPACE_RANGES {
            return Err(Error::UnexpectedToken(format!(
                "ToUnicode CMap exceeds {MAX_CODESPACE_RANGES} code-space ranges"
            )));
        }
        self.codespaces.push(CodeSpace { len, low, high });
        Ok(())
    }

    /// Add one `beginbfchar` pair.
    fn add_bfchar(&mut self, source: &[u8], destination: &[u8]) -> Result<()> {
        let Some((len, code)) = source_code(source) else {
            return Ok(());
        };
        self.insert(len, code, decode_destination(destination))
    }

    /// Add one `beginbfrange` triple with a string or array destination.
    fn add_bfrange(&mut self, low: &[u8], high: &[u8], destination: &Object) -> Result<()> {
        let (Some((len, low)), Some((high_len, high))) = (source_code(low), source_code(high))
        else {
            return Ok(());
        };
        if len != high_len || high < low {
            return Ok(());
        }
        let available = MAX_CMAP_ENTRIES.saturating_sub(self.entries.len());
        let count = usize::try_from(u64::from(high - low) + 1)
            .unwrap_or(available)
            .min(available);
        let codes = || (0..count).map(move |offset| low + offset as u32);

        match destination {
            Object::String(bytes) => {
                for (offset, code) in codes().enumerate() {
                    let text = decode_destination(&increment_big_endian(bytes, offset as u32));
                    self.insert(len, code, text)?;
                }
            }
            Object::Array(items) => {
                for (offset, code) in codes().enumerate() {
                    let Some(Object::String(bytes)) = items.get(offset) else {
                        break;
                    };
                    self.insert(len, code, decode_destination(bytes))?;
                }
            }
            _ => {}
        }
        Ok(())
    }
}

/// Decode a CMap destination as UTF-16BE, falling back to Latin-1 for the odd
/// single-byte destinations produced by some legacy writers.
fn decode_destination(bytes: &[u8]) -> String {
    if bytes.is_empty() {
        return String::new();
    }
    if bytes.len().is_multiple_of(2) {
        let units = bytes
            .as_chunks::<2>()
            .0
            .iter()
            .map(|pair| u16::from_be_bytes(*pair))
            .collect::<Vec<_>>();
        return String::from_utf16_lossy(&units);
    }
    bytes.iter().map(|&byte| char::from(byte)).collect()
}

/// Increment a big-endian byte string by `delta`, carrying across bytes.
fn increment_big_endian(base: &[u8], delta: u32) -> Vec<u8> {
    let mut out = base.to_vec();
    let mut carry = u64::from(delta);
    for byte in out.iter_mut().rev() {
        if carry == 0 {
            break;
        }
        let sum = u64::from(*byte) + (carry & 0xFF);
        *byte = (sum & 0xFF) as u8;
        carry = (carry >> 8) + (sum >> 8);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"
/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Adobe-Identity-UCS def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
3 beginbfchar
<0001> <0041>
<0002> <00660069>
<0003> <D83DDE00>
endbfchar
2 beginbfrange
<0010> <0012> <0061>
<0020> <0022> [<006B> <006C> <006D>]
endbfrange
endcmap
CMapName currentdict /CMap defineresource pop
end
end
"#;

    #[test]
    fn parses_bfchar_bfrange_and_arrays() {
        let map = ToUnicodeMap::parse(SAMPLE.as_bytes()).expect("valid CMap");
        assert_eq!(map.len(), 9);
        assert_eq!(map.decode(&[0x00, 0x01]), "A");
        assert_eq!(map.decode(&[0x00, 0x02]), "fi");
        assert_eq!(map.decode(&[0x00, 0x03]), "\u{1F600}");
        assert_eq!(map.decode(&[0x00, 0x10, 0x00, 0x11, 0x00, 0x12]), "abc");
        assert_eq!(map.decode(&[0x00, 0x20, 0x00, 0x21, 0x00, 0x22]), "klm");
    }

    #[test]
    fn unmapped_codes_become_replacements_without_stalling() {
        let map = ToUnicodeMap::parse(SAMPLE.as_bytes()).expect("valid CMap");
        assert_eq!(map.decode(&[0x00, 0x02, 0x00, 0x09]), "fi\u{FFFD}");
        assert_eq!(map.decode(&[0x00]), "\u{FFFD}");
        assert_eq!(map.decode(&[]), "");
    }

    #[test]
    fn parses_one_byte_codespace() {
        let cmap = b"1 begincodespacerange <00> <FF> endcodespacerange \
                     1 beginbfchar <41> <0058> endbfchar";
        let map = ToUnicodeMap::parse(cmap).expect("valid CMap");
        assert_eq!(map.decode(&[0x41, 0x42]), "X\u{FFFD}");
    }

    #[test]
    fn rejects_a_truncated_block() {
        let cmap = b"1 beginbfchar <0001> <0041>";
        assert!(matches!(
            ToUnicodeMap::parse(cmap),
            Err(Error::UnexpectedEof)
        ));
    }

    #[test]
    fn increments_carry_across_destination_bytes() {
        assert_eq!(increment_big_endian(&[0x00, 0xFF], 1), vec![0x01, 0x00]);
        assert_eq!(increment_big_endian(&[0x00, 0x40], 3), vec![0x00, 0x43]);
    }

    #[test]
    fn bounds_the_mapping_table() {
        let mut map = ToUnicodeMap::default();
        for code in 0..MAX_CMAP_ENTRIES as u32 {
            map.insert(2, code, String::from("x")).expect("within cap");
        }
        assert!(matches!(
            map.insert(2, MAX_CMAP_ENTRIES as u32, String::from("x")),
            Err(Error::UnexpectedToken(_))
        ));
    }
}
