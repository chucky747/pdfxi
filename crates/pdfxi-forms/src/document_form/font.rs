//! Resolution and encoding of the font named by an AcroForm field's `/DA`.
//!
//! Appearance generation must draw the value in the font the document declares,
//! not in an unconditional Helvetica substitute. This module:
//!
//! 1. resolves the `/DA` font resource name through the widget page's
//!    `/Resources /Font` and then the AcroForm `/DR /Font` (both may hold
//!    indirect references);
//! 2. classifies the font as a simple font (Type1, TrueType, MMType1) or an
//!    identity-encoded composite font (Type0 with `Identity-H`/`Identity-V`);
//! 3. records whether a font program is embedded through the `/FontDescriptor`
//!    `FontFile`, `FontFile2` or `FontFile3` entries (the descendant descriptor
//!    for composite fonts);
//! 4. builds a Unicode-to-code map from the `/ToUnicode` CMap when present,
//!    otherwise from WinAnsi for simple fonts;
//! 5. encodes a value into literal string bytes (simple fonts) or two-byte
//!    hexadecimal CIDs (composite fonts).
//!
//! Anything it cannot encode is reported, never guessed: the caller falls back
//! to the base-14 Helvetica appearance and surfaces the reason through
//! [`AppearanceFont::Fallback`](super::AppearanceFont).
//!
//! # Limits
//!
//! - Only identity-encoded composite fonts are encoded. A named CMap such as
//!   `UniJIS-UCS2-H` has a code space this module does not model, so those fonts
//!   always take the fallback rather than emitting codes that might be wrong.
//! - `/ToUnicode` destinations of more than one character (ligatures, combining
//!   sequences) are not encoded; the fallback is used.
//! - Glyph coverage inside the embedded program is not inspected. An embedded
//!   simple font with WinAnsi is trusted for the bytes WinAnsi defines, which is
//!   the same assumption a PDF viewer makes for a well-formed font.
//! - `MacRomanEncoding`, `StandardEncoding`, `PDFDocEncoding` and custom
//!   `/Differences` arrays are honoured only for printable ASCII, where those
//!   encodings agree. Other characters take the fallback.

use std::collections::BTreeMap;

use pdfxi_core::{Dictionary, Document, Filter, Object};

use super::{AppearanceFont, FallbackReason, FormField};
use crate::appearance::{EncodedText, TextEncoder};

/// Maximum number of reverse CMap entries retained from one `/ToUnicode` stream.
const MAX_REVERSE_CMAP_ENTRIES: usize = 65_536;

/// The outcome of resolving a field's `/DA` font resource.
pub(super) enum FontChoice {
    /// The font exists and can be referenced by the appearance.
    Field(FieldFont),
    /// No usable font was found; the caller must use the base-14 path.
    Fallback(FallbackReason),
}

/// A resolved `/DA` font and the encoder for its character codes.
pub(super) struct FieldFont {
    /// Resource name written after the `Tf` operator.
    pub resource: String,
    /// The font object referenced from the appearance stream resources.
    pub object: Object,
    /// Whether a font program is embedded in the document.
    pub embedded: bool,
    /// The Unicode-to-code map in effect.
    kind: EncodingKind,
}

impl FieldFont {
    /// Returns the report value for this font.
    pub(super) fn report(&self) -> AppearanceFont {
        AppearanceFont::FieldFont {
            resource: self.resource.clone(),
            embedded: self.embedded,
        }
    }
}

impl TextEncoder for FieldFont {
    fn encode(&self, text: &str) -> Option<EncodedText> {
        self.kind.encode(text)
    }
}

/// How characters map to the codes of a resolved font.
enum EncodingKind {
    /// One-byte character codes emitted as a literal string.
    Simple(BTreeMap<char, u8>),
    /// Two-byte CIDs emitted as a hexadecimal string.
    Composite(BTreeMap<char, u16>),
}

impl EncodingKind {
    fn encode(&self, text: &str) -> Option<EncodedText> {
        match self {
            Self::Simple(codes) => {
                let mut bytes = Vec::with_capacity(text.len());
                for ch in text.chars() {
                    bytes.push(*codes.get(&ch)?);
                }
                Some(EncodedText::Literal(bytes))
            }
            Self::Composite(codes) => {
                let mut bytes = Vec::with_capacity(text.len().saturating_mul(2));
                for ch in text.chars() {
                    bytes.extend_from_slice(&codes.get(&ch)?.to_be_bytes());
                }
                Some(EncodedText::Hex(bytes))
            }
        }
    }
}

/// Resolves the `/DA` resource `requested` for `field`.
///
/// The widget's page resources are searched first (that is what a viewer uses
/// to paint the widget), then the AcroForm `/DR /Font` dictionary. A font that
/// resolves but has no usable encoding is still returned as
/// [`FontChoice::Field`]; the caller tests the actual value through
/// [`TextEncoder::encode`] and reports [`FallbackReason::CannotEncode`].
pub(super) fn resolve_font(
    doc: &Document,
    acroform: &Dictionary,
    field: &FormField,
    requested: &str,
) -> FontChoice {
    let requested = sanitise_name(requested);
    let page_index = field.widgets.first().and_then(|widget| widget.page);
    let mut candidates: Vec<(String, Object)> = Vec::new();
    if let Some(fonts) = page_fonts(doc, page_index)
        && let Some(object) = fonts.get(&requested)
    {
        candidates.push((requested.clone(), object.clone()));
    }
    if candidates.is_empty()
        && let Some(fonts) = document_fonts(doc, acroform)
        && let Some(object) = fonts.get(&requested)
    {
        candidates.push((requested.clone(), object.clone()));
    }
    let Some((resource, object)) = candidates.into_iter().next() else {
        return FontChoice::Fallback(FallbackReason::MissingResource);
    };
    let Some(dictionary) = dictionary_of(doc, &object) else {
        return FontChoice::Fallback(FallbackReason::UnsupportedFont);
    };
    let Some(kind) = encoding_for(doc, &dictionary) else {
        return FontChoice::Fallback(FallbackReason::UnsupportedFont);
    };
    let embedded = font_is_embedded(doc, &dictionary);
    FontChoice::Field(FieldFont {
        resource,
        object,
        embedded,
        kind,
    })
}

/// Returns the sanitised resource name used by the appearance.
///
/// PDF names may contain almost any byte; the appearance generator only emits
/// `Tf` operands for ASCII alphanumeric names, so anything else is reduced to a
/// safe identifier. The sanitised name is used for lookup *and* for the written
/// resource, so the two always agree.
pub(super) fn sanitise_name(requested: &str) -> String {
    let candidate: String = requested
        .chars()
        .filter(|ch| ch.is_ascii_alphanumeric() || *ch == '-' || *ch == '_')
        .collect();
    if candidate.is_empty() {
        "Helv".to_owned()
    } else {
        candidate
    }
}

/// Returns the page `/Resources /Font` dictionary for `page_index`.
fn page_fonts(doc: &Document, page_index: Option<usize>) -> Option<Dictionary> {
    let index = page_index?;
    let page = doc.page(index).ok()?;
    let resources = match page.resources {
        Some(id) => doc.object(id).ok()?,
        None => doc
            .object(page.id)
            .ok()?
            .as_dictionary()?
            .get("Resources")?
            .clone(),
    };
    resources
        .as_dictionary()
        .and_then(|resources| resources.get("Font"))
        .and_then(|fonts| dictionary_of(doc, fonts))
}

/// Returns the AcroForm `/DR /Font` dictionary.
fn document_fonts(doc: &Document, acroform: &Dictionary) -> Option<Dictionary> {
    acroform
        .get("DR")
        .and_then(|dr| dictionary_of(doc, dr))
        .and_then(|dr| dr.get("Font").cloned())
        .and_then(|fonts| dictionary_of(doc, &fonts))
}

/// Resolves an object to a dictionary, following at most one reference.
fn dictionary_of(doc: &Document, object: &Object) -> Option<Dictionary> {
    match object {
        Object::Reference(id) => doc.object(*id).ok()?.as_dictionary().cloned(),
        other => other.as_dictionary().cloned(),
    }
}

/// Returns `true` when a font program is embedded for `font`.
fn font_is_embedded(doc: &Document, font: &Dictionary) -> bool {
    let descriptor = match font.get("FontDescriptor") {
        Some(object) => dictionary_of(doc, object),
        None => descendant_dictionary(doc, font)
            .and_then(|descendant| descendant.get("FontDescriptor").cloned())
            .and_then(|object| dictionary_of(doc, &object)),
    };
    descriptor.is_some_and(|descriptor| {
        ["FontFile", "FontFile2", "FontFile3"]
            .iter()
            .any(|key| descriptor.contains_key(*key))
    })
}

/// Returns the first descendant font dictionary of a composite font.
fn descendant_dictionary(doc: &Document, font: &Dictionary) -> Option<Dictionary> {
    font.get("DescendantFonts")
        .and_then(Object::as_array)
        .and_then(|descendants| descendants.first())
        .and_then(|object| dictionary_of(doc, object))
}

/// Builds the character-code map for a font dictionary.
fn encoding_for(doc: &Document, font: &Dictionary) -> Option<EncodingKind> {
    let subtype = font.get("Subtype").and_then(Object::as_name).unwrap_or("");
    match subtype {
        "Type0" => {
            let encoding = font.get("Encoding").and_then(Object::as_name)?;
            if !matches!(encoding, "Identity-H" | "Identity-V") {
                return None;
            }
            let codes = font
                .get("ToUnicode")
                .and_then(|object| to_unicode_codes(doc, object))?;
            Some(EncodingKind::Composite(codes))
        }
        "Type1" | "TrueType" | "MMType1" => {
            let codes = match font.get("ToUnicode") {
                Some(object) => simple_codes_from_cid(to_unicode_codes(doc, object)?),
                None => simple_codes(font),
            };
            Some(EncodingKind::Simple(codes))
        }
        _ => None,
    }
}

/// Converts a two-byte reverse map into a one-byte simple-font map.
///
/// Codes above `0xFF` cannot be written by a simple font; they are dropped.
/// The map is only useful when every needed character has a small code, and the
/// caller's encoding test rejects anything else.
fn simple_codes_from_cid(codes: BTreeMap<char, u16>) -> BTreeMap<char, u8> {
    codes
        .into_iter()
        .filter_map(|(ch, code)| u8::try_from(code).ok().map(|code| (ch, code)))
        .collect()
}

/// Builds a simple-font code map from its encoding.
fn simple_codes(font: &Dictionary) -> BTreeMap<char, u8> {
    let base = font
        .get("Encoding")
        .and_then(|encoding| match encoding {
            Object::Name(name) => Some(name.clone()),
            Object::Dictionary(encoding) => encoding
                .get("BaseEncoding")
                .and_then(Object::as_name)
                .map(str::to_owned),
            _ => None,
        })
        .unwrap_or_default();
    let mut codes = BTreeMap::new();
    // Printable ASCII is identical in Standard, WinAnsi, MacRoman and the
    // PDFDocEncoding default, so it is always encodable.
    for ch in ' '..='~' {
        codes.insert(ch, ch as u8);
    }
    if base == "WinAnsiEncoding" {
        for code in 0x80_u16..=0xFF {
            if let Some(ch) = win_ansi_char(code as u8) {
                codes.entry(ch).or_insert(code as u8);
            }
        }
    }
    // A `/Differences` array may map custom glyph names; those names are not
    // resolved here, so the ASCII default above is all that is promised.
    codes
}

/// Encodes text for the base-14 Helvetica fallback.
///
/// The fallback font is written with `/WinAnsiEncoding`, so a value must be
/// emitted as WinAnsi bytes rather than a UTF-16 literal. Returns `None` when a
/// character has no WinAnsi code; the caller then uses
/// [`DefaultTextEncoder`](crate::appearance::DefaultTextEncoder), which is only
/// meaningful for ASCII and non-ASCII text that a reader can map itself.
#[derive(Debug, Clone, Copy, Default)]
pub(super) struct WinAnsiEncoder;

impl TextEncoder for WinAnsiEncoder {
    fn encode(&self, text: &str) -> Option<EncodedText> {
        let mut bytes = Vec::with_capacity(text.len());
        for ch in text.chars() {
            bytes.push(win_ansi_code(ch)?);
        }
        Some(EncodedText::Literal(bytes))
    }
}

/// Returns the WinAnsi code for `ch`, if the encoding defines one.
fn win_ansi_code(ch: char) -> Option<u8> {
    let code = u32::from(ch);
    if let Ok(byte) = u8::try_from(code)
        && win_ansi_char(byte) == Some(ch)
    {
        return Some(byte);
    }
    (0x80_u8..=0x9F).find(|code| win_ansi_char(*code) == Some(ch))
}

/// Returns the Unicode character for a WinAnsi code, if defined.
///
/// Codes `0x80..=0x9F` are the WinAnsi specials (ISO 32000-1 Annex D.2);
/// `0xA0..=0xFF` are Latin-1; `0x20..=0x7E` are ASCII. Undefined slots return
/// `None`.
fn win_ansi_char(code: u8) -> Option<char> {
    match code {
        0x20..=0x7E => Some(char::from(code)),
        0x80 => Some('\u{20AC}'),
        0x82 => Some('\u{201A}'),
        0x83 => Some('\u{0192}'),
        0x84 => Some('\u{201E}'),
        0x85 => Some('\u{2026}'),
        0x86 => Some('\u{2020}'),
        0x87 => Some('\u{2021}'),
        0x88 => Some('\u{02C6}'),
        0x89 => Some('\u{2030}'),
        0x8A => Some('\u{0160}'),
        0x8B => Some('\u{2039}'),
        0x8C => Some('\u{0152}'),
        0x8E => Some('\u{017D}'),
        0x91 => Some('\u{2018}'),
        0x92 => Some('\u{2019}'),
        0x93 => Some('\u{201C}'),
        0x94 => Some('\u{201D}'),
        0x95 => Some('\u{2022}'),
        0x96 => Some('\u{2013}'),
        0x97 => Some('\u{2014}'),
        0x98 => Some('\u{02DC}'),
        0x99 => Some('\u{2122}'),
        0x9A => Some('\u{0161}'),
        0x9B => Some('\u{203A}'),
        0x9C => Some('\u{0153}'),
        0x9E => Some('\u{017E}'),
        0x9F => Some('\u{0178}'),
        0xA0..=0xFF => char::from_u32(u32::from(code)),
        _ => None,
    }
}

/// Parses an inverted `/ToUnicode` CMap (code to single character).
fn to_unicode_codes(doc: &Document, object: &Object) -> Option<BTreeMap<char, u16>> {
    let (dictionary, data) = match object {
        Object::Reference(id) => match doc.object(*id).ok()? {
            Object::Stream { dictionary, data } => (dictionary, data),
            _ => return None,
        },
        Object::Stream { dictionary, data } => (dictionary.clone(), data.clone()),
        _ => return None,
    };
    let decoded = match filters_from_dictionary(&dictionary) {
        Some(filters) => pdfxi_core::decode_stream(&data, &filters).ok()?,
        None => data,
    };
    Some(reverse_cmap(&decoded))
}

/// Builds a filter chain from a stream `/Filter` entry.
///
/// Only `FlateDecode` is decodable in this build; a dictionary naming anything
/// else returns `None`, which the caller treats as an unusable CMap.
fn filters_from_dictionary(dictionary: &Dictionary) -> Option<Vec<Filter>> {
    let filters: Vec<String> = match dictionary.get("Filter") {
        None => return None,
        Some(Object::Name(name)) => vec![name.clone()],
        Some(Object::Array(items)) => items
            .iter()
            .filter_map(Object::as_name)
            .map(str::to_owned)
            .collect(),
        Some(_) => return None,
    };
    let mut chain = Vec::with_capacity(filters.len());
    for name in filters {
        match name.as_str() {
            "FlateDecode" | "Fl" => chain.push(Filter::FlateDecode(Default::default())),
            _ => return None,
        }
    }
    Some(chain)
}

/// Inverts a decoded `/ToUnicode` CMap into a character-to-code map.
///
/// Only single-character destinations are kept; a code that maps to several
/// characters (a ligature) cannot be produced from plain text. The first code
/// for each character wins, which keeps the result deterministic.
fn reverse_cmap(data: &[u8]) -> BTreeMap<char, u16> {
    let mut codes = BTreeMap::new();
    let tokens = c_map_tokens(data);
    let mut index = 0_usize;
    while index < tokens.len() {
        match tokens[index].as_str() {
            "beginbfchar" => {
                index += 1;
                while index + 1 < tokens.len() && tokens[index] != "endbfchar" {
                    let (Some(source), Some(destination)) =
                        (hex_value(&tokens[index]), hex_string(&tokens[index + 1]))
                    else {
                        index += 2;
                        continue;
                    };
                    index += 2;
                    insert_mapping(&mut codes, source, &destination);
                }
                index += 1;
            }
            "beginbfrange" => {
                index += 1;
                while index + 2 < tokens.len() && tokens[index] != "endbfrange" {
                    let (Some(low), Some(high)) =
                        (hex_value(&tokens[index]), hex_value(&tokens[index + 1]))
                    else {
                        index += 3;
                        continue;
                    };
                    index += 2;
                    if tokens[index] == "[" {
                        index += 1;
                        let mut offset = 0_u32;
                        while index < tokens.len() && tokens[index] != "]" {
                            if let Some(destination) = hex_string(&tokens[index])
                                && offset <= u32::from(u16::MAX)
                            {
                                insert_mapping(
                                    &mut codes,
                                    low.saturating_add(offset),
                                    &destination,
                                );
                            }
                            offset = offset.saturating_add(1);
                            index += 1;
                        }
                        index += 1;
                    } else if let Some(destination) = hex_string(&tokens[index]) {
                        index += 1;
                        let mut code = low;
                        let mut destination = destination;
                        while code <= high && code <= u32::from(u16::MAX) {
                            insert_mapping(&mut codes, code, &destination);
                            increment_utf16(&mut destination);
                            code += 1;
                        }
                    } else {
                        index += 1;
                    }
                }
                index += 1;
            }
            _ => index += 1,
        }
    }
    codes
}

/// Retains the first single-character mapping for `code`.
fn insert_mapping(codes: &mut BTreeMap<char, u16>, code: u32, destination: &[u16]) {
    if codes.len() >= MAX_REVERSE_CMAP_ENTRIES {
        return;
    }
    let Ok(code) = u16::try_from(code) else {
        return;
    };
    let [unit] = destination else {
        return;
    };
    if let Some(ch) = char::from_u32(u32::from(*unit)) {
        codes.entry(ch).or_insert(code);
    }
}

/// Increments a UTF-16BE destination string in place, as a `bfrange` does.
fn increment_utf16(destination: &mut [u16]) {
    if let Some(last) = destination.last_mut() {
        *last = last.wrapping_add(1);
    }
}

/// Tokenises the subset of CMap syntax the inverter needs.
///
/// Hex strings are returned with `<` and `>` delimiters, names and operators as
/// their own tokens, and `[`/`]` as single tokens. Comments and ordinary
/// whitespace are skipped. The token count is bounded by the input length.
fn c_map_tokens(data: &[u8]) -> Vec<String> {
    let mut tokens = Vec::new();
    let mut index = 0_usize;
    while index < data.len() {
        let byte = data[index];
        match byte {
            b'%' => {
                while index < data.len() && !matches!(data[index], b'\r' | b'\n') {
                    index += 1;
                }
            }
            b'<' if !data.get(index + 1).is_some_and(|next| *next == b'<') => {
                let start = index;
                index += 1;
                while index < data.len() && data[index] != b'>' {
                    index += 1;
                }
                index = (index + 1).min(data.len());
                tokens.push(String::from_utf8_lossy(&data[start..index]).into_owned());
            }
            b'[' | b']' => {
                tokens.push(char::from(byte).to_string());
                index += 1;
            }
            b'(' => {
                // A literal destination string is legal but rare; keep it as one
                // token with balanced parentheses and backslash escapes.
                let start = index;
                index += 1;
                let mut depth = 1_usize;
                while index < data.len() && depth > 0 {
                    match data[index] {
                        b'\\' => index += 2,
                        b'(' => {
                            depth += 1;
                            index += 1;
                        }
                        b')' => {
                            depth -= 1;
                            index += 1;
                        }
                        _ => index += 1,
                    }
                }
                tokens.push(
                    String::from_utf8_lossy(&data[start..index.min(data.len())]).into_owned(),
                );
            }
            _ => {
                let start = index;
                while index < data.len()
                    && !matches!(
                        data[index],
                        b' ' | b'\t' | b'\r' | b'\n' | b'\x0c' | b'[' | b']' | b'<' | b'>' | b'%'
                    )
                {
                    index += 1;
                }
                if index == start {
                    index += 1;
                } else {
                    tokens.push(String::from_utf8_lossy(&data[start..index]).into_owned());
                }
            }
        }
    }
    tokens
}

/// Parses a `<hex>` token as an integer code.
fn hex_value(token: &str) -> Option<u32> {
    let digits = token.strip_prefix('<')?.strip_suffix('>')?;
    if digits.is_empty() || digits.len() > 8 {
        return None;
    }
    u32::from_str_radix(digits, 16).ok()
}

/// Parses a `<hex>` token as a UTF-16BE string.
fn hex_string(token: &str) -> Option<Vec<u16>> {
    let digits = token.strip_prefix('<')?.strip_suffix('>')?;
    if digits.len() % 4 != 0 {
        return None;
    }
    let mut units = Vec::with_capacity(digits.len() / 4);
    for chunk in digits.as_bytes().chunks(4) {
        let text = std::str::from_utf8(chunk).ok()?;
        units.push(u16::from_str_radix(text, 16).ok()?);
    }
    Some(units)
}

#[cfg(test)]
mod tests;
