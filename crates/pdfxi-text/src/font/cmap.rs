//! Parsers for the numeric tables a font dictionary points at.
//!
//! This module turns the PDF representations of `/W`, `/ToUnicode`, and
//! `/CIDToGIDMap` into validated, queryable structures. Every parser treats its
//! input as hostile: lengths are bounded, integers are range-checked, and all
//! failures are reported as typed [`FontError`] values rather than panics.

use std::collections::BTreeMap;

use super::FontError;

/// One element of a PDF `/W` array.
///
/// The `/W` array alternates a starting CID with either a bracketed list of
/// widths or a terminating CID and a single width. This enum mirrors that shape
/// without requiring a general PDF object model.
#[derive(Clone, Debug, PartialEq)]
pub enum WToken {
    /// A single number, interpreted as a CID or a width depending on position.
    Number(f32),
    /// A bracketed `[w1 w2 ...]` list of consecutive-CID widths.
    Array(Vec<f32>),
}

/// A validated CID-width table parsed from a font's `/W` array.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct CidWidths {
    widths: BTreeMap<u16, f32>,
}

impl CidWidths {
    /// Parse a `/W` array into a CID-to-width map.
    ///
    /// Two forms are accepted:
    ///
    /// - `c [w1 w2 ... wn]` assigns `w1..wn` to CIDs `c..c+n-1`.
    /// - `c_first c_last w` assigns `w` to every CID in `c_first..=c_last`.
    ///
    /// CIDs must be integers in `0..=65535`. A CID whose implied index would
    /// exceed `65535`, a descending range, an odd token tail, or a non-finite
    /// width is rejected.
    pub fn parse(tokens: &[WToken]) -> Result<Self, FontError> {
        let mut widths = BTreeMap::new();
        let mut index = 0usize;
        while index < tokens.len() {
            match &tokens[index] {
                WToken::Number(raw_cid) => {
                    let cid = cid_from_number(*raw_cid, "W")?;
                    if index + 1 >= tokens.len() {
                        return Err(FontError::Invalid {
                            field: "W",
                            reason: "dangling CID with no width array or range end".into(),
                        });
                    }
                    match &tokens[index + 1] {
                        WToken::Array(values) => {
                            if values.is_empty() {
                                return Err(FontError::Invalid {
                                    field: "W",
                                    reason: "empty width array".into(),
                                });
                            }
                            let start = usize::from(cid);
                            for (offset, value) in values.iter().enumerate() {
                                check_width(*value)?;
                                let next = start.saturating_add(offset);
                                let mapped =
                                    u16::try_from(next).map_err(|_| FontError::Invalid {
                                        field: "W",
                                        reason: "CID array exceeds 65535".into(),
                                    })?;
                                let _ = widths.insert(mapped, *value);
                            }
                            index += 2;
                        }
                        WToken::Number(raw_last) => {
                            let last = cid_from_number(*raw_last, "W")?;
                            if index + 2 >= tokens.len() {
                                return Err(FontError::Invalid {
                                    field: "W",
                                    reason: "CID range has no width".into(),
                                });
                            }
                            let width = match &tokens[index + 2] {
                                WToken::Number(value) => *value,
                                WToken::Array(_) => {
                                    return Err(FontError::Invalid {
                                        field: "W",
                                        reason: "CID range width must be a single number".into(),
                                    });
                                }
                            };
                            check_width(width)?;
                            if cid > last {
                                return Err(FontError::Invalid {
                                    field: "W",
                                    reason: "descending CID range".into(),
                                });
                            }
                            for mapped in cid..=last {
                                let _ = widths.insert(mapped, width);
                            }
                            index += 3;
                        }
                    }
                }
                WToken::Array(_) => {
                    return Err(FontError::Invalid {
                        field: "W",
                        reason: "width array without a leading CID".into(),
                    });
                }
            }
        }
        Ok(Self { widths })
    }

    /// Return the explicit width for `cid`, or `None` when `/W` omitted it (the
    /// caller should then use `/DW`).
    pub fn width_for(&self, cid: u16) -> Option<f32> {
        self.widths.get(&cid).copied()
    }

    /// Return the number of explicitly mapped CIDs.
    pub fn len(&self) -> usize {
        self.widths.len()
    }

    /// Return `true` when no CIDs have explicit widths.
    pub fn is_empty(&self) -> bool {
        self.widths.is_empty()
    }
}

/// A CID-to-glyph-index map from a font's `/CIDToGIDMap` entry.
#[derive(Clone, Debug, PartialEq)]
pub enum CidToGidMap {
    /// The `/Identity` map: every CID maps to the GID of the same value.
    Identity,
    /// An explicit stream of big-endian 16-bit GIDs, indexed by CID.
    Explicit(Vec<u16>),
}

impl CidToGidMap {
    /// Parse an explicit `/CIDToGIDMap` stream.
    ///
    /// The stream is a sequence of 16-bit big-endian GIDs; byte `2*cid` and
    /// `2*cid+1` hold the GID for `cid`. An odd byte count is rejected.
    pub fn parse_stream(bytes: &[u8]) -> Result<Self, FontError> {
        if !bytes.len().is_multiple_of(2) {
            return Err(FontError::OddCidToGidLength { len: bytes.len() });
        }
        let (pairs, _remainder) = bytes.as_chunks::<2>();
        let mut values = Vec::with_capacity(pairs.len());
        for pair in pairs {
            values.push(u16::from_be_bytes([pair[0], pair[1]]));
        }
        Ok(Self::Explicit(values))
    }

    /// Return the GID for `cid`, or `None` when the map has no entry.
    pub fn gid_for(&self, cid: u16) -> Option<u16> {
        match self {
            Self::Identity => Some(cid),
            Self::Explicit(values) => values.get(usize::from(cid)).copied(),
        }
    }

    /// Return the number of GIDs in an explicit map (`usize::MAX` for Identity).
    pub fn len(&self) -> usize {
        match self {
            Self::Identity => usize::MAX,
            Self::Explicit(values) => values.len(),
        }
    }

    /// Return `true` when an explicit map contains no GIDs.
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Explicit(values) if values.is_empty())
    }
}

/// A parsed `/ToUnicode` CMap, mapping source codes to Unicode strings.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct ToUnicodeCMap {
    map: BTreeMap<u16, String>,
}

impl ToUnicodeCMap {
    /// Parse the byte text of a `/ToUnicode` CMap stream.
    ///
    /// Only the `bfchar` and `bfrange` operators are consumed; all other CMap
    /// operators are ignored. Source codes are one or two big-endian bytes;
    /// destination strings are interpreted as UTF-16BE. `bfrange` supports both
    /// the sequential-destination form and the explicit destination-array form.
    pub fn parse(bytes: &[u8]) -> Result<Self, FontError> {
        let tokens = lex(bytes)?;
        let mut map = BTreeMap::new();
        let mut index = 0usize;
        while index < tokens.len() {
            match &tokens[index].0 {
                Token::Word(word) if word == "beginbfchar" => {
                    let count = parse_count(&tokens, index)?;
                    index += 1;
                    for _ in 0..count {
                        let source = expect_hex(&tokens, index)?;
                        index += 1;
                        let destination = expect_hex(&tokens, index)?;
                        index += 1;
                        let cid = source_cid(source, tokens[index - 2].1)?;
                        let text =
                            utf16be_to_string(destination).ok_or_else(|| FontError::ToUnicode {
                                offset: tokens[index - 1].1,
                                reason: "invalid UTF-16BE destination in bfchar".into(),
                            })?;
                        let _ = map.insert(cid, text);
                    }
                    expect_word(&tokens, index, "endbfchar")?;
                    index += 1;
                }
                Token::Word(word) if word == "beginbfrange" => {
                    let count = parse_count(&tokens, index)?;
                    index += 1;
                    for _ in 0..count {
                        let low_token = expect_hex(&tokens, index)?;
                        let low_offset = tokens[index].1;
                        index += 1;
                        let high_token = expect_hex(&tokens, index)?;
                        index += 1;
                        let low = source_cid(low_token, low_offset)?;
                        let high = source_cid(high_token, tokens[index - 1].1)?;
                        if low > high {
                            return Err(FontError::ToUnicode {
                                offset: low_offset,
                                reason: "descending bfrange".into(),
                            });
                        }
                        match &tokens.get(index).map(|entry| &entry.0) {
                            Some(Token::Hex(destination)) => {
                                let mut units = utf16be_units(destination).ok_or_else(|| {
                                    FontError::ToUnicode {
                                        offset: tokens[index].1,
                                        reason: "invalid UTF-16BE destination in bfrange".into(),
                                    }
                                })?;
                                for cid in low..=high {
                                    let text = units_to_string(&units).ok_or_else(|| {
                                        FontError::ToUnicode {
                                            offset: tokens[index].1,
                                            reason: "invalid surrogate in bfrange".into(),
                                        }
                                    })?;
                                    let _ = map.insert(cid, text);
                                    if let Some(last) = units.last_mut() {
                                        *last = last.saturating_add(1);
                                    }
                                }
                                index += 1;
                            }
                            Some(Token::Word(word)) if word == "[" => {
                                index += 1;
                                let mut cid = low;
                                while index < tokens.len()
                                    && !matches!(&tokens[index].0, Token::Word(w) if w == "]")
                                {
                                    let destination = expect_hex(&tokens, index)?;
                                    let offset = tokens[index].1;
                                    index += 1;
                                    let text = utf16be_to_string(destination).ok_or_else(|| {
                                        FontError::ToUnicode {
                                            offset,
                                            reason: "invalid UTF-16BE destination in bfrange array"
                                                .into(),
                                        }
                                    })?;
                                    if cid <= high {
                                        let _ = map.insert(cid, text);
                                    }
                                    cid = cid.saturating_add(1);
                                }
                                expect_word(&tokens, index, "]")?;
                                index += 1;
                            }
                            _ => {
                                return Err(FontError::ToUnicode {
                                    offset: tokens
                                        .get(index)
                                        .map(|entry| entry.1)
                                        .unwrap_or(bytes.len()),
                                    reason: "bfrange needs a destination string or array".into(),
                                });
                            }
                        }
                    }
                    expect_word(&tokens, index, "endbfrange")?;
                    index += 1;
                }
                _ => index += 1,
            }
        }
        Ok(Self { map })
    }

    /// Return the Unicode string mapped to source code `code`, if any.
    pub fn unicode_for(&self, code: u16) -> Option<&str> {
        self.map.get(&code).map(String::as_str)
    }

    /// Return the number of mapped source codes.
    pub fn len(&self) -> usize {
        self.map.len()
    }

    /// Return `true` when the CMap maps no source codes.
    pub fn is_empty(&self) -> bool {
        self.map.is_empty()
    }
}

/// A lexical token with the byte offset where it began.
#[derive(Debug, Clone, PartialEq)]
enum Token {
    /// A `<hex>` string, decoded to bytes.
    Hex(Vec<u8>),
    /// A keyword, name, number, or punctuation marker.
    Word(String),
}

/// Tokenize a CMap byte stream. Comments (`%` to end of line) are ignored.
fn lex(bytes: &[u8]) -> Result<Vec<(Token, usize)>, FontError> {
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < bytes.len() {
        let byte = bytes[index];
        if byte.is_ascii_whitespace() {
            index += 1;
            continue;
        }
        if byte == b'%' {
            while index < bytes.len() && bytes[index] != b'\n' && bytes[index] != b'\r' {
                index += 1;
            }
            continue;
        }
        let start = index;
        if byte == b'<' {
            if index + 1 < bytes.len() && bytes[index + 1] == b'<' {
                out.push((Token::Word("<<".into()), start));
                index += 2;
                continue;
            }
            index += 1;
            let mut hex = Vec::new();
            while index < bytes.len() && bytes[index] != b'>' {
                let current = bytes[index];
                if current.is_ascii_whitespace() {
                    index += 1;
                    continue;
                }
                let value = hex_value(current).ok_or_else(|| FontError::ToUnicode {
                    offset: index,
                    reason: "non-hex byte inside <...> string".into(),
                })?;
                hex.push(value);
                index += 1;
            }
            if index >= bytes.len() {
                return Err(FontError::ToUnicode {
                    offset: start,
                    reason: "unterminated hex string".into(),
                });
            }
            index += 1; // consume the closing '>'
            let (pairs, remainder) = hex.as_chunks::<2>();
            let mut decoded = Vec::with_capacity(hex.len().div_ceil(2));
            for pair in pairs {
                decoded.push((pair[0] << 4) | pair[1]);
            }
            if let Some(odd) = remainder.first() {
                // PDF pads an odd final hex digit with zero.
                decoded.push(odd << 4);
            }
            out.push((Token::Hex(decoded), start));
        } else if byte == b'>' && index + 1 < bytes.len() && bytes[index + 1] == b'>' {
            out.push((Token::Word(">>".into()), start));
            index += 2;
        } else if byte == b'[' || byte == b']' {
            out.push((Token::Word((byte as char).to_string()), start));
            index += 1;
        } else if matches!(byte, b'(' | b')' | b'{' | b'}') {
            // Literal strings and dictionaries are not used by bfchar/bfrange.
            index += 1;
        } else {
            let mut word = Vec::new();
            while index < bytes.len()
                && !bytes[index].is_ascii_whitespace()
                && !matches!(
                    bytes[index],
                    b'<' | b'>' | b'[' | b']' | b'(' | b')' | b'{' | b'}'
                )
            {
                word.push(bytes[index]);
                index += 1;
            }
            if word.is_empty() {
                index += 1;
                continue;
            }
            // Lossy conversion is safe here: CMap operators are ASCII, and any
            // non-ASCII word is ignored by the parser rather than interpreted.
            out.push((
                Token::Word(String::from_utf8_lossy(&word).into_owned()),
                start,
            ));
        }
    }
    Ok(out)
}

/// Convert a hex nibble to its value.
fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Read the decimal count immediately preceding `beginbfchar`/`beginbfrange`.
fn parse_count(tokens: &[(Token, usize)], index: usize) -> Result<usize, FontError> {
    if index == 0 {
        return Err(FontError::ToUnicode {
            offset: tokens[index].1,
            reason: "missing count before beginbfchar/beginbfrange".into(),
        });
    }
    match &tokens[index - 1].0 {
        Token::Word(word) => word.parse::<usize>().map_err(|_| FontError::ToUnicode {
            offset: tokens[index - 1].1,
            reason: "invalid bfchar/bfrange count".into(),
        }),
        Token::Hex(_) => Err(FontError::ToUnicode {
            offset: tokens[index - 1].1,
            reason: "bfchar/bfrange count must be a decimal number".into(),
        }),
    }
}

/// Require a hex token at `index`.
fn expect_hex(tokens: &[(Token, usize)], index: usize) -> Result<&[u8], FontError> {
    match tokens.get(index) {
        Some((Token::Hex(bytes), _)) => Ok(bytes),
        Some((_, offset)) => Err(FontError::ToUnicode {
            offset: *offset,
            reason: "expected a <hex> string".into(),
        }),
        None => Err(FontError::ToUnicode {
            offset: 0,
            reason: "unexpected end of CMap".into(),
        }),
    }
}

/// Require a word token equal to `expected` at `index`.
fn expect_word(tokens: &[(Token, usize)], index: usize, expected: &str) -> Result<(), FontError> {
    match tokens.get(index) {
        Some((Token::Word(word), _)) if word == expected => Ok(()),
        Some((_, offset)) => Err(FontError::ToUnicode {
            offset: *offset,
            reason: format!("expected `{expected}`"),
        }),
        None => Err(FontError::ToUnicode {
            offset: 0,
            reason: format!("expected `{expected}` but CMap ended"),
        }),
    }
}

/// Convert a one- or two-byte source code to a `u16` CID.
fn source_cid(bytes: &[u8], offset: usize) -> Result<u16, FontError> {
    match bytes.len() {
        1 => Ok(u16::from(bytes[0])),
        2 => Ok(u16::from_be_bytes([bytes[0], bytes[1]])),
        _ => Err(FontError::ToUnicode {
            offset,
            reason: "source code must be one or two bytes".into(),
        }),
    }
}

/// Decode a UTF-16BE byte string into a Rust string.
fn utf16be_to_string(bytes: &[u8]) -> Option<String> {
    units_to_string(&utf16be_units(bytes)?)
}

/// Split a UTF-16BE byte string into 16-bit code units.
///
/// Odd-length strings are treated as Latin-1 bytes (a common non-conforming
/// CMap pattern) rather than rejected, so such maps still produce useful text.
fn utf16be_units(bytes: &[u8]) -> Option<Vec<u16>> {
    if bytes.is_empty() {
        return Some(Vec::new());
    }
    if bytes.len().is_multiple_of(2) {
        let (pairs, _remainder) = bytes.as_chunks::<2>();
        let mut units = Vec::with_capacity(pairs.len());
        for pair in pairs {
            units.push(u16::from_be_bytes([pair[0], pair[1]]));
        }
        Some(units)
    } else {
        Some(bytes.iter().map(|byte| u16::from(*byte)).collect())
    }
}

/// Decode UTF-16 code units, rejecting unpaired surrogates.
fn units_to_string(units: &[u16]) -> Option<String> {
    let mut result = String::new();
    for unit in std::char::decode_utf16(units.iter().copied()) {
        result.push(unit.ok()?);
    }
    Some(result)
}

/// Convert a number token to a CID, rejecting non-integers and out-of-range.
fn cid_from_number(value: f32, field: &'static str) -> Result<u16, FontError> {
    if !value.is_finite() || value.fract() != 0.0 || value < 0.0 || value > f32::from(u16::MAX) {
        return Err(FontError::Invalid {
            field,
            reason: format!("{value} is not a CID in 0..=65535"),
        });
    }
    Ok(value as u16)
}

/// Reject a non-finite or negative glyph width.
fn check_width(value: f32) -> Result<(), FontError> {
    if value.is_finite() && value >= 0.0 {
        Ok(())
    } else {
        Err(FontError::Invalid {
            field: "width",
            reason: format!("{value} is not a finite, non-negative width"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cid_widths_parse_arrays_and_ranges() {
        let tokens = vec![
            WToken::Number(10.0),
            WToken::Array(vec![500.0, 600.0, 700.0]),
            WToken::Number(20.0),
            WToken::Number(22.0),
            WToken::Number(900.0),
        ];
        let widths = CidWidths::parse(&tokens).expect("valid /W array");
        assert_eq!(widths.len(), 6);
        assert_eq!(widths.width_for(10), Some(500.0));
        assert_eq!(widths.width_for(11), Some(600.0));
        assert_eq!(widths.width_for(12), Some(700.0));
        assert_eq!(widths.width_for(20), Some(900.0));
        assert_eq!(widths.width_for(21), Some(900.0));
        assert_eq!(widths.width_for(22), Some(900.0));
        assert_eq!(widths.width_for(13), None);
        assert_eq!(widths.width_for(19), None);
    }

    #[test]
    fn cid_widths_reject_malformed_tables() {
        assert!(CidWidths::parse(&[WToken::Number(5.0)]).is_err());
        assert!(CidWidths::parse(&[WToken::Array(vec![1.0])]).is_err());
        assert!(
            CidWidths::parse(&[
                WToken::Number(10.0),
                WToken::Number(5.0),
                WToken::Number(1.0),
            ])
            .is_err()
        );
        assert!(CidWidths::parse(&[WToken::Number(10.0), WToken::Array(vec![]),]).is_err());
        assert!(CidWidths::parse(&[WToken::Number(70_000.0), WToken::Array(vec![1.0]),]).is_err());
        assert!(CidWidths::parse(&[WToken::Number(10.0), WToken::Array(vec![f32::NAN]),]).is_err());
    }

    #[test]
    fn cid_to_gid_map_parses_stream_and_identity() {
        let map = CidToGidMap::parse_stream(&[0x00, 0x0a, 0x01, 0x02]).expect("even stream");
        assert_eq!(map.gid_for(0), Some(10));
        assert_eq!(map.gid_for(1), Some(258));
        assert_eq!(map.gid_for(2), None);
        assert_eq!(CidToGidMap::Identity.gid_for(42), Some(42));
        assert!(matches!(
            CidToGidMap::parse_stream(&[0x00]),
            Err(FontError::OddCidToGidLength { len: 1 })
        ));
    }

    #[test]
    fn tounicode_non_bmp_surrogate_pairs_and_lone_surrogate_rejection() {
        // <D83DDE00> is the UTF-16BE surrogate pair for U+1F600 (grinning face).
        let cmap = b"2 beginbfchar\n\
                     <01> <D83DDE00>\n\
                     <02> <0041>\n\
                     endbfchar\n\
                     2 beginbfrange\n\
                     <10> <11> <D83DDE00>\n\
                     <20> <21> [<D83DDE00> <D83DDE02>]\n\
                     endbfrange\n";
        let parsed = ToUnicodeCMap::parse(cmap).expect("valid surrogate-pair CMap");
        assert_eq!(parsed.unicode_for(0x01), Some("\u{1F600}"));
        assert_eq!(parsed.unicode_for(0x02), Some("A"));
        // Sequential bfrange increments the trailing low surrogate.
        assert_eq!(parsed.unicode_for(0x10), Some("\u{1F600}"));
        assert_eq!(parsed.unicode_for(0x11), Some("\u{1F601}"));
        // Array bfrange keeps each destination's own surrogate pair.
        assert_eq!(parsed.unicode_for(0x20), Some("\u{1F600}"));
        assert_eq!(parsed.unicode_for(0x21), Some("\u{1F602}"));

        // An unpaired high surrogate must be a typed error, not a replacement
        // character or a panic, in both bfchar and bfrange destinations.
        let lone_bfchar = b"1 beginbfchar\n<30> <D83D>\nendbfchar\n";
        assert!(matches!(
            ToUnicodeCMap::parse(lone_bfchar),
            Err(FontError::ToUnicode { .. })
        ));
        let lone_bfrange = b"1 beginbfrange\n<40> <41> <D83D>\nendbfrange\n";
        assert!(matches!(
            ToUnicodeCMap::parse(lone_bfrange),
            Err(FontError::ToUnicode { .. })
        ));
    }
}
