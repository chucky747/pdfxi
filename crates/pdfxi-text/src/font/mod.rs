//! Font dictionary parsing and glyph-metric synthesis.
//!
//! A PDF page's `/Font` dictionary describes how character codes become glyphs
//! and how far the text cursor advances. This module provides a dependency-free
//! model of that dictionary: [`FontDictionary`] is the raw input, and
//! [`FontMetrics::from_dictionary`] validates it and produces a [`FontMetrics`]
//! value with the queries text extraction needs:
//!
//! - advances in text-space units ([`FontMetrics::advance_for_code`] and
//!   [`FontMetrics::advance_for_cid`]),
//! - vertical metrics and bounding box ([`FontMetrics::ascent`],
//!   [`FontMetrics::descent`], [`FontMetrics::font_bbox`]),
//! - whether the font program is embedded ([`FontMetrics::is_embedded`]),
//! - Unicode recovery through `/ToUnicode` ([`FontMetrics::unicode_for`]), and
//! - CID-to-GID resolution ([`FontMetrics::gid_for`]).
//!
//! Both simple fonts (Type1, TrueType, MMType1) and composite fonts (Type0 with
//! a CIDFontType0 or CIDFontType2 descendant) are supported. Every parser is
//! defensive: malformed or missing tables yield typed [`FontError`] values, and
//! standard-14 metrics are synthesized only when the specification permits it
//! (a non-embedded standard font with no explicit widths). No input can cause a
//! panic.

pub mod cmap;
pub mod standard14;

pub use cmap::{CidToGidMap, CidWidths, ToUnicodeCMap, WToken};
pub use standard14::Standard14Font;

/// Errors produced while parsing a font dictionary or its tables.
#[derive(Clone, Debug, PartialEq, thiserror::Error)]
pub enum FontError {
    /// A required dictionary entry was absent.
    #[error("font dictionary is missing required entry: {0}")]
    MissingEntry(&'static str),
    /// The `/Subtype` value names an unsupported simple font type.
    #[error("unsupported font subtype: {0}")]
    UnsupportedSubtype(String),
    /// The descendant CID font subtype is unsupported.
    #[error("unsupported CID font subtype: {0}")]
    UnsupportedCidSubtype(String),
    /// A table entry had an invalid value.
    #[error("invalid {field}: {reason}")]
    Invalid {
        /// The dictionary key or table that was invalid.
        field: &'static str,
        /// Why the value was rejected.
        reason: String,
    },
    /// The `/ToUnicode` CMap stream could not be parsed.
    #[error("ToUnicode CMap error at byte {offset}: {reason}")]
    ToUnicode {
        /// Byte offset into the CMap stream where the problem was found.
        offset: usize,
        /// Why the CMap was rejected.
        reason: String,
    },
    /// A `/CIDToGIDMap` stream did not contain whole 16-bit values.
    #[error("CIDToGIDMap stream length {len} is not a multiple of 2")]
    OddCidToGidLength {
        /// The invalid stream length in bytes.
        len: usize,
    },
}

/// The `/Subtype` of a simple font dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontSubtype {
    /// Type1 (PostScript) font.
    Type1,
    /// Multiple-master Type1 font.
    MmType1,
    /// TrueType font.
    TrueType,
    /// Type0 composite font; its descendant supplies the CID metrics.
    Type0,
}

impl FontSubtype {
    /// Parse a PDF name such as `Type1` or `/TrueType`.
    pub fn from_pdf_name(name: &str) -> Result<Self, FontError> {
        match trim_name(name) {
            "Type1" => Ok(Self::Type1),
            "MMType1" => Ok(Self::MmType1),
            "TrueType" => Ok(Self::TrueType),
            "Type0" => Ok(Self::Type0),
            other => Err(FontError::UnsupportedSubtype(other.to_owned())),
        }
    }
}

/// The subtype of the descendant CID font of a Type0 font.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CidSubtype {
    /// CIDFontType0 (CFF-based) descendant.
    CidFontType0,
    /// CIDFontType2 (TrueType-based) descendant.
    CidFontType2,
}

impl CidSubtype {
    /// Parse a PDF name such as `CIDFontType2` or `/CIDFontType0`.
    pub fn from_pdf_name(name: &str) -> Result<Self, FontError> {
        match trim_name(name) {
            "CIDFontType0" => Ok(Self::CidFontType0),
            "CIDFontType2" => Ok(Self::CidFontType2),
            other => Err(FontError::UnsupportedCidSubtype(other.to_owned())),
        }
    }
}

/// Strip an optional leading slash from a PDF name.
fn trim_name(name: &str) -> &str {
    name.strip_prefix('/').unwrap_or(name)
}

/// The `/Encoding` declared by a font dictionary.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FontEncoding {
    /// WinAnsiEncoding.
    WinAnsiEncoding,
    /// MacRomanEncoding.
    MacRomanEncoding,
    /// PDFDocEncoding, used by document information strings.
    PdfDocEncoding,
    /// MacExpertEncoding.
    MacExpertEncoding,
    /// The Adobe StandardEncoding (the default for simple fonts).
    StandardEncoding,
    /// The `Identity-H` CMap (2-byte horizontal CIDs).
    IdentityH,
    /// The `Identity-V` CMap (2-byte vertical CIDs).
    IdentityV,
    /// Any other predefined CMap name (for example `UniJIS-UCS2-H`).
    NamedCMap(String),
}

impl FontEncoding {
    /// Parse a PDF encoding/CMap name.
    ///
    /// Recognition of the well-known names is exact; any other name is retained
    /// as [`Self::NamedCMap`] because a Type0 font's `/Encoding` is a CMap name
    /// rather than a simple character encoding.
    pub fn from_pdf_name(name: &str) -> Self {
        match trim_name(name) {
            "WinAnsiEncoding" => Self::WinAnsiEncoding,
            "MacRomanEncoding" => Self::MacRomanEncoding,
            "PDFDocEncoding" => Self::PdfDocEncoding,
            "MacExpertEncoding" => Self::MacExpertEncoding,
            "StandardEncoding" => Self::StandardEncoding,
            "Identity-H" => Self::IdentityH,
            "Identity-V" => Self::IdentityV,
            other => Self::NamedCMap(other.to_owned()),
        }
    }
}

/// A font bounding box in glyph space (normally 1/1000 text units).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontBBox {
    /// Left edge.
    pub x_min: f32,
    /// Bottom edge.
    pub y_min: f32,
    /// Right edge.
    pub x_max: f32,
    /// Top edge.
    pub y_max: f32,
}

impl FontBBox {
    /// An all-zero bounding box, used when no metrics are available.
    pub const ZERO: Self = Self {
        x_min: 0.0,
        y_min: 0.0,
        x_max: 0.0,
        y_max: 0.0,
    };

    /// Create a bounding box from its four edges.
    pub const fn new(x_min: f32, y_min: f32, x_max: f32, y_max: f32) -> Self {
        Self {
            x_min,
            y_min,
            x_max,
            y_max,
        }
    }

    /// Return the box width (`x_max - x_min`).
    pub fn width(&self) -> f32 {
        self.x_max - self.x_min
    }

    /// Return the box height (`y_max - y_min`).
    pub fn height(&self) -> f32 {
        self.y_max - self.y_min
    }

    /// Return `true` when all four edges are finite.
    pub fn is_finite(&self) -> bool {
        self.x_min.is_finite()
            && self.y_min.is_finite()
            && self.x_max.is_finite()
            && self.y_max.is_finite()
    }
}

/// Identifies which embedded font program a descriptor carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EmbeddedFontFile {
    /// A Type1 font program (`/FontFile`).
    Type1,
    /// A TrueType font program (`/FontFile2`).
    TrueType,
    /// A CFF or OpenType font program (`/FontFile3`).
    Cff,
}

/// A validated subset of a PDF `/FontDescriptor`.
#[derive(Clone, Debug, PartialEq)]
pub struct FontDescriptor {
    /// The descriptor `/Flags` bit field.
    pub flags: u32,
    /// The font bounding box in glyph space.
    pub font_bbox: FontBBox,
    /// Italic angle in degrees, counterclockwise.
    pub italic_angle: f32,
    /// Ascent in glyph space, when present.
    pub ascent: Option<f32>,
    /// Descent in glyph space, when present.
    pub descent: Option<f32>,
    /// Cap height in glyph space, when present.
    pub cap_height: Option<f32>,
    /// Stem width in glyph space, when present.
    pub stem_v: Option<f32>,
    /// Width used for codes absent from the width table (`/MissingWidth`).
    pub missing_width: Option<f32>,
    /// The embedded font program, if any.
    pub embedded_file: Option<EmbeddedFontFile>,
}

impl FontDescriptor {
    /// Return `true` when the descriptor references an embedded font program.
    pub fn is_embedded(&self) -> bool {
        self.embedded_file.is_some()
    }

    /// Validate that every carried number is finite.
    fn validate(&self) -> Result<(), FontError> {
        if !self.font_bbox.is_finite() {
            return Err(FontError::Invalid {
                field: "FontBBox",
                reason: "edges must be finite".into(),
            });
        }
        if !self.italic_angle.is_finite() {
            return Err(FontError::Invalid {
                field: "ItalicAngle",
                reason: "must be finite".into(),
            });
        }
        for (field, value) in [
            ("Ascent", self.ascent),
            ("Descent", self.descent),
            ("CapHeight", self.cap_height),
            ("StemV", self.stem_v),
            ("MissingWidth", self.missing_width),
        ] {
            if value.is_some_and(|number| !number.is_finite()) {
                return Err(FontError::Invalid {
                    field,
                    reason: "must be finite".into(),
                });
            }
        }
        Ok(())
    }
}

/// A 2D affine transform mapping glyph space to text space.
///
/// The usual Type1/TrueType/CID `FontMatrix` is `[0.001 0 0 0.001 0 0]`,
/// converting thousandths of a text unit to text units. [`Self::typical`]
/// returns that matrix and is the default when a font omits `/FontMatrix`.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct FontMatrix {
    /// `a` component: horizontal glyph-space to text-space scale.
    pub a: f32,
    /// `b` component.
    pub b: f32,
    /// `c` component.
    pub c: f32,
    /// `d` component: vertical glyph-space to text-space scale.
    pub d: f32,
    /// `e` (horizontal translation) component.
    pub e: f32,
    /// `f` (vertical translation) component.
    pub f: f32,
}

impl FontMatrix {
    /// Create a matrix from its six components.
    pub const fn new(a: f32, b: f32, c: f32, d: f32, e: f32, f: f32) -> Self {
        Self { a, b, c, d, e, f }
    }

    /// Return the identity matrix `[1 0 0 1 0 0]`.
    pub const fn identity() -> Self {
        Self::new(1.0, 0.0, 0.0, 1.0, 0.0, 0.0)
    }

    /// Return the typical `[0.001 0 0 0.001 0 0]` text-space matrix.
    pub const fn typical() -> Self {
        Self::new(0.001, 0.0, 0.0, 0.001, 0.0, 0.0)
    }

    /// Return the horizontal scale used to convert a width to a text advance.
    pub fn x_scale(&self) -> f32 {
        self.a
    }

    /// Return the vertical scale used to convert a width to a text advance.
    pub fn y_scale(&self) -> f32 {
        self.d
    }

    /// Transform a glyph-space point `(x, y)` into text space.
    pub fn transform(&self, x: f32, y: f32) -> (f32, f32) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }

    /// Return `true` when all six components are finite.
    pub fn is_finite(&self) -> bool {
        self.a.is_finite()
            && self.b.is_finite()
            && self.c.is_finite()
            && self.d.is_finite()
            && self.e.is_finite()
            && self.f.is_finite()
    }
}

impl Default for FontMatrix {
    fn default() -> Self {
        Self::typical()
    }
}

/// The resolved kind of a font dictionary.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FontKind {
    /// A Type1 simple font.
    Type1,
    /// A multiple-master Type1 simple font.
    MmType1,
    /// A TrueType simple font.
    TrueType,
    /// A Type0 composite font with the given CID descendant subtype.
    Type0 {
        /// The descendant CID font subtype.
        cid_subtype: CidSubtype,
    },
}

/// The input form of a `/CIDToGIDMap` entry.
#[derive(Clone, Debug, PartialEq)]
pub enum CidToGidMapInput {
    /// The name `/Identity`.
    Identity,
    /// A stream of 16-bit big-endian GIDs.
    Stream(Vec<u8>),
}

/// Raw contents of a PDF `/Font` dictionary and its descendant CID font.
///
/// Fields that are `None` are absent from the dictionary. Because
/// `pdfxi-text` stays dependency-free, callers supply the dictionary as these
/// typed entries; [`FontMetrics::from_dictionary`] performs all validation and
/// table parsing.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct FontDictionary {
    /// The `/BaseFont` PostScript name.
    pub base_font: Option<String>,
    /// The simple-font `/Subtype`.
    pub subtype: Option<FontSubtype>,
    /// The descendant CID font `/Subtype` for a Type0 font.
    pub cid_subtype: Option<CidSubtype>,
    /// The `/Encoding` name.
    pub encoding: Option<FontEncoding>,
    /// `/FirstChar`, the code mapped by the first entry of `/Widths`.
    pub first_char: Option<u32>,
    /// `/Widths`, one width per code starting at `first_char`.
    pub widths: Option<Vec<f32>>,
    /// The font's `/FontDescriptor`.
    pub font_descriptor: Option<FontDescriptor>,
    /// The `/FontMatrix` transform.
    pub font_matrix: Option<FontMatrix>,
    /// Raw bytes of the `/ToUnicode` CMap stream.
    pub to_unicode: Option<Vec<u8>>,
    /// The descendant CID font's `/W` array tokens.
    pub w_array: Option<Vec<WToken>>,
    /// The descendant CID font's `/DW` default width.
    pub default_width: Option<f32>,
    /// The descendant CID font's `/CIDToGIDMap`.
    pub cid_to_gid_map: Option<CidToGidMapInput>,
}

/// Widths of a simple font, indexed from `first_char`.
#[derive(Clone, Debug, PartialEq)]
struct SimpleWidths {
    first_char: u32,
    widths: Vec<f32>,
}

/// Validated glyph metrics and tables derived from a [`FontDictionary`].
///
/// Construct one with [`Self::from_dictionary`]. All width queries return raw
/// glyph-space values through `glyph_width*` and text-space advances through
/// `advance*`; the two differ by the font's horizontal [`FontMatrix`] scale.
#[derive(Clone, Debug, PartialEq)]
pub struct FontMetrics {
    base_font: String,
    kind: FontKind,
    encoding: FontEncoding,
    matrix: FontMatrix,
    descriptor: Option<FontDescriptor>,
    to_unicode: Option<ToUnicodeCMap>,
    simple_widths: Option<SimpleWidths>,
    cid_widths: Option<CidWidths>,
    default_width: f32,
    cid_to_gid: Option<CidToGidMap>,
    standard14: Option<Standard14Font>,
}

impl FontMetrics {
    /// Parse and validate a font dictionary into metrics.
    ///
    /// In addition to the checks each table performs, this method enforces the
    /// specification-level invariants that matter for safe text layout:
    /// `/BaseFont` and `/Subtype` must exist, a Type0 font must declare its
    /// descendant CID subtype and `/Encoding`, widths without a `/FirstChar`
    /// (or vice versa) are rejected, and numbers must be finite.
    pub fn from_dictionary(dict: FontDictionary) -> Result<Self, FontError> {
        let base_font = dict.base_font.ok_or(FontError::MissingEntry("BaseFont"))?;
        let subtype = dict.subtype.ok_or(FontError::MissingEntry("Subtype"))?;
        let kind = match subtype {
            FontSubtype::Type0 => FontKind::Type0 {
                cid_subtype: dict
                    .cid_subtype
                    .ok_or(FontError::MissingEntry("CIDFontSubtype"))?,
            },
            FontSubtype::Type1 => FontKind::Type1,
            FontSubtype::MmType1 => FontKind::MmType1,
            FontSubtype::TrueType => FontKind::TrueType,
        };

        let encoding = match dict.encoding {
            Some(encoding) => encoding,
            None if matches!(kind, FontKind::Type0 { .. }) => {
                return Err(FontError::MissingEntry("Encoding"));
            }
            None => FontEncoding::StandardEncoding,
        };

        let matrix = dict.font_matrix.unwrap_or_default();
        if !matrix.is_finite() {
            return Err(FontError::Invalid {
                field: "FontMatrix",
                reason: "components must be finite".into(),
            });
        }

        let descriptor = dict.font_descriptor;
        if let Some(descriptor) = &descriptor {
            descriptor.validate()?;
        }

        let simple_widths = match (dict.first_char, dict.widths) {
            (Some(first_char), Some(widths)) => {
                if first_char > u32::from(u16::MAX) {
                    return Err(FontError::Invalid {
                        field: "FirstChar",
                        reason: format!("{first_char} is outside 0..=65535"),
                    });
                }
                if widths.is_empty() {
                    return Err(FontError::Invalid {
                        field: "Widths",
                        reason: "width array must not be empty".into(),
                    });
                }
                for width in &widths {
                    if !width.is_finite() || *width < 0.0 {
                        return Err(FontError::Invalid {
                            field: "Widths",
                            reason: "widths must be finite and non-negative".into(),
                        });
                    }
                }
                Some(SimpleWidths { first_char, widths })
            }
            (None, None) => None,
            (Some(_), None) => return Err(FontError::MissingEntry("Widths")),
            (None, Some(_)) => return Err(FontError::MissingEntry("FirstChar")),
        };

        let to_unicode = match dict.to_unicode {
            Some(bytes) => Some(ToUnicodeCMap::parse(&bytes)?),
            None => None,
        };
        let cid_widths = match dict.w_array {
            Some(tokens) => Some(CidWidths::parse(&tokens)?),
            None => None,
        };
        let cid_to_gid = match dict.cid_to_gid_map {
            Some(CidToGidMapInput::Identity) => Some(CidToGidMap::Identity),
            Some(CidToGidMapInput::Stream(bytes)) => Some(CidToGidMap::parse_stream(&bytes)?),
            None => None,
        };

        let default_width = dict.default_width.unwrap_or(1000.0);
        if !default_width.is_finite() || default_width < 0.0 {
            return Err(FontError::Invalid {
                field: "DW",
                reason: "default width must be finite and non-negative".into(),
            });
        }

        let embedded = descriptor.as_ref().is_some_and(FontDescriptor::is_embedded);
        let standard14 = if embedded {
            None
        } else {
            Standard14Font::from_base_font(&base_font)
        };

        Ok(Self {
            base_font,
            kind,
            encoding,
            matrix,
            descriptor,
            to_unicode,
            simple_widths,
            cid_widths,
            default_width,
            cid_to_gid,
            standard14,
        })
    }

    /// Return the `/BaseFont` name.
    pub fn base_font(&self) -> &str {
        &self.base_font
    }

    /// Return the resolved font kind.
    pub fn kind(&self) -> FontKind {
        self.kind
    }

    /// Return the declared encoding.
    pub fn encoding(&self) -> &FontEncoding {
        &self.encoding
    }

    /// Return `true` when an embedded font program is present.
    pub fn is_embedded(&self) -> bool {
        self.descriptor
            .as_ref()
            .is_some_and(FontDescriptor::is_embedded)
    }

    /// Return the font matrix.
    pub fn font_matrix(&self) -> FontMatrix {
        self.matrix
    }

    /// Return the font descriptor, when present.
    pub fn descriptor(&self) -> Option<&FontDescriptor> {
        self.descriptor.as_ref()
    }

    /// Return the resolved standard-14 font, when metrics may be synthesized.
    pub fn standard14(&self) -> Option<Standard14Font> {
        self.standard14
    }

    /// Return the parsed `/ToUnicode` CMap, when present.
    pub fn to_unicode(&self) -> Option<&ToUnicodeCMap> {
        self.to_unicode.as_ref()
    }

    /// Return the parsed `/CIDToGIDMap`, when present.
    pub fn cid_to_gid_map(&self) -> Option<&CidToGidMap> {
        self.cid_to_gid.as_ref()
    }

    /// Return the parsed `/W` table, when present.
    pub fn cid_widths(&self) -> Option<&CidWidths> {
        self.cid_widths.as_ref()
    }

    /// Return the `/DW` default width in glyph-space units.
    pub fn default_width(&self) -> f32 {
        self.default_width
    }

    /// Return the `/FirstChar` of the simple width table, when present.
    pub fn first_char(&self) -> Option<u32> {
        self.simple_widths.as_ref().map(|simple| simple.first_char)
    }

    /// Return the ascent in glyph-space units.
    ///
    /// The descriptor value wins; otherwise the documented standard-14 ascent
    /// is used when available; otherwise zero.
    pub fn ascent(&self) -> f32 {
        if let Some(ascent) = self.descriptor.as_ref().and_then(|d| d.ascent) {
            ascent
        } else {
            self.standard14.map_or(0.0, Standard14Font::ascent)
        }
    }

    /// Return the descent in glyph-space units.
    pub fn descent(&self) -> f32 {
        if let Some(descent) = self.descriptor.as_ref().and_then(|d| d.descent) {
            descent
        } else {
            self.standard14.map_or(0.0, Standard14Font::descent)
        }
    }

    /// Return the font bounding box in glyph-space units.
    pub fn font_bbox(&self) -> FontBBox {
        if let Some(bbox) = self.descriptor.as_ref().map(|d| d.font_bbox) {
            bbox
        } else if let Some(standard) = self.standard14 {
            let (x_min, y_min, x_max, y_max) = standard.bbox();
            FontBBox::new(x_min, y_min, x_max, y_max)
        } else {
            FontBBox::ZERO
        }
    }

    /// Return the raw glyph-space width for a simple-font character code.
    ///
    /// Lookup order is the explicit `/Widths` array, then `/MissingWidth`, then
    /// documented standard-14 AFM data (only for a non-embedded standard font),
    /// then zero.
    pub fn glyph_width(&self, code: u32) -> f32 {
        if let Some(width) = self.simple_width(code) {
            return width;
        }
        if let Some(missing) = self.descriptor.as_ref().and_then(|d| d.missing_width) {
            return missing;
        }
        if let Some(standard) = self.standard14
            && let Some(ch) = standard14::code_to_char(code, &self.encoding)
        {
            return standard
                .width_for_char(ch)
                .map_or(standard.nominal_width() as f32, |width| width as f32);
        }
        0.0
    }

    /// Return the raw glyph-space width for a CID.
    ///
    /// Lookup order is the explicit `/W` table, then `/DW` (default `1000.0`).
    pub fn glyph_width_for_cid(&self, cid: u16) -> f32 {
        self.cid_widths
            .as_ref()
            .and_then(|widths| widths.width_for(cid))
            .unwrap_or(self.default_width)
    }

    /// Return the text-space advance for a simple-font character code.
    pub fn advance_for_code(&self, code: u32) -> f32 {
        self.glyph_width(code) * self.matrix.x_scale()
    }

    /// Return the text-space advance for a CID.
    pub fn advance_for_cid(&self, cid: u16) -> f32 {
        self.glyph_width_for_cid(cid) * self.matrix.x_scale()
    }

    /// Return the text-space advance for a code or CID.
    ///
    /// For a Type0 font the value is interpreted as a CID; for simple fonts it
    /// is a character code. Codes above `u16::MAX` passed to a Type0 font return
    /// the default advance.
    pub fn advance(&self, code_or_cid: u32) -> f32 {
        match self.kind {
            FontKind::Type0 { .. } => match u16::try_from(code_or_cid) {
                Ok(cid) => self.advance_for_cid(cid),
                Err(_) => self.default_width * self.matrix.x_scale(),
            },
            _ => self.advance_for_code(code_or_cid),
        }
    }

    /// Return the Unicode string for a source code or CID.
    ///
    /// `/ToUnicode` is consulted first. For simple fonts without a mapping, a
    /// single scalar is recovered from the declared simple encoding when
    /// possible. Composite encodings cannot be recovered without a CMap.
    pub fn unicode_for(&self, code: u16) -> Option<String> {
        if let Some(mapping) = &self.to_unicode
            && let Some(text) = mapping.unicode_for(code)
        {
            return Some(text.to_owned());
        }
        standard14::code_to_char(u32::from(code), &self.encoding).map(|ch| ch.to_string())
    }

    /// Return the GID for a CID through `/CIDToGIDMap`.
    ///
    /// `/Identity` maps every CID to itself; an explicit map returns the entry
    /// at the CID index, or `None` when the CID is beyond the stream.
    pub fn gid_for(&self, cid: u16) -> Option<u16> {
        self.cid_to_gid
            .as_ref()
            .and_then(|mapping| mapping.gid_for(cid))
    }

    /// Look up a code in the simple font's `/Widths` array.
    fn simple_width(&self, code: u32) -> Option<f32> {
        let simple = self.simple_widths.as_ref()?;
        let last = simple
            .first_char
            .checked_add(simple.widths.len() as u32 - 1)?;
        if code < simple.first_char || code > last {
            return None;
        }
        let offset = (code - simple.first_char) as usize;
        simple.widths.get(offset).copied()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Compare text-space advances, which accumulate f32 rounding from the
    /// 0.001 font-matrix scale.
    fn close(actual: f32, expected: f32) -> bool {
        (actual - expected).abs() < 1e-5
    }

    fn simple_dict(base: &str, subtype: FontSubtype, first: u32, widths: &[f32]) -> FontDictionary {
        FontDictionary {
            base_font: Some(base.into()),
            subtype: Some(subtype),
            first_char: Some(first),
            widths: Some(widths.to_vec()),
            ..FontDictionary::default()
        }
    }

    fn type0_dict() -> FontDictionary {
        FontDictionary {
            base_font: Some("CustomType0".into()),
            subtype: Some(FontSubtype::Type0),
            cid_subtype: Some(CidSubtype::CidFontType2),
            encoding: Some(FontEncoding::IdentityH),
            ..FontDictionary::default()
        }
    }

    fn descriptor(
        embedded: Option<EmbeddedFontFile>,
        missing_width: Option<f32>,
    ) -> FontDescriptor {
        FontDescriptor {
            flags: 4,
            font_bbox: FontBBox::new(-100.0, -200.0, 1000.0, 900.0),
            italic_angle: 0.0,
            ascent: Some(700.0),
            descent: Some(-200.0),
            cap_height: Some(700.0),
            stem_v: Some(80.0),
            missing_width,
            embedded_file: embedded,
        }
    }

    #[test]
    fn simple_type1_widths_are_reported_in_text_space() {
        let metrics = FontMetrics::from_dictionary(simple_dict(
            "CustomType1",
            FontSubtype::Type1,
            65,
            &[667.0, 667.0, 722.0],
        ))
        .expect("valid Type1 dictionary");
        assert_eq!(metrics.kind(), FontKind::Type1);
        assert_eq!(metrics.first_char(), Some(65));
        assert_eq!(metrics.glyph_width(65), 667.0);
        assert_eq!(metrics.glyph_width(67), 722.0);
        assert!(close(metrics.advance_for_code(67), 0.722));
        // 68 is just past LastChar (67) and no fallback is available.
        assert_eq!(metrics.glyph_width(68), 0.0);
        assert!(close(metrics.advance(65), 0.667));
    }

    #[test]
    fn truetype_widths_are_indexed_from_first_char() {
        let metrics = FontMetrics::from_dictionary(simple_dict(
            "CustomTrueType",
            FontSubtype::TrueType,
            32,
            &[278.0, 278.0, 556.0],
        ))
        .expect("valid TrueType dictionary");
        assert_eq!(metrics.kind(), FontKind::TrueType);
        assert_eq!(metrics.glyph_width(32), 278.0);
        assert_eq!(metrics.glyph_width(33), 278.0);
        assert_eq!(metrics.glyph_width(34), 556.0);
        assert_eq!(metrics.glyph_width(35), 0.0);
    }

    #[test]
    fn type0_w_array_supports_lists_and_ranges() {
        let mut dict = type0_dict();
        dict.w_array = Some(vec![
            WToken::Number(100.0),
            WToken::Array(vec![500.0, 600.0, 700.0]),
            WToken::Number(200.0),
            WToken::Number(202.0),
            WToken::Number(900.0),
        ]);
        let metrics = FontMetrics::from_dictionary(dict).expect("valid Type0 dictionary");
        assert_eq!(metrics.glyph_width_for_cid(100), 500.0);
        assert_eq!(metrics.glyph_width_for_cid(101), 600.0);
        assert_eq!(metrics.glyph_width_for_cid(102), 700.0);
        assert_eq!(metrics.glyph_width_for_cid(200), 900.0);
        assert_eq!(metrics.glyph_width_for_cid(201), 900.0);
        assert_eq!(metrics.glyph_width_for_cid(202), 900.0);
        // Unmapped CIDs fall back to /DW.
        assert_eq!(metrics.glyph_width_for_cid(150), 1000.0);
        assert_eq!(metrics.default_width(), 1000.0);
    }

    #[test]
    fn identity_h_cid_advances_respect_font_matrix() {
        let mut dict = type0_dict();
        assert_eq!(dict.encoding, Some(FontEncoding::IdentityH));
        dict.w_array = Some(vec![WToken::Number(7.0), WToken::Array(vec![500.0])]);
        let metrics = FontMetrics::from_dictionary(dict.clone()).expect("Identity-H dictionary");
        assert_eq!(metrics.advance_for_cid(7), 0.5);
        assert_eq!(metrics.advance(7), 0.5);

        dict.font_matrix = Some(FontMatrix::new(0.002, 0.0, 0.0, 0.002, 0.0, 0.0));
        let scaled = FontMetrics::from_dictionary(dict).expect("scaled dictionary");
        assert_eq!(scaled.advance_for_cid(7), 1.0);
    }

    #[test]
    fn tounicode_bfchar_and_bfrange_are_mapped() {
        let cmap = b"/CIDInit /ProcSet findresource begin\n12 dict begin\nbegincmap\n\
                     2 beginbfchar\n<01> <0041>\n<02> <0062>\nendbfchar\n\
                     2 beginbfrange\n<03> <05> <0043>\n<10> <11> [<0058> <0059>]\nendbfrange\n\
                     endcmap\n";
        let mut dict = type0_dict();
        dict.to_unicode = Some(cmap.to_vec());
        let metrics = FontMetrics::from_dictionary(dict).expect("valid ToUnicode CMap");
        assert_eq!(metrics.unicode_for(1).as_deref(), Some("A"));
        assert_eq!(metrics.unicode_for(2).as_deref(), Some("b"));
        assert_eq!(metrics.unicode_for(3).as_deref(), Some("C"));
        assert_eq!(metrics.unicode_for(4).as_deref(), Some("D"));
        assert_eq!(metrics.unicode_for(5).as_deref(), Some("E"));
        assert_eq!(metrics.unicode_for(0x10).as_deref(), Some("X"));
        assert_eq!(metrics.unicode_for(0x11).as_deref(), Some("Y"));
        assert_eq!(metrics.unicode_for(6), None);
    }

    #[test]
    fn missing_width_is_used_for_codes_outside_the_table() {
        let mut dict = simple_dict("CustomType1", FontSubtype::Type1, 65, &[600.0]);
        dict.font_descriptor = Some(descriptor(None, Some(500.0)));
        let metrics = FontMetrics::from_dictionary(dict).expect("valid dictionary");
        assert_eq!(metrics.glyph_width(65), 600.0);
        assert_eq!(metrics.glyph_width(66), 500.0);
        assert_eq!(metrics.advance_for_code(66), 0.5);
    }

    #[test]
    fn embedded_font_flag_controls_standard14_fallback() {
        let mut dict = FontDictionary {
            base_font: Some("Helvetica".into()),
            subtype: Some(FontSubtype::Type1),
            ..FontDictionary::default()
        };
        dict.font_descriptor = Some(descriptor(Some(EmbeddedFontFile::TrueType), None));
        let embedded = FontMetrics::from_dictionary(dict.clone()).expect("embedded font");
        assert!(embedded.is_embedded());
        assert_eq!(embedded.standard14(), None);
        assert_eq!(embedded.glyph_width(0x41), 0.0);

        dict.font_descriptor = Some(descriptor(None, None));
        let not_embedded = FontMetrics::from_dictionary(dict).expect("non-embedded font");
        assert!(!not_embedded.is_embedded());
        assert_eq!(not_embedded.standard14(), Some(Standard14Font::Helvetica));
    }

    #[test]
    fn cid_to_gid_map_supports_identity_and_explicit_streams() {
        let mut dict = type0_dict();
        dict.cid_to_gid_map = Some(CidToGidMapInput::Identity);
        let identity = FontMetrics::from_dictionary(dict.clone()).expect("Identity CIDToGIDMap");
        assert_eq!(identity.gid_for(9), Some(9));

        dict.cid_to_gid_map = Some(CidToGidMapInput::Stream(vec![
            0x00, 0x01, 0x00, 0x02, 0x01, 0x03,
        ]));
        let explicit = FontMetrics::from_dictionary(dict).expect("explicit CIDToGIDMap");
        assert_eq!(explicit.gid_for(0), Some(1));
        assert_eq!(explicit.gid_for(1), Some(2));
        assert_eq!(explicit.gid_for(2), Some(259));
        assert_eq!(explicit.gid_for(3), None);
    }

    #[test]
    fn malformed_tables_are_rejected_without_panicking() {
        let mut bad_widths = simple_dict("X", FontSubtype::Type1, 65, &[f32::NAN]);
        assert!(matches!(
            FontMetrics::from_dictionary(bad_widths.clone()),
            Err(FontError::Invalid { .. })
        ));
        bad_widths.widths = Some(vec![1.0]);
        bad_widths.first_char = None;
        assert_eq!(
            FontMetrics::from_dictionary(bad_widths),
            Err(FontError::MissingEntry("FirstChar"))
        );

        let mut bad_default = type0_dict();
        bad_default.default_width = Some(-1.0);
        assert!(matches!(
            FontMetrics::from_dictionary(bad_default),
            Err(FontError::Invalid { .. })
        ));

        let mut bad_cmap = type0_dict();
        bad_cmap.to_unicode = Some(b"1 beginbfchar\n<01> <0041\nendbfchar\n".to_vec());
        assert!(matches!(
            FontMetrics::from_dictionary(bad_cmap),
            Err(FontError::ToUnicode { .. })
        ));

        let mut bad_w = type0_dict();
        bad_w.w_array = Some(vec![WToken::Number(5.0)]);
        assert!(matches!(
            FontMetrics::from_dictionary(bad_w),
            Err(FontError::Invalid { .. })
        ));

        assert_eq!(
            FontMetrics::from_dictionary(FontDictionary::default()),
            Err(FontError::MissingEntry("BaseFont"))
        );
    }

    #[test]
    fn standard14_metrics_are_synthesized_for_non_embedded_base_fonts() {
        let dict = FontDictionary {
            base_font: Some("ABCDEF+Helvetica".into()),
            subtype: Some(FontSubtype::Type1),
            encoding: Some(FontEncoding::WinAnsiEncoding),
            ..FontDictionary::default()
        };
        let metrics = FontMetrics::from_dictionary(dict).expect("standard-14 font");
        assert!(!metrics.is_embedded());
        assert_eq!(metrics.standard14(), Some(Standard14Font::Helvetica));
        assert_eq!(metrics.glyph_width(0x41), 667.0);
        assert!(close(metrics.advance_for_code(0x41), 0.667));
        assert_eq!(metrics.ascent(), 718.0);
        assert_eq!(metrics.descent(), -207.0);
        assert_eq!(
            metrics.font_bbox(),
            FontBBox::new(-166.0, -225.0, 1000.0, 931.0)
        );
    }

    #[test]
    fn font_matrix_transforms_and_non_finite_matrices_are_rejected() {
        let matrix = FontMatrix::typical();
        assert_eq!(matrix.transform(500.0, 0.0), (0.5, 0.0));
        assert_eq!(matrix.x_scale(), 0.001);
        assert_eq!(FontMatrix::identity().transform(3.0, 4.0), (3.0, 4.0));

        let mut dict = simple_dict("X", FontSubtype::Type1, 65, &[600.0]);
        dict.font_matrix = Some(FontMatrix::new(f32::INFINITY, 0.0, 0.0, 1.0, 0.0, 0.0));
        assert!(matches!(
            FontMetrics::from_dictionary(dict),
            Err(FontError::Invalid {
                field: "FontMatrix",
                ..
            })
        ));
    }

    #[test]
    fn pdf_names_and_encodings_parse() {
        assert_eq!(FontSubtype::from_pdf_name("/Type1"), Ok(FontSubtype::Type1));
        assert_eq!(
            FontSubtype::from_pdf_name("TrueType"),
            Ok(FontSubtype::TrueType)
        );
        assert!(matches!(
            FontSubtype::from_pdf_name("Unknown"),
            Err(FontError::UnsupportedSubtype(_))
        ));
        assert_eq!(
            CidSubtype::from_pdf_name("/CIDFontType2"),
            Ok(CidSubtype::CidFontType2)
        );
        assert_eq!(
            FontEncoding::from_pdf_name("Identity-H"),
            FontEncoding::IdentityH
        );
        assert_eq!(
            FontEncoding::from_pdf_name("UniJIS-UCS2-H"),
            FontEncoding::NamedCMap("UniJIS-UCS2-H".into())
        );
    }

    #[test]
    fn simple_encoding_fallback_recovers_unicode() {
        let metrics =
            FontMetrics::from_dictionary(simple_dict("X", FontSubtype::Type1, 0, &[600.0]))
                .expect("valid dictionary with StandardEncoding default");
        assert_eq!(metrics.encoding(), &FontEncoding::StandardEncoding);
        assert_eq!(metrics.unicode_for(0x41).as_deref(), Some("A"));
        // CIDs in a composite encoding cannot be recovered without /ToUnicode.
        let composite = FontMetrics::from_dictionary(type0_dict()).expect("composite font");
        assert_eq!(composite.unicode_for(0x41), None);
    }
}
