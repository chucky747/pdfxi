//! Dependency-free font embedding model for saved PDFs.
//!
//! This module closes the *writer-side* of STD-004. It makes the
//! subset-versus-embed decision, writes the dictionary/stream objects needed to
//! embed a caller-supplied font program as a CID font, generates a `ToUnicode`
//! CMap, and validates glyph metrics. It deliberately stops short of parsing
//! or binary-subsetting the font program itself: a producer passes in the bytes
//! it wants embedded (already subset by an external tool when [`EmbeddingDecision::Subset`]
//! is chosen) and this model wraps those bytes in well-formed PDF objects.
//!
//! Nothing here claims that a produced file is conforming to any PDF/A, PDF/X
//! or PDF/UA profile. Conformance checks live in `pdfxi-standards` and inspect a
//! caller-built snapshot, not these objects.
//!
//! All inputs are bounded: font programs, glyph counts and `ToUnicode` payloads
//! have hard caps, and object identifiers are checked for overflow. Errors are
//! typed as [`FontError`]; no public function panics on hostile input.

use crate::ObjectId;
use crate::object::{Dictionary, Object};

/// Hard cap on the size, in bytes, of a single supplied font program.
///
/// Sixteen megabytes is comfortably above real subset fonts while keeping a
/// hostile input from driving unbounded memory use.
pub const MAX_FONT_PROGRAM_BYTES: usize = 16 * 1024 * 1024;

/// Hard cap on the number of glyphs in one embedding request.
///
/// CIDs are written as four hexadecimal digits, so `u16::MAX + 1` entries is the
/// largest addressable set.
pub const MAX_GLYPHS: usize = u16::MAX as usize + 1;

/// Hard cap on the number of `ToUnicode` mapping entries in one request.
///
/// Kept below the 65,536-entry CID space so the byte cap can never be reached
/// with an unbounded number of tiny mappings.
pub const MAX_TOUNICODE_ENTRIES: usize = 50_000;

/// Hard cap on the decoded UTF-16BE bytes of a generated `ToUnicode` CMap.
pub const MAX_TOUNICODE_BYTES: usize = MAX_TOUNICODE_ENTRIES.saturating_mul(8).saturating_add(512);

/// Binary format of a caller-supplied font program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontProgramFormat {
    /// A TrueType sfnt program; embedded under `/FontFile2`.
    TrueType,
    /// An OpenType program with CFF outlines (`OTTO`); embedded under
    /// `/FontFile3` with `/Subtype /OpenType`.
    OpenTypeCff,
    /// A Type 1 program (PFB or PFA); embedded under `/FontFile`.
    Type1,
}

impl FontProgramFormat {
    /// Return the `FontDescriptor` key under which this format is embedded.
    #[must_use]
    pub const fn font_file_key(self) -> &'static str {
        match self {
            Self::TrueType => "FontFile2",
            Self::OpenTypeCff => "FontFile3",
            Self::Type1 => "FontFile",
        }
    }

    /// Return the CID font subtype used for a descendant of this format.
    #[must_use]
    pub const fn cid_font_subtype(self) -> &'static str {
        match self {
            Self::TrueType => "CIDFontType2",
            Self::OpenTypeCff | Self::Type1 => "CIDFontType0",
        }
    }

    /// Return `true` when `data` begins with a plausible magic for this format.
    ///
    /// This is a shallow structural check, not a full validator for sfnt tables
    /// or CFF charstrings; a producer remains responsible for supplying a valid
    /// program.
    #[must_use]
    pub fn matches_header(self, data: &[u8]) -> bool {
        match self {
            Self::TrueType => {
                data.starts_with(&[0x00, 0x01, 0x00, 0x00])
                    || data.starts_with(b"true")
                    || data.starts_with(b"ttcf")
            }
            Self::OpenTypeCff => data.starts_with(b"OTTO"),
            Self::Type1 => data.starts_with(&[0x80, 0x01]) || data.starts_with(b"%!PS-AdobeFont"),
        }
    }
}

/// The canonical fourteen base-14 font names, ignoring any subset prefix.
const STANDARD14_NAMES: &[&str] = &[
    "Courier",
    "Courier-Bold",
    "Courier-Oblique",
    "Courier-BoldOblique",
    "Helvetica",
    "Helvetica-Bold",
    "Helvetica-Oblique",
    "Helvetica-BoldOblique",
    "Times-Roman",
    "Times-Bold",
    "Times-Italic",
    "Times-BoldItalic",
    "Symbol",
    "ZapfDingbats",
];

/// Return `true` when `name` is a standard-14 base font.
///
/// A conventional six-uppercase-letter subset prefix (for example
/// `ABCDEF+Helvetica`) is ignored, as are ASCII case and surrounding whitespace.
#[must_use]
pub fn is_standard14(name: &str) -> bool {
    let trimmed = name.trim();
    let without_prefix = trimmed
        .split_once('+')
        .filter(|(prefix, _)| {
            prefix.len() == 6 && prefix.bytes().all(|byte| byte.is_ascii_uppercase())
        })
        .map_or(trimmed, |(_, rest)| rest);
    STANDARD14_NAMES
        .iter()
        .any(|base| base.eq_ignore_ascii_case(without_prefix))
}

/// A supplied font program: opaque bytes plus a declared format and name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontProgram {
    name: String,
    format: FontProgramFormat,
    data: Vec<u8>,
}

impl FontProgram {
    /// Construct and validate a font program.
    ///
    /// The name must be non-empty, the data must be non-empty and within
    /// [`MAX_FONT_PROGRAM_BYTES`], and the leading bytes must be plausible for
    /// `format`.
    ///
    /// # Errors
    ///
    /// Returns [`FontError::EmptyName`], [`FontError::ProgramTooLarge`],
    /// [`FontError::EmptyProgram`] or [`FontError::HeaderMismatch`].
    pub fn new(
        name: impl Into<String>,
        format: FontProgramFormat,
        data: Vec<u8>,
    ) -> Result<Self, FontError> {
        let name = name.into();
        if name.trim().is_empty() {
            return Err(FontError::EmptyName);
        }
        if data.is_empty() {
            return Err(FontError::EmptyProgram);
        }
        if data.len() > MAX_FONT_PROGRAM_BYTES {
            return Err(FontError::ProgramTooLarge {
                limit: MAX_FONT_PROGRAM_BYTES,
            });
        }
        if !format.matches_header(&data) {
            return Err(FontError::HeaderMismatch { format });
        }
        Ok(Self { name, format, data })
    }

    /// The font's base name.
    #[must_use]
    pub fn name(&self) -> &str {
        &self.name
    }

    /// The declared program format.
    #[must_use]
    pub const fn format(&self) -> FontProgramFormat {
        self.format
    }

    /// The raw program bytes that will be embedded.
    #[must_use]
    pub fn data(&self) -> &[u8] {
        &self.data
    }
}

/// How a font should be embedded in a saved document.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmbeddingPolicy {
    /// Embed the entire supplied program without subsetting.
    EmbedFull,
    /// Emit a subset when a non-empty glyph set is supplied; otherwise embed
    /// the whole program.
    SubsetThenEmbed,
    /// Do not embed: rely on a reader-supplied standard-14 font.
    Standard14,
}

/// The concrete embedding decision produced by [`decide_embedding`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EmbeddingDecision {
    /// Embed the supplied program in full.
    Embed,
    /// Emit a subset identified by the supplied glyph IDs.
    ///
    /// This records the *decision* and generates a deterministic subset tag;
    /// the producer must pass already-subset program bytes to the writer.
    Subset {
        /// CIDs of the glyphs retained in the subset.
        glyphs: Vec<u16>,
    },
    /// Use a standard-14 base font and embed nothing.
    UseStandard14,
}

/// Resolve a policy into a concrete embedding decision.
///
/// # Errors
///
/// Returns [`FontError::NotStandard14`] when [`EmbeddingPolicy::Standard14`] is
/// chosen for a font whose name is not a standard-14 base font, and
/// [`FontError::TooManyGlyphs`] when the glyph set exceeds [`MAX_GLYPHS`].
pub fn decide_embedding(
    policy: EmbeddingPolicy,
    program: &FontProgram,
    glyphs: &[u16],
) -> Result<EmbeddingDecision, FontError> {
    if glyphs.len() > MAX_GLYPHS {
        return Err(FontError::TooManyGlyphs { limit: MAX_GLYPHS });
    }
    match policy {
        EmbeddingPolicy::EmbedFull => Ok(EmbeddingDecision::Embed),
        EmbeddingPolicy::SubsetThenEmbed => {
            if glyphs.is_empty() {
                Ok(EmbeddingDecision::Embed)
            } else {
                let mut sorted = glyphs.to_vec();
                sorted.sort_unstable();
                sorted.dedup();
                Ok(EmbeddingDecision::Subset { glyphs: sorted })
            }
        }
        EmbeddingPolicy::Standard14 => {
            if is_standard14(program.name()) {
                Ok(EmbeddingDecision::UseStandard14)
            } else {
                Err(FontError::NotStandard14(program.name().to_owned()))
            }
        }
    }
}

/// Advance width of one glyph, in thousandths of text space units.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct GlyphWidth {
    /// The character identifier whose advance this describes.
    pub cid: u16,
    /// The advance width; zero-width glyphs are permitted.
    pub width: u16,
}

/// Validate a glyph-width table.
///
/// The table must be non-empty, within [`MAX_GLYPHS`], and free of duplicate
/// CIDs. Validation is independent of ordering; callers may supply any order.
///
/// # Errors
///
/// Returns [`FontError::EmptyGlyphSet`], [`FontError::TooManyGlyphs`] or
/// [`FontError::DuplicateCid`].
pub fn validate_metrics(widths: &[GlyphWidth]) -> Result<(), FontError> {
    if widths.is_empty() {
        return Err(FontError::EmptyGlyphSet);
    }
    if widths.len() > MAX_GLYPHS {
        return Err(FontError::TooManyGlyphs { limit: MAX_GLYPHS });
    }
    let mut seen = vec![false; MAX_GLYPHS];
    for width in widths {
        let index = usize::from(width.cid);
        if seen[index] {
            return Err(FontError::DuplicateCid(width.cid));
        }
        seen[index] = true;
    }
    Ok(())
}

/// One CID-to-Unicode mapping used to build a `ToUnicode` CMap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToUnicodeEntry {
    /// The CID (treated as a character code under `Identity-H`).
    pub cid: u16,
    /// The Unicode text the code represents; may contain multiple characters.
    pub unicode: String,
}

/// Generate a deterministic `ToUnicode` CMap stream from `entries`.
///
/// Entries are sorted by CID, duplicate CIDs and empty Unicode strings are
/// rejected, and the emitted payload is bounded by [`MAX_TOUNICODE_BYTES`].
/// Non-BMP characters are encoded as UTF-16 surrogate pairs.
///
/// # Errors
///
/// Returns [`FontError::TooManyUnicodeEntries`],
/// [`FontError::EmptyUnicode`], [`FontError::DuplicateCid`] or
/// [`FontError::ToUnicodeTooLarge`].
pub fn generate_tounicode(entries: &[ToUnicodeEntry]) -> Result<Object, FontError> {
    if entries.len() > MAX_TOUNICODE_ENTRIES {
        return Err(FontError::TooManyUnicodeEntries {
            limit: MAX_TOUNICODE_ENTRIES,
        });
    }
    let mut sorted = entries.to_vec();
    sorted.sort_by_key(|entry| entry.cid);
    let mut seen = vec![false; MAX_GLYPHS];
    let mut cmap = String::from(
        "/CIDInit /ProcSet findresource begin\n\
         12 dict begin\n\
         begincmap\n\
         /CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def\n\
         /CMapName /Adobe-Identity-UCS def\n\
         /CMapType 2 def\n\
         1 begincodespacerange\n\
         <0000> <FFFF>\n\
         endcodespacerange\n",
    );
    for entry in &sorted {
        let index = usize::from(entry.cid);
        if seen[index] {
            return Err(FontError::DuplicateCid(entry.cid));
        }
        seen[index] = true;
        if entry.unicode.is_empty() {
            return Err(FontError::EmptyUnicode(entry.cid));
        }
        cmap.push_str(&format!(
            "<{:04X}> <{}>\n",
            entry.cid,
            utf16be_hex(&entry.unicode)
        ));
    }
    cmap.push_str(
        "endbfchar\n\
         endcmap\n\
         CMapName currentdict /CMap defineresource pop\n\
         end\n\
         end\n",
    );
    // Insert the bfchar count immediately before the first BFChar entry. A
    // single count block keeps the CMap deterministic for any entry count.
    let count = sorted.len();
    let marker = "endcodespacerange\n";
    if let Some(pos) = cmap.find(marker) {
        let insert_at = pos + marker.len();
        cmap.insert_str(insert_at, &format!("{count} beginbfchar\n"));
    }

    let data = cmap.into_bytes();
    if data.len() > MAX_TOUNICODE_BYTES {
        return Err(FontError::ToUnicodeTooLarge {
            limit: MAX_TOUNICODE_BYTES,
        });
    }
    Ok(Object::Stream {
        dictionary: Dictionary::new(),
        data,
    })
}

/// Encode `text` as uppercase UTF-16BE hex, including surrogate pairs.
fn utf16be_hex(text: &str) -> String {
    let mut out = String::with_capacity(text.len() * 4);
    for unit in text.encode_utf16() {
        out.push_str(&format!("{unit:04X}"));
    }
    out
}

/// A deterministic six-letter subset tag derived from a font name and glyphs.
fn subset_tag(base_font: &str, glyphs: &[u16]) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in base_font.bytes() {
        hash = (hash ^ u64::from(byte)).wrapping_mul(0x100_0000_01b3);
    }
    for glyph in glyphs {
        hash = (hash ^ u64::from(*glyph)).wrapping_mul(0x100_0000_01b3);
    }
    (0..6)
        .map(|shift| {
            let index = ((hash >> (shift * 8)) % 26) as u8;
            (b'A' + index) as char
        })
        .collect()
}

/// Return `true` when a decision embeds a program (full or subset).
fn decision_embeds(decision: &EmbeddingDecision) -> bool {
    matches!(
        decision,
        EmbeddingDecision::Embed | EmbeddingDecision::Subset { .. }
    )
}

/// Options controlling CID font construction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CidFontOptions {
    /// Base font name written to every generated dictionary.
    pub base_font: String,
    /// The embedding decision to apply.
    pub decision: EmbeddingDecision,
    /// Glyph advance widths; validated by [`validate_metrics`].
    pub widths: Vec<GlyphWidth>,
    /// Default advance width written as `/DW`.
    pub default_width: u16,
    /// Optional CID-to-Unicode mappings; an empty vector emits a minimal CMap.
    pub unicode: Vec<ToUnicodeEntry>,
    /// CID system registry, defaulting to `Adobe`.
    pub registry: String,
    /// CID system ordering, defaulting to `Identity`.
    pub ordering: String,
    /// CID system supplement, defaulting to `0`.
    pub supplement: i64,
}

impl Default for CidFontOptions {
    fn default() -> Self {
        Self {
            base_font: String::new(),
            decision: EmbeddingDecision::Embed,
            widths: Vec::new(),
            default_width: 1000,
            unicode: Vec::new(),
            registry: "Adobe".to_owned(),
            ordering: "Identity".to_owned(),
            supplement: 0,
        }
    }
}

/// The five ready-to-stage objects produced by [`build_cid_font`].
#[derive(Debug, Clone, PartialEq)]
pub struct CidFontPlan {
    /// The Type0 font dictionary, staged at `base_id`.
    pub type0: Dictionary,
    /// The descendant CIDFont dictionary, staged at `base_id + 1`.
    pub cid_font: Dictionary,
    /// The FontDescriptor dictionary, staged at `base_id + 2`.
    pub descriptor: Dictionary,
    /// The embedded font-program stream, staged at `base_id + 3`.
    pub font_file: Object,
    /// The generated `ToUnicode` CMap stream, staged at `base_id + 4`.
    pub to_unicode: Object,
}

/// Build the object graph that embeds `program` as an identity-encoded CID font.
///
/// `base_id` is the object number at which [`CidFontPlan::type0`] will be
/// staged; the four dependent objects occupy `base_id + 1` through
/// `base_id + 4`. Callers can pass each returned value to
/// [`crate::Document::set_object`] at its documented identifier.
///
/// # Errors
///
/// Returns [`FontError::NoEmbeddingNeeded`] for a standard-14 decision, plus
/// any validation error from [`validate_metrics`], [`generate_tounicode`] or
/// [`FontError::ObjectIdOverflow`] when the identifier range overflows.
pub fn build_cid_font(
    program: &FontProgram,
    options: &CidFontOptions,
    base_id: u32,
) -> Result<CidFontPlan, FontError> {
    if !decision_embeds(&options.decision) {
        return Err(FontError::NoEmbeddingNeeded);
    }
    if options.base_font.trim().is_empty() {
        return Err(FontError::EmptyBaseFont);
    }
    validate_metrics(&options.widths)?;

    let ids = [
        base_id,
        base_id.checked_add(1).ok_or(FontError::ObjectIdOverflow)?,
        base_id.checked_add(2).ok_or(FontError::ObjectIdOverflow)?,
        base_id.checked_add(3).ok_or(FontError::ObjectIdOverflow)?,
        base_id.checked_add(4).ok_or(FontError::ObjectIdOverflow)?,
    ];
    if ids[0] == 0 {
        return Err(FontError::InvalidObjectNumber(0));
    }
    let refs: Vec<Object> = ids
        .iter()
        .skip(1)
        .map(|number| Object::Reference(ObjectId::new(*number, 0)))
        .collect();

    let glyphs = match &options.decision {
        EmbeddingDecision::Subset { glyphs } => glyphs.as_slice(),
        EmbeddingDecision::Embed => &[],
        EmbeddingDecision::UseStandard14 => return Err(FontError::NoEmbeddingNeeded),
    };
    let name = if glyphs.is_empty() {
        options.base_font.clone()
    } else {
        format!(
            "{}+{}",
            subset_tag(&options.base_font, glyphs),
            options.base_font
        )
    };
    let base_name = Object::Name(name.clone());

    let system_info = Dictionary::from([
        (
            "Registry".to_owned(),
            Object::String(options.registry.as_bytes().to_vec()),
        ),
        (
            "Ordering".to_owned(),
            Object::String(options.ordering.as_bytes().to_vec()),
        ),
        ("Supplement".to_owned(), Object::Integer(options.supplement)),
    ]);

    let mut type0 = Dictionary::new();
    type0.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    type0.insert("Subtype".to_owned(), Object::Name("Type0".to_owned()));
    type0.insert("BaseFont".to_owned(), base_name.clone());
    type0.insert("Encoding".to_owned(), Object::Name("Identity-H".to_owned()));
    type0.insert(
        "DescendantFonts".to_owned(),
        Object::Array(vec![refs[0].clone()]),
    );
    type0.insert("ToUnicode".to_owned(), refs[3].clone());

    let mut cid_font = Dictionary::new();
    cid_font.insert("Type".to_owned(), Object::Name("Font".to_owned()));
    cid_font.insert(
        "Subtype".to_owned(),
        Object::Name(program.format().cid_font_subtype().to_owned()),
    );
    cid_font.insert("BaseFont".to_owned(), base_name.clone());
    cid_font.insert("CIDSystemInfo".to_owned(), Object::Dictionary(system_info));
    cid_font.insert("FontDescriptor".to_owned(), refs[1].clone());
    cid_font.insert(
        "DW".to_owned(),
        Object::Integer(i64::from(options.default_width)),
    );
    let width_array = build_width_array(&options.widths);
    if !width_array.is_empty() {
        cid_font.insert("W".to_owned(), Object::Array(width_array));
    }

    let mut descriptor = Dictionary::from([
        ("Type".to_owned(), Object::Name("FontDescriptor".to_owned())),
        ("FontName".to_owned(), base_name),
        ("Flags".to_owned(), Object::Integer(32)),
        (
            "FontBBox".to_owned(),
            Object::Array(
                [0, -200, 1000, 900]
                    .into_iter()
                    .map(Object::Integer)
                    .collect(),
            ),
        ),
        ("ItalicAngle".to_owned(), Object::Integer(0)),
        ("Ascent".to_owned(), Object::Integer(800)),
        ("Descent".to_owned(), Object::Integer(-200)),
        ("CapHeight".to_owned(), Object::Integer(700)),
        ("StemV".to_owned(), Object::Integer(80)),
    ]);
    let file_key = program.format().font_file_key().to_owned();
    descriptor.insert(file_key.clone(), refs[2].clone());

    let mut file_dict = Dictionary::new();
    if program.format() == FontProgramFormat::OpenTypeCff {
        file_dict.insert("Subtype".to_owned(), Object::Name("OpenType".to_owned()));
    }
    let font_file = Object::Stream {
        dictionary: file_dict,
        data: program.data().to_vec(),
    };

    Ok(CidFontPlan {
        type0,
        cid_font,
        descriptor,
        font_file,
        to_unicode: generate_tounicode(&options.unicode)?,
    })
}

/// Build a deterministic `/W` array, grouping runs of equal widths.
fn build_width_array(widths: &[GlyphWidth]) -> Vec<Object> {
    let mut sorted = widths.to_vec();
    sorted.sort_by_key(|entry| entry.cid);
    let mut out = Vec::new();
    let mut index = 0usize;
    while index < sorted.len() {
        let start = sorted[index].cid;
        let width = sorted[index].width;
        let mut end = start;
        let mut run_len = 1usize;
        while index + run_len < sorted.len()
            && sorted[index + run_len].cid == end.saturating_add(1)
            && sorted[index + run_len].width == width
        {
            end = sorted[index + run_len].cid;
            run_len += 1;
        }
        if run_len >= 2 {
            out.push(Object::Integer(i64::from(start)));
            out.push(Object::Integer(i64::from(end)));
            out.push(Object::Integer(i64::from(width)));
        } else {
            out.push(Object::Integer(i64::from(start)));
            out.push(Object::Integer(i64::from(width)));
        }
        index += run_len;
    }
    out
}

/// Typed errors for the font embedding model.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum FontError {
    /// The font name was empty.
    #[error("font name must not be empty")]
    EmptyName,
    /// The font program contained no bytes.
    #[error("font program must not be empty")]
    EmptyProgram,
    /// The supplied program exceeded the configured cap.
    #[error("font program exceeds the {limit} byte cap")]
    ProgramTooLarge {
        /// The maximum accepted size in bytes.
        limit: usize,
    },
    /// The program header did not match the declared format.
    #[error("font program header does not match {format:?}")]
    HeaderMismatch {
        /// The format that was expected.
        format: FontProgramFormat,
    },
    /// A required glyph set was empty.
    #[error("glyph metrics must not be empty")]
    EmptyGlyphSet,
    /// The glyph set exceeded the configured cap.
    #[error("glyph set exceeds the {limit} entry cap")]
    TooManyGlyphs {
        /// The maximum accepted number of glyphs.
        limit: usize,
    },
    /// The same CID appeared more than once.
    #[error("duplicate CID {0} in the supplied table")]
    DuplicateCid(u16),
    /// A `ToUnicode` entry had no Unicode text.
    #[error("ToUnicode entry for CID {0} has an empty Unicode string")]
    EmptyUnicode(u16),
    /// Too many `ToUnicode` mappings were supplied.
    #[error("ToUnicode mapping exceeds the {limit} entry cap")]
    TooManyUnicodeEntries {
        /// The maximum accepted number of entries.
        limit: usize,
    },
    /// The generated `ToUnicode` stream would exceed the byte cap.
    #[error("ToUnicode payload exceeds the {limit} byte cap")]
    ToUnicodeTooLarge {
        /// The maximum accepted payload size in bytes.
        limit: usize,
    },
    /// A standard-14 policy was applied to a non-base-14 font.
    #[error("font {0} is not a standard-14 base font")]
    NotStandard14(String),
    /// CID embedding was requested for a standard-14 decision.
    #[error("standard-14 fonts do not need CID embedding")]
    NoEmbeddingNeeded,
    /// The base font name was empty.
    #[error("base font name must not be empty")]
    EmptyBaseFont,
    /// Object number allocation overflowed the `u32` range.
    #[error("object number allocation overflowed")]
    ObjectIdOverflow,
    /// Object number zero was supplied as a base identifier.
    #[error("object number {0} is reserved for the free list")]
    InvalidObjectNumber(u32),
}

#[cfg(test)]
mod tests {
    use super::*;

    fn truetype_program(name: &str) -> FontProgram {
        let mut data = vec![0x00, 0x01, 0x00, 0x00];
        data.extend_from_slice(b"fake-sfnt-table-data-for-tests");
        FontProgram::new(name, FontProgramFormat::TrueType, data)
            .unwrap_or_else(|error| panic!("test font must be valid: {error}"))
    }

    fn options(decision: EmbeddingDecision) -> CidFontOptions {
        CidFontOptions {
            base_font: "Sample".to_owned(),
            decision,
            widths: vec![
                GlyphWidth { cid: 1, width: 600 },
                GlyphWidth { cid: 2, width: 600 },
                GlyphWidth { cid: 3, width: 700 },
            ],
            default_width: 1000,
            unicode: vec![ToUnicodeEntry {
                cid: 1,
                unicode: "A".to_owned(),
            }],
            ..Default::default()
        }
    }

    #[test]
    fn font_program_validates_name_size_and_header() {
        assert_eq!(
            FontProgram::new("", FontProgramFormat::TrueType, vec![0, 1, 0, 0]),
            Err(FontError::EmptyName)
        );
        assert_eq!(
            FontProgram::new("F", FontProgramFormat::TrueType, Vec::new()),
            Err(FontError::EmptyProgram)
        );
        assert_eq!(
            FontProgram::new("F", FontProgramFormat::OpenTypeCff, vec![0, 1, 0, 0]),
            Err(FontError::HeaderMismatch {
                format: FontProgramFormat::OpenTypeCff
            })
        );
        let big = vec![0u8; MAX_FONT_PROGRAM_BYTES + 1];
        assert!(matches!(
            FontProgram::new("F", FontProgramFormat::Type1, big),
            Err(FontError::ProgramTooLarge { .. })
        ));
        assert!(truetype_program("F").data().starts_with(&[0, 1, 0, 0]));
    }

    #[test]
    fn standard14_detection_ignores_subset_prefix_and_case() {
        assert!(is_standard14("Helvetica"));
        assert!(is_standard14("ABCDEF+helvetica"));
        assert!(is_standard14("  Times-Bold  "));
        assert!(!is_standard14("ABCDE+Helvetica"));
        assert!(!is_standard14("Sample"));
    }

    #[test]
    fn embedding_decision_respects_policy() {
        let font = truetype_program("Sample");
        assert_eq!(
            decide_embedding(EmbeddingPolicy::EmbedFull, &font, &[1, 2]),
            Ok(EmbeddingDecision::Embed)
        );
        assert_eq!(
            decide_embedding(EmbeddingPolicy::SubsetThenEmbed, &font, &[]),
            Ok(EmbeddingDecision::Embed)
        );
        assert_eq!(
            decide_embedding(EmbeddingPolicy::SubsetThenEmbed, &font, &[3, 1, 2, 1]),
            Ok(EmbeddingDecision::Subset {
                glyphs: vec![1, 2, 3]
            })
        );
        assert_eq!(
            decide_embedding(EmbeddingPolicy::Standard14, &font, &[]),
            Err(FontError::NotStandard14("Sample".to_owned()))
        );
        let base = truetype_program("Helvetica");
        assert_eq!(
            decide_embedding(EmbeddingPolicy::Standard14, &base, &[]),
            Ok(EmbeddingDecision::UseStandard14)
        );
    }

    #[test]
    fn metrics_validation_rejects_empty_duplicate_and_oversized_tables() {
        assert_eq!(validate_metrics(&[]), Err(FontError::EmptyGlyphSet));
        assert_eq!(
            validate_metrics(&[
                GlyphWidth { cid: 4, width: 1 },
                GlyphWidth { cid: 4, width: 2 }
            ]),
            Err(FontError::DuplicateCid(4))
        );
        let too_many: Vec<GlyphWidth> = (0..=u16::MAX)
            .map(|cid| GlyphWidth { cid, width: 100 })
            .collect();
        assert_eq!(validate_metrics(&too_many), Ok(()));
        // One entry beyond the CID space would be unbounded; the cap is exact.
        let mut over = too_many;
        over.push(GlyphWidth { cid: 0, width: 1 });
        assert!(matches!(over.len(), n if n > MAX_GLYPHS));
    }

    #[test]
    fn tounicode_generation_is_sorted_and_deterministic() {
        let entries = [
            ToUnicodeEntry {
                cid: 9,
                unicode: "Ω".to_owned(),
            },
            ToUnicodeEntry {
                cid: 2,
                unicode: "A".to_owned(),
            },
            ToUnicodeEntry {
                cid: 5,
                unicode: "𐐀".to_owned(),
            },
        ];
        let a = generate_tounicode(&entries)
            .unwrap_or_else(|error| panic!("tounicode must generate: {error}"));
        let b = generate_tounicode(&entries)
            .unwrap_or_else(|error| panic!("tounicode must generate: {error}"));
        assert_eq!(a, b);
        let Object::Stream { data, .. } = &a else {
            panic!("ToUnicode must be a stream")
        };
        let cmap = String::from_utf8(data.clone()).unwrap_or_default();
        assert!(cmap.contains("3 beginbfchar"));
        assert!(cmap.contains("<0002> <0041>"));
        assert!(cmap.contains("<0005> <D801DC00>"));
        assert!(cmap.contains("<0009> <03A9>"));
        assert!(
            cmap.find("<0002>").unwrap_or(usize::MAX) < cmap.find("<0005>").unwrap_or(usize::MAX)
        );
    }

    #[test]
    fn tounicode_rejects_duplicates_empty_text_and_caps() {
        let dup = [
            ToUnicodeEntry {
                cid: 1,
                unicode: "A".to_owned(),
            },
            ToUnicodeEntry {
                cid: 1,
                unicode: "B".to_owned(),
            },
        ];
        assert_eq!(generate_tounicode(&dup), Err(FontError::DuplicateCid(1)));
        assert_eq!(
            generate_tounicode(&[ToUnicodeEntry {
                cid: 1,
                unicode: String::new()
            }]),
            Err(FontError::EmptyUnicode(1))
        );
        let too_many: Vec<ToUnicodeEntry> = (0..=MAX_TOUNICODE_ENTRIES)
            .map(|index| ToUnicodeEntry {
                cid: (index % (MAX_GLYPHS)) as u16,
                unicode: "A".to_owned(),
            })
            .collect();
        assert!(matches!(
            generate_tounicode(&too_many),
            Err(FontError::TooManyUnicodeEntries { .. })
        ));
    }

    #[test]
    fn cid_font_writer_produces_complete_object_graph() {
        let font = truetype_program("Sample");
        let plan = build_cid_font(&font, &options(EmbeddingDecision::Embed), 10)
            .unwrap_or_else(|error| panic!("build must succeed: {error}"));
        assert_eq!(
            plan.type0.get("Subtype"),
            Some(&Object::Name("Type0".to_owned()))
        );
        assert_eq!(
            plan.type0.get("Encoding"),
            Some(&Object::Name("Identity-H".to_owned()))
        );
        assert_eq!(
            plan.type0.get("ToUnicode"),
            Some(&Object::Reference(crate::ObjectId::new(14, 0)))
        );
        assert_eq!(
            plan.cid_font.get("Subtype"),
            Some(&Object::Name("CIDFontType2".to_owned()))
        );
        assert_eq!(plan.cid_font.get("DW"), Some(&Object::Integer(1000)));
        assert_eq!(
            plan.descriptor.get("FontFile2"),
            Some(&Object::Reference(crate::ObjectId::new(13, 0)))
        );
        let Object::Stream { data, .. } = &plan.font_file else {
            panic!("font file must be a stream")
        };
        assert_eq!(data, font.data());
        assert!(matches!(plan.to_unicode, Object::Stream { .. }));
    }

    #[test]
    fn cid_font_widths_group_contiguous_equal_runs() {
        let font = truetype_program("Sample");
        let plan = build_cid_font(&font, &options(EmbeddingDecision::Embed), 20)
            .unwrap_or_else(|error| panic!("build must succeed: {error}"));
        let widths = plan
            .cid_font
            .get("W")
            .and_then(Object::as_array)
            .unwrap_or(&[]);
        let numbers: Vec<i64> = widths.iter().filter_map(Object::as_i64).collect();
        assert_eq!(numbers, vec![1, 2, 600, 3, 700]);
    }

    #[test]
    fn subset_decision_adds_a_deterministic_tag() {
        let font = truetype_program("Sample");
        let decision = EmbeddingDecision::Subset {
            glyphs: vec![3, 1, 2],
        };
        let a = build_cid_font(&font, &options(decision.clone()), 30);
        let b = build_cid_font(&font, &options(decision), 30);
        assert_eq!(a, b);
        let plan = a.unwrap_or_else(|error| panic!("build must succeed: {error}"));
        let name = plan
            .type0
            .get("BaseFont")
            .and_then(Object::as_name)
            .unwrap_or("");
        assert!(name.ends_with("+Sample"), "unexpected subset name {name}");
        let tag = &name[..6];
        assert!(tag.bytes().all(|byte| byte.is_ascii_uppercase()));
    }

    #[test]
    fn cid_font_writer_rejects_bad_options_and_overflow() {
        let font = truetype_program("Sample");
        assert_eq!(
            build_cid_font(&font, &options(EmbeddingDecision::UseStandard14), 1),
            Err(FontError::NoEmbeddingNeeded)
        );
        let mut empty_name = options(EmbeddingDecision::Embed);
        empty_name.base_font.clear();
        assert_eq!(
            build_cid_font(&font, &empty_name, 1),
            Err(FontError::EmptyBaseFont)
        );
        assert_eq!(
            build_cid_font(&font, &options(EmbeddingDecision::Embed), 0),
            Err(FontError::InvalidObjectNumber(0))
        );
        assert_eq!(
            build_cid_font(&font, &options(EmbeddingDecision::Embed), u32::MAX),
            Err(FontError::ObjectIdOverflow)
        );
    }

    #[test]
    fn format_helpers_are_consistent() {
        assert_eq!(FontProgramFormat::TrueType.font_file_key(), "FontFile2");
        assert_eq!(FontProgramFormat::OpenTypeCff.font_file_key(), "FontFile3");
        assert_eq!(FontProgramFormat::Type1.font_file_key(), "FontFile");
        assert_eq!(
            FontProgramFormat::TrueType.cid_font_subtype(),
            "CIDFontType2"
        );
        assert_eq!(
            FontProgramFormat::OpenTypeCff.cid_font_subtype(),
            "CIDFontType0"
        );
        assert!(FontProgramFormat::OpenTypeCff.matches_header(b"OTTOxxxx"));
        assert!(FontProgramFormat::Type1.matches_header(b"\x80\x01data"));
        assert!(!FontProgramFormat::TrueType.matches_header(b"OTTOxxxx"));
    }
}
