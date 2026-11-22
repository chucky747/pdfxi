//! Plain-data snapshot of the document facts a preflight check needs.
//!
//! [`PreflightInput`] is intentionally a dependency-free value type. It does
//! not read PDF bytes and knows nothing about `pdfxi-core`: a caller parses or
//! walks a document and fills in these fields, then hands the snapshot to a
//! document profile such as [`crate::PdfA1bProfile`]. Keeping the boundary as
//! plain data means the standards crate stays dependency-free (see
//! `docs/ARCHITECTURE.md`) and every check remains deterministic and testable
//! without a parser.
//!
//! All collections are [`Vec`]s rather than hash maps so that iteration order
//! is caller-controlled and finding output can be canonicalised by
//! [`crate::sort_findings`] regardless of the order in which facts were added.

use std::fmt;

/// A PDF version as its numeric `(major, minor)` components.
///
/// Storing numbers rather than a string makes range comparisons exact. Use
/// [`PdfVersion::parse`] to interpret a `%PDF-x.y` header.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PdfVersion {
    /// The major version (for example `1` in `1.7`).
    pub major: u8,
    /// The minor version (for example `7` in `1.7`).
    pub minor: u8,
}

impl PdfVersion {
    /// Creates a version from its numeric components.
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// Parses an `x.y` version string.
    ///
    /// Returns `None` for any text that is not exactly one ASCII digit, a dot,
    /// and one ASCII digit. Callers should treat `None` as "version unknown".
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        let bytes = text.as_bytes();
        if bytes.len() != 3 || bytes[1] != b'.' {
            return None;
        }
        if !bytes[0].is_ascii_digit() || !bytes[2].is_ascii_digit() {
            return None;
        }
        Some(Self::new(bytes[0] - b'0', bytes[2] - b'0'))
    }
}

impl fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// One OutputIntent dictionary from the document catalog.
///
/// The raw `/S` name is preserved so a profile can require a specific
/// registered subtype such as `GTS_PDFA1` (PDF/A) or `GTS_PDFX` (PDF/X).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutputIntent {
    /// The `/S` subtype name, usually `GTS_PDFA1` or `GTS_PDFX`.
    ///
    /// A leading slash is tolerated by [`OutputIntent::is_subtype`].
    pub subtype: String,
    /// Whether the intent carries a `/DestOutputProfile` ICC stream.
    pub has_dest_output_profile: bool,
}

impl OutputIntent {
    /// Creates an output intent from a subtype name and profile flag.
    #[must_use]
    pub fn new(subtype: impl Into<String>, has_dest_output_profile: bool) -> Self {
        Self {
            subtype: subtype.into(),
            has_dest_output_profile,
        }
    }

    /// Compares this intent's subtype to `name`, ignoring ASCII case and an
    /// optional leading slash.
    #[must_use]
    pub fn is_subtype(&self, name: &str) -> bool {
        let stored = self.subtype.strip_prefix('/').unwrap_or(&self.subtype);
        let query = name.strip_prefix('/').unwrap_or(name);
        stored.eq_ignore_ascii_case(query)
    }
}

/// Catalog-level flags that preflight profiles inspect.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CatalogInfo {
    /// Every `/OutputIntent` entry in the catalog, in document order.
    pub output_intents: Vec<OutputIntent>,
    /// Whether the catalog has an XMP `/Metadata` stream.
    pub has_xmp_metadata: bool,
    /// Whether a `/MarkInfo` dictionary is present.
    pub has_mark_info: bool,
    /// Whether `/MarkInfo /Marked` is true.
    pub is_marked: bool,
    /// Whether the catalog has a `/Lang` entry.
    pub has_lang: bool,
    /// Whether the catalog has a `/StructTreeRoot` entry.
    pub has_struct_tree_root: bool,
    /// Whether the catalog has an `/AcroForm` dictionary.
    pub has_acro_form: bool,
    /// Whether the catalog has a `/DPartRoot` entry (PDF/VT document parts).
    pub has_dpart_root: bool,
    /// Catalog or document-level entries that are present and that a target
    /// profile may prohibit, for example `/EmbeddedFiles` or `/JavaScript`.
    ///
    /// This list is opt-in from the caller; profiles only flag entries they
    /// explicitly forbid, and fixups only remove entries the profile forbids.
    pub forbidden_entries: Vec<String>,
}

/// The five standard page boxes, reduced to presence flags.
///
/// Preflight only needs to know whether a box is defined, not its geometry.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PageBoxes {
    /// Whether `/MediaBox` is present.
    pub has_media_box: bool,
    /// Whether `/CropBox` is present.
    pub has_crop_box: bool,
    /// Whether `/BleedBox` is present.
    pub has_bleed_box: bool,
    /// Whether `/TrimBox` is present.
    pub has_trim_box: bool,
    /// Whether `/ArtBox` is present.
    pub has_art_box: bool,
}

impl PageBoxes {
    /// Returns boxes with all five entries present.
    #[must_use]
    pub fn all_present() -> Self {
        Self {
            has_media_box: true,
            has_crop_box: true,
            has_bleed_box: true,
            has_trim_box: true,
            has_art_box: true,
        }
    }
}

/// Facts about one page.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageInfo {
    /// The one-based page number used in finding locations.
    pub number: usize,
    /// The page boxes that are present.
    pub boxes: PageBoxes,
    /// Whether the page carries a `/DPart` association (PDF/VT).
    pub has_dpart: bool,
}

/// The font type of a font resource.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FontType {
    /// Type 1.
    Type1,
    /// TrueType.
    TrueType,
    /// Type 3.
    Type3,
    /// Type 0 (composite).
    Type0,
    /// Multiple-master Type 1.
    MmType1,
    /// Any other or unknown font type.
    Other,
}

impl fmt::Display for FontType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Type1 => "Type1",
            Self::TrueType => "TrueType",
            Self::Type3 => "Type3",
            Self::Type0 => "Type0",
            Self::MmType1 => "MMType1",
            Self::Other => "Other",
        };
        f.write_str(label)
    }
}

/// One font resource and whether its font program is embedded.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FontInfo {
    /// The resource name, for example `/F1`.
    pub name: String,
    /// The font type, used to make messages actionable.
    pub font_type: FontType,
    /// Whether the font program is embedded in the file.
    pub embedded: bool,
}

/// A colour space that occurs in the document's resources or content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColourSpace {
    /// Device gray.
    DeviceGray,
    /// Device RGB.
    DeviceRgb,
    /// Device CMYK.
    DeviceCmyk,
    /// ICC-based colour.
    IccBased,
    /// Calibrated gray.
    CalGray,
    /// Calibrated RGB.
    CalRgb,
    /// CIE Lab.
    Lab,
    /// Indexed colour.
    Indexed,
    /// Separation (spot) colour.
    Separation,
    /// DeviceN colour.
    DeviceN,
    /// Pattern colour space.
    Pattern,
}

impl fmt::Display for ColourSpace {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::DeviceGray => "DeviceGray",
            Self::DeviceRgb => "DeviceRGB",
            Self::DeviceCmyk => "DeviceCMYK",
            Self::IccBased => "ICCBased",
            Self::CalGray => "CalGray",
            Self::CalRgb => "CalRGB",
            Self::Lab => "Lab",
            Self::Indexed => "Indexed",
            Self::Separation => "Separation",
            Self::DeviceN => "DeviceN",
            Self::Pattern => "Pattern",
        };
        f.write_str(label)
    }
}

/// The primary filter of an image XObject.
///
/// Only the filter that determines the compression family is recorded; a
/// caller should supply the outermost/decode-relevant filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ImageFilter {
    /// DCTDecode (JPEG).
    DctDecode,
    /// JPXDecode (JPEG 2000).
    JpxDecode,
    /// JBIG2Decode.
    Jbig2Decode,
    /// FlateDecode (zlib).
    FlateDecode,
    /// CCITTFaxDecode.
    CcittFaxDecode,
    /// LZWDecode.
    LzwDecode,
    /// RunLengthDecode.
    RunLengthDecode,
    /// ASCII85Decode.
    Ascii85Decode,
    /// ASCIIHexDecode.
    AsciiHexDecode,
    /// No compression filter.
    None,
}

impl fmt::Display for ImageFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::DctDecode => "DCTDecode",
            Self::JpxDecode => "JPXDecode",
            Self::Jbig2Decode => "JBIG2Decode",
            Self::FlateDecode => "FlateDecode",
            Self::CcittFaxDecode => "CCITTFaxDecode",
            Self::LzwDecode => "LZWDecode",
            Self::RunLengthDecode => "RunLengthDecode",
            Self::Ascii85Decode => "ASCII85Decode",
            Self::AsciiHexDecode => "ASCIIHexDecode",
            Self::None => "None",
        };
        f.write_str(label)
    }
}

/// One image XObject and its compression filter.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImageInfo {
    /// The resource name, for example `/Im1`.
    pub name: String,
    /// The compression filter used by the image.
    pub filter: ImageFilter,
}

/// Transparency usage in the document.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TransparencyUsage {
    /// Whether any transparency construct is used.
    pub uses_transparency: bool,
    /// Human-readable locations (for example `page 2`) where it occurs.
    ///
    /// Empty is valid when `uses_transparency` is true but no precise location
    /// is available; the finding then has no location.
    pub locations: Vec<String>,
}

/// One annotation and the presence of its appearance stream.
///
/// Preflight profiles use the `/Subtype` name to detect annotation types a
/// target standard forbids and the `/AP` flag to require appearance streams.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AnnotationInfo {
    /// The annotation `/Subtype` name, for example `/Text`.
    pub subtype: String,
    /// Whether the annotation dictionary carries an `/AP` appearance stream.
    pub has_appearance_stream: bool,
}

impl AnnotationInfo {
    /// Creates an annotation record from a subtype and appearance-stream flag.
    #[must_use]
    pub fn new(subtype: impl Into<String>, has_appearance_stream: bool) -> Self {
        Self {
            subtype: subtype.into(),
            has_appearance_stream,
        }
    }

    /// Compares this annotation's subtype to `name`, ignoring ASCII case and an
    /// optional leading slash.
    #[must_use]
    pub fn is_subtype(&self, name: &str) -> bool {
        let stored = self.subtype.strip_prefix('/').unwrap_or(&self.subtype);
        let query = name.strip_prefix('/').unwrap_or(name);
        stored.eq_ignore_ascii_case(query)
    }
}

/// One simplified structure element from the tagged structure tree.
///
/// The caller reduces a real structure tree to these plain facts. Identifiers
/// and reading-order positions are caller-assigned but must be stable for the
/// document; the PDF/UA checks in [`crate::structure`] use them to verify parent
/// links and reading order without parsing PDF objects.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StructElementInfo {
    /// Stable, document-unique identifier used by parent and parent-tree links.
    pub id: usize,
    /// Structure type (role) name without a leading slash, for example `Figure`.
    pub role: String,
    /// The element's `/Alt` replacement text, when present.
    pub alt_text: Option<String>,
    /// Whether the element is an artifact and therefore exempt from alt text.
    pub is_artifact: bool,
    /// Identifier of the parent element; `None` for the structure-tree root.
    pub parent: Option<usize>,
    /// Reading-order position; a parent must precede each of its children.
    pub reading_order: usize,
}

/// Per-page marked-content and structure-parent facts used by PDF/UA checks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PageStructureInfo {
    /// One-based page number used in finding locations.
    pub page_number: usize,
    /// Whether the page dictionary carries a `/StructParents` entry.
    pub has_struct_parents: bool,
    /// Whether the page contains text drawing operators.
    pub has_text: bool,
    /// Whether all text on the page sits inside tagged marked content.
    pub has_tagged_text: bool,
    /// MCIDs of marked-content sequences, in content-stream order.
    pub mcids: Vec<u32>,
    /// `/StructParents` array: structure element id indexed by MCID.
    pub struct_parents: Vec<usize>,
}

/// A dependency-free snapshot of the facts preflight profiles need.
///
/// A caller adapts `pdfxi-core` (or any other PDF model) into this struct by
/// filling the fields from the parsed document. Unset collections mean "no
/// such objects were found"; `page_count` is kept separate from `pages` so a
/// caller that has only the page-tree count can still be checked for the pages
/// it knows about.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PreflightInput {
    /// The version declared by the `%PDF-x.y` header.
    pub version: PdfVersion,
    /// Whether the document uses encryption.
    pub is_encrypted: bool,
    /// Catalog-level flags and entries.
    pub catalog: CatalogInfo,
    /// The total number of pages reported by the page tree.
    pub page_count: usize,
    /// Per-page facts, in any order the caller finds convenient.
    pub pages: Vec<PageInfo>,
    /// Font resources, in any order.
    pub fonts: Vec<FontInfo>,
    /// Colour spaces used by resources or content.
    pub colour_spaces: Vec<ColourSpace>,
    /// Image XObjects, in any order.
    pub images: Vec<ImageInfo>,
    /// Transparency usage.
    pub transparency: TransparencyUsage,
    /// Annotations found in the document, in any order.
    pub annotations: Vec<AnnotationInfo>,
    /// Raw bytes of the required output intent's `/DestOutputProfile` stream.
    ///
    /// `None` means the caller did not supply the bytes; profiles then keep the
    /// existing output-intent findings and do not claim ICC validation.
    pub output_intent_icc: Option<Vec<u8>>,
    /// Simplified structure-tree elements used by PDF/UA checks.
    pub struct_elements: Vec<StructElementInfo>,
    /// Per-page marked-content facts used by PDF/UA checks.
    pub page_structure: Vec<PageStructureInfo>,
    /// Custom roles declared in the catalog `/RoleMap`.
    pub role_map_roles: Vec<String>,
}

impl PreflightInput {
    /// Returns the first output intent matching `subtype`, if any.
    #[must_use]
    pub fn output_intent_of(&self, subtype: &str) -> Option<&OutputIntent> {
        self.catalog
            .output_intents
            .iter()
            .find(|intent| intent.is_subtype(subtype))
    }

    /// Returns `true` when per-page facts were supplied for every page.
    #[must_use]
    pub fn has_complete_page_data(&self) -> bool {
        self.pages.len() == self.page_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_parse_and_display_are_exact() {
        assert_eq!(PdfVersion::parse("1.7"), Some(PdfVersion::new(1, 7)));
        assert_eq!(PdfVersion::parse("2.0"), Some(PdfVersion::new(2, 0)));
        assert_eq!(PdfVersion::parse("1"), None);
        assert_eq!(PdfVersion::parse("1.10"), None);
        assert_eq!(PdfVersion::parse("a.b"), None);
        assert_eq!(PdfVersion::new(1, 7).to_string(), "1.7");
        assert!(PdfVersion::new(1, 4) < PdfVersion::new(1, 7));
    }

    #[test]
    fn output_intent_subtype_ignores_case_and_slash() {
        let intent = OutputIntent::new("/gts_pdfx", true);
        assert!(intent.is_subtype("GTS_PDFX"));
        assert!(intent.is_subtype("/GTS_PDFX"));
        assert!(!intent.is_subtype("GTS_PDFA1"));
    }

    #[test]
    fn input_helpers_reflect_supplied_facts() {
        let mut input = PreflightInput {
            version: PdfVersion::new(1, 7),
            page_count: 2,
            ..Default::default()
        };
        assert!(!input.has_complete_page_data());
        input.pages.push(PageInfo {
            number: 1,
            boxes: PageBoxes::all_present(),
            has_dpart: false,
        });
        input.pages.push(PageInfo {
            number: 2,
            boxes: PageBoxes::all_present(),
            has_dpart: false,
        });
        assert!(input.has_complete_page_data());
        input
            .catalog
            .output_intents
            .push(OutputIntent::new("GTS_PDFA1", true));
        assert!(input.output_intent_of("GTS_PDFA1").is_some());
        assert!(input.output_intent_of("GTS_PDFX").is_none());
    }

    #[test]
    fn annotation_subtype_ignores_case_and_slash() {
        let annotation = AnnotationInfo::new("/popup", false);
        assert!(annotation.is_subtype("Popup"));
        assert!(annotation.is_subtype("/POPUP"));
        assert!(!annotation.is_subtype("Text"));
        assert!(!annotation.has_appearance_stream);
    }

    #[test]
    fn display_labels_are_stable() {
        assert_eq!(FontType::MmType1.to_string(), "MMType1");
        assert_eq!(ColourSpace::DeviceRgb.to_string(), "DeviceRGB");
        assert_eq!(ImageFilter::Jbig2Decode.to_string(), "JBIG2Decode");
    }
}
