//! Minimal TrueType reader for the bundled fallback font.
//!
//! Only the tables needed to embed a `TrueType` program as an identity-encoded
//! CID font are read: `head` (`unitsPerEm`), `maxp` (`numGlyphs`), `hhea` and
//! `hmtx` (advance widths), and `cmap` (Unicode to glyph identifier). Glyph
//! outlines are not interpreted; the raw program bytes are embedded so the
//! reader rasterizes the real glyphs.
//!
//! Every table offset and length is bounds-checked against the program buffer,
//! and malformed data returns [`TrueTypeError`] instead of panicking.

use std::collections::BTreeMap;

/// Typed failures while reading the fallback font program.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum TrueTypeError {
    /// The program was too short to contain an sfnt header.
    #[error("font program is truncated")]
    Truncated,
    /// The sfnt version was not a supported TrueType signature.
    #[error("unsupported font signature 0x{0:08X}")]
    BadSignature(u32),
    /// A required table was missing.
    #[error("font is missing the required '{0}' table")]
    MissingTable(&'static str),
    /// A table offset or length lay outside the program.
    #[error("font table '{0}' lies outside the program")]
    TableOutOfBounds(&'static str),
    /// The `cmap` subtable format is not supported.
    #[error("unsupported cmap format {0}")]
    UnsupportedCmap(u16),
    /// A character had no glyph in the font.
    #[error("the fallback font has no glyph for U+{0:04X}")]
    MissingGlyph(u32),
}

/// A parsed TrueType program with the tables needed for CID font embedding.
#[derive(Debug, Clone)]
pub(crate) struct TrueTypeFont {
    units_per_em: u16,
    num_glyphs: u16,
    advances: Vec<u16>,
    cmap: BTreeMap<u32, u16>,
}

impl TrueTypeFont {
    /// Parse `data` as a TrueType font program.
    pub(crate) fn parse(data: &[u8]) -> Result<Self, TrueTypeError> {
        if data.len() < 12 {
            return Err(TrueTypeError::Truncated);
        }
        let signature = read_u32(data, 0)?;
        if signature != 0x0001_0000 && signature != u32::from_be_bytes(*b"true") {
            return Err(TrueTypeError::BadSignature(signature));
        }
        let num_tables = usize::from(read_u16(data, 4)?);
        if data.len() < 12 + num_tables.saturating_mul(16) {
            return Err(TrueTypeError::Truncated);
        }
        let mut tables: BTreeMap<[u8; 4], (usize, usize)> = BTreeMap::new();
        for index in 0..num_tables {
            let record = 12 + index * 16;
            let tag: [u8; 4] = data
                .get(record..record + 4)
                .and_then(|slice| slice.try_into().ok())
                .ok_or(TrueTypeError::Truncated)?;
            let offset = read_u32(data, record + 8)? as usize;
            let length = read_u32(data, record + 12)? as usize;
            let end = offset.checked_add(length).ok_or(TrueTypeError::Truncated)?;
            if end > data.len() {
                return Err(TrueTypeError::TableOutOfBounds(table_name(&tag)));
            }
            tables.insert(tag, (offset, length));
        }

        let head = table(data, &tables, b"head")?;
        let units_per_em = read_u16(data, head + 18)?;
        if units_per_em == 0 {
            return Err(TrueTypeError::MissingTable("head"));
        }
        let maxp = table(data, &tables, b"maxp")?;
        let num_glyphs = read_u16(data, maxp + 4)?;
        let hhea = table(data, &tables, b"hhea")?;
        let number_of_h_metrics = usize::from(read_u16(data, hhea + 34)?);
        let hmtx = table(data, &tables, b"hmtx")?;
        let advances = read_advances(data, hmtx, number_of_h_metrics, usize::from(num_glyphs))?;

        let cmap_table = table(data, &tables, b"cmap")?;
        let cmap = parse_cmap(data, cmap_table)?;

        Ok(Self {
            units_per_em,
            num_glyphs,
            advances,
            cmap,
        })
    }

    /// Font design units per em.
    pub(crate) fn units_per_em(&self) -> u16 {
        self.units_per_em
    }

    /// Number of glyphs declared by the program.
    #[cfg(test)]
    pub(crate) fn num_glyphs(&self) -> u16 {
        self.num_glyphs
    }

    /// Glyph identifier for a Unicode scalar.
    pub(crate) fn glyph_id(&self, character: char) -> Result<u16, TrueTypeError> {
        self.cmap
            .get(&(character as u32))
            .copied()
            .filter(|glyph| *glyph < self.num_glyphs)
            .ok_or(TrueTypeError::MissingGlyph(character as u32))
    }

    /// Advance width of a glyph in font design units.
    pub(crate) fn advance_units(&self, glyph: u16) -> u16 {
        self.advances
            .get(usize::from(glyph))
            .copied()
            .or_else(|| self.advances.last().copied())
            .unwrap_or(0)
    }

    /// Advance width in thousandths of a text space unit at `font_size`.
    pub(crate) fn advance_for_char(&self, character: char, font_size: f64) -> f64 {
        let units = self
            .glyph_id(character)
            .map(|glyph| self.advance_units(glyph))
            .unwrap_or(0);
        f64::from(units) * font_size / f64::from(self.units_per_em)
    }

    /// Encode a string as the two-byte glyph identifiers used by `Identity-H`.
    #[cfg(test)]
    pub(crate) fn encode(&self, text: &str) -> Result<Vec<u8>, TrueTypeError> {
        let mut bytes = Vec::with_capacity(text.len().saturating_mul(2));
        for character in text.chars() {
            let glyph = self.glyph_id(character)?;
            bytes.extend_from_slice(&glyph.to_be_bytes());
        }
        Ok(bytes)
    }
}

/// Resolve a table's payload offset, checking the declared length again.
fn table(
    data: &[u8],
    tables: &BTreeMap<[u8; 4], (usize, usize)>,
    tag: &[u8; 4],
) -> Result<usize, TrueTypeError> {
    let (offset, length) = tables
        .get(tag)
        .copied()
        .ok_or(TrueTypeError::MissingTable(table_name(tag)))?;
    if offset.saturating_add(length) > data.len() {
        return Err(TrueTypeError::TableOutOfBounds(table_name(tag)));
    }
    Ok(offset)
}

/// Best-effort static label for an sfnt table tag.
fn table_name(tag: &[u8; 4]) -> &'static str {
    match tag {
        b"head" => "head",
        b"maxp" => "maxp",
        b"hhea" => "hhea",
        b"hmtx" => "hmtx",
        b"cmap" => "cmap",
        _ => "unknown",
    }
}

/// Read the advance-width array from `hmtx`.
fn read_advances(
    data: &[u8],
    offset: usize,
    number_of_h_metrics: usize,
    num_glyphs: usize,
) -> Result<Vec<u16>, TrueTypeError> {
    if number_of_h_metrics == 0 || num_glyphs == 0 {
        return Err(TrueTypeError::MissingTable("hmtx"));
    }
    let needed = number_of_h_metrics
        .checked_mul(4)
        .ok_or(TrueTypeError::Truncated)?;
    if offset.saturating_add(needed) > data.len() {
        return Err(TrueTypeError::TableOutOfBounds("hmtx"));
    }
    let mut advances = Vec::with_capacity(num_glyphs);
    for index in 0..number_of_h_metrics {
        advances.push(read_u16(data, offset + index * 4)?);
    }
    let last = advances.last().copied().unwrap_or(0);
    while advances.len() < num_glyphs {
        advances.push(last);
    }
    Ok(advances)
}

/// Parse the preferred Unicode `cmap` subtable into a code to glyph map.
fn parse_cmap(data: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, TrueTypeError> {
    let num_tables = usize::from(read_u16(data, offset + 2)?);
    let mut best: Option<(u32, usize)> = None;
    for index in 0..num_tables {
        let record = offset + 4 + index * 8;
        let platform = read_u16(data, record)?;
        let encoding = read_u16(data, record + 2)?;
        let subtable = offset
            .checked_add(read_u32(data, record + 4)? as usize)
            .ok_or(TrueTypeError::Truncated)?;
        // Prefer full Unicode (3/10), then Unicode BMP (3/1), then any Unicode.
        let rank = match (platform, encoding) {
            (3, 10) => 3,
            (3, 1) => 2,
            (0, _) => 1,
            _ => 0,
        };
        if rank > 0 && best.is_none_or(|(current, _)| rank > current) {
            best = Some((rank, subtable));
        }
    }
    let (_, subtable) = best.ok_or(TrueTypeError::MissingTable("cmap"))?;
    parse_cmap_subtable(data, subtable)
}

/// Parse one `cmap` subtable.
fn parse_cmap_subtable(data: &[u8], offset: usize) -> Result<BTreeMap<u32, u16>, TrueTypeError> {
    let format = read_u16(data, offset)?;
    let mut map = BTreeMap::new();
    match format {
        0 => {
            let count = usize::from(read_u16(data, offset + 2)?).min(256);
            for code in 0..count {
                let glyph = u16::from(read_u8(data, offset + 6 + code)?);
                if glyph != 0 {
                    map.insert(code as u32, glyph);
                }
            }
        }
        4 => {
            let seg_count_x2 = read_u16(data, offset + 6)?;
            if seg_count_x2 == 0 || seg_count_x2 % 2 != 0 {
                return Err(TrueTypeError::UnsupportedCmap(4));
            }
            let seg_count = usize::from(seg_count_x2 / 2);
            let end_codes = offset + 14;
            let start_codes = end_codes + seg_count * 2 + 2;
            let deltas = start_codes + seg_count * 2;
            let range_offsets = deltas + seg_count * 2;
            let mut total = 0usize;
            for segment in 0..seg_count {
                let end = u32::from(read_u16(data, end_codes + segment * 2)?);
                let start = u32::from(read_u16(data, start_codes + segment * 2)?);
                let delta = read_u16(data, deltas + segment * 2)?;
                let range_offset = read_u16(data, range_offsets + segment * 2)?;
                if start > end {
                    continue;
                }
                for code in start..=end {
                    if code == 0xFFFF {
                        continue;
                    }
                    total = total.saturating_add(1);
                    if total > 200_000 {
                        return Err(TrueTypeError::UnsupportedCmap(4));
                    }
                    let glyph = if range_offset == 0 {
                        (code as u16).wrapping_add(delta)
                    } else {
                        let address = range_offsets
                            .saturating_add(segment * 2)
                            .saturating_add(usize::from(range_offset))
                            .saturating_add((code - start) as usize * 2);
                        let raw = read_u16(data, address)?;
                        if raw == 0 { 0 } else { raw.wrapping_add(delta) }
                    };
                    if glyph != 0 {
                        map.insert(code, glyph);
                    }
                }
            }
        }
        6 => {
            let first = u32::from(read_u16(data, offset + 6)?);
            let count = usize::from(read_u16(data, offset + 8)?);
            let array = offset + 10;
            for index in 0..count {
                let glyph = read_u16(data, array + index * 2)?;
                if glyph != 0 {
                    map.insert(first + index as u32, glyph);
                }
            }
        }
        12 => {
            let groups = read_u32(data, offset + 12)? as usize;
            if offset.saturating_add(16 + groups.saturating_mul(12)) > data.len() {
                return Err(TrueTypeError::TableOutOfBounds("cmap"));
            }
            let mut total = 0usize;
            for group in 0..groups {
                let record = offset + 16 + group * 12;
                let start = read_u32(data, record)?;
                let end = read_u32(data, record + 4)?;
                let start_glyph = read_u32(data, record + 8)?;
                if start > end || end > 0x10FFFF {
                    continue;
                }
                for code in start..=end {
                    total = total.saturating_add(1);
                    if total > 200_000 {
                        return Err(TrueTypeError::UnsupportedCmap(12));
                    }
                    if let Ok(glyph) = u16::try_from(start_glyph + (code - start))
                        && glyph != 0
                    {
                        map.insert(code, glyph);
                    }
                }
            }
        }
        other => return Err(TrueTypeError::UnsupportedCmap(other)),
    }
    if map.is_empty() {
        return Err(TrueTypeError::MissingTable("cmap"));
    }
    Ok(map)
}

fn read_u8(data: &[u8], offset: usize) -> Result<u8, TrueTypeError> {
    data.get(offset).copied().ok_or(TrueTypeError::Truncated)
}

fn read_u16(data: &[u8], offset: usize) -> Result<u16, TrueTypeError> {
    let bytes = data
        .get(offset..offset + 2)
        .and_then(|slice| <[u8; 2]>::try_from(slice).ok())
        .ok_or(TrueTypeError::Truncated)?;
    Ok(u16::from_be_bytes(bytes))
}

fn read_u32(data: &[u8], offset: usize) -> Result<u32, TrueTypeError> {
    let bytes = data
        .get(offset..offset + 4)
        .and_then(|slice| <[u8; 4]>::try_from(slice).ok())
        .ok_or(TrueTypeError::Truncated)?;
    Ok(u32::from_be_bytes(bytes))
}

#[cfg(test)]
mod tests {
    use super::*;

    const FONT: &[u8] = include_bytes!("../assets/CourierPrime-Regular.ttf");

    #[test]
    fn bundled_font_parses_and_maps_ascii() {
        let font = TrueTypeFont::parse(FONT).expect("bundled font must parse");
        assert!(font.units_per_em() > 0);
        assert!(font.num_glyphs() > 32);
        for character in "The quick brown fox 0123!".chars() {
            let glyph = font.glyph_id(character).expect("glyph for ASCII");
            assert!(glyph < font.num_glyphs());
        }
        let encoded = font.encode("PDFXI").expect("encode");
        assert_eq!(encoded.len(), 10);
        assert!(font.advance_for_char('M', 12.0) > 0.0);
    }

    #[test]
    fn missing_glyph_is_typed() {
        let font = TrueTypeFont::parse(FONT).expect("bundled font must parse");
        assert!(matches!(
            font.glyph_id('\u{10FFFD}'),
            Err(TrueTypeError::MissingGlyph(_))
        ));
    }

    #[test]
    fn truncated_program_is_rejected() {
        assert!(matches!(
            TrueTypeFont::parse(b"not a font"),
            Err(TrueTypeError::Truncated)
        ));
        assert!(matches!(
            TrueTypeFont::parse(&[0, 1, 0, 0, 0, 0]),
            Err(TrueTypeError::Truncated)
        ));
    }
}
