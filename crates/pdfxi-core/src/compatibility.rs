//! PDF version detection and compatibility reporting.
//!
//! A PDF declares its version in the `%PDF-M.m` header line, but real-world
//! files frequently deviate from the ideal: the header is preceded by junk
//! bytes, the version token is padded with white space, the binary comment line
//! is absent, or the document catalog carries a `/Version` name that overrides
//! the header. This module centralizes tolerant detection of those cases and
//! reports them as typed warnings instead of silently guessing.
//!
//! The entry point is [`analyze_compatibility`], which works on raw bytes and
//! therefore also reports the version of a document whose structural layer is
//! damaged. [`Document::compatibility_report`](crate::Document::compatibility_report)
//! is the convenience wrapper for an already-opened document.

use crate::object::Object;
use crate::parser;

/// Number of leading bytes searched for a `%PDF-` header.
///
/// ISO 32000 tolerates a small amount of leading garbage; 1024 bytes is the
/// conventional limit used by other readers and is enough to accept files
/// produced by e-mail and print pipelines without scanning the whole input.
pub const HEADER_SCAN_LIMIT: usize = 1024;

/// A parsed PDF version, expressed as a major and minor number.
///
/// For example `%PDF-1.7` parses to `PdfVersion { major: 1, minor: 7 }` and
/// `%PDF-2.0` to `PdfVersion { major: 2, minor: 0 }`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PdfVersion {
    /// The major version (for example `1` or `2`).
    pub major: u8,
    /// The minor version (for example `4` or `7`).
    pub minor: u8,
}

impl PdfVersion {
    /// Construct a version from its major and minor components.
    #[must_use]
    pub const fn new(major: u8, minor: u8) -> Self {
        Self { major, minor }
    }

    /// PDF 1.0, the earliest version this crate recognizes.
    pub const V1_0: Self = Self { major: 1, minor: 0 };
    /// PDF 1.7, the most widely deployed 1.x version.
    pub const V1_7: Self = Self { major: 1, minor: 7 };
    /// PDF 2.0, the current ISO standard version.
    pub const V2_0: Self = Self { major: 2, minor: 0 };
    /// The version assumed when a file has no readable header version.
    ///
    /// PDF 1.4 is the conservative default used by other readers and covers
    /// the classic xref table feature set this crate writes.
    pub const DEFAULT: Self = Self { major: 1, minor: 4 };

    /// Parse a version from a name such as `1.7`, `%PDF-2.0` is *not* accepted;
    /// pass the characters after `%PDF-` or a catalog `/Version` name.
    ///
    /// Surrounding white space is ignored. Both fields must be decimal digits
    /// and fit in a `u8`; trailing junk is rejected rather than truncated.
    pub fn parse(text: &str) -> Option<Self> {
        let text = text.trim();
        let (major, minor) = text.split_once('.')?;
        let major = major.trim();
        let minor = minor.trim();
        if major.is_empty() || minor.is_empty() {
            return None;
        }
        if !major.bytes().all(|byte| byte.is_ascii_digit())
            || !minor.bytes().all(|byte| byte.is_ascii_digit())
        {
            return None;
        }
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }

    /// Return `true` when this version lies in the supported PDF 1.0-2.0 range.
    ///
    /// That is major 1 with a minor up to and including 7, or exactly 2.0.
    pub fn is_supported(self) -> bool {
        (self.major == 1 && self.minor <= 7) || (self.major == 2 && self.minor == 0)
    }

    /// Return the `(major, minor)` pair as a tuple.
    pub fn as_tuple(self) -> (u8, u8) {
        (self.major, self.minor)
    }
}

impl std::fmt::Display for PdfVersion {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// Where the effective version of a document came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VersionSource {
    /// The version was taken from the `%PDF-` header line.
    Header,
    /// A catalog `/Version` name overrode the header version.
    Catalog,
}

/// A non-fatal version or header irregularity detected in a PDF.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum CompatibilityWarning {
    /// No `%PDF-` marker was found; an assumed version is used instead.
    #[error("no %PDF- header found; assumed version {assumed}")]
    HeaderMissing {
        /// The version assumed in place of a readable header.
        assumed: PdfVersion,
    },
    /// The header was found, but not at byte zero.
    #[error("%PDF- header begins at byte {offset} instead of 0")]
    HeaderNotAtStart {
        /// Byte offset of the `%PDF-` marker.
        offset: usize,
    },
    /// The characters following `%PDF-` were not a readable version.
    #[error("could not read a version from {raw:?}; assumed {assumed}")]
    HeaderVersionUnreadable {
        /// The raw characters that followed the header marker.
        raw: String,
        /// The version assumed in place of the unreadable one.
        assumed: PdfVersion,
    },
    /// A catalog `/Version` entry overrides the declared header version.
    #[error("catalog /Version {catalog} overrides header version {header}")]
    CatalogVersionOverride {
        /// The version declared in the header.
        header: PdfVersion,
        /// The overriding catalog version.
        catalog: PdfVersion,
    },
    /// The effective version is outside the supported 1.0-2.0 range.
    #[error("PDF version {version} is outside the supported 1.0-2.0 range")]
    UnsupportedVersion {
        /// The effective version that was rejected.
        version: PdfVersion,
    },
}

/// A typed report describing the version compatibility of a PDF.
///
/// The report is deterministic: warnings are emitted in a fixed order
/// (header issues first, then a catalog override, then an unsupported-version
/// note) regardless of how the underlying bytes are laid out.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityReport {
    header_version: PdfVersion,
    catalog_version: Option<PdfVersion>,
    effective_version: PdfVersion,
    source: VersionSource,
    header_offset: Option<usize>,
    warnings: Vec<CompatibilityWarning>,
}

impl CompatibilityReport {
    /// The version declared by the header, or [`PdfVersion::DEFAULT`] when the
    /// header is missing or unreadable.
    pub fn header_version(&self) -> PdfVersion {
        self.header_version
    }

    /// The catalog `/Version` override, when a readable one was present.
    pub fn catalog_version(&self) -> Option<PdfVersion> {
        self.catalog_version
    }

    /// The version that applies to the document: the catalog override when
    /// present, otherwise the header version.
    pub fn effective_version(&self) -> PdfVersion {
        self.effective_version
    }

    /// Where [`Self::effective_version`] came from.
    pub fn source(&self) -> VersionSource {
        self.source
    }

    /// Byte offset of the `%PDF-` marker, or `None` when no header was found.
    pub fn header_offset(&self) -> Option<usize> {
        self.header_offset
    }

    /// All typed warnings, in deterministic order.
    pub fn warnings(&self) -> &[CompatibilityWarning] {
        &self.warnings
    }

    /// Return `true` when the effective version is within the supported range.
    pub fn is_supported(&self) -> bool {
        self.effective_version.is_supported()
    }
}

/// The result of tolerant header parsing (internal to this module).
#[derive(Debug, Clone)]
struct HeaderInfo {
    version: PdfVersion,
    offset: Option<usize>,
    warnings: Vec<CompatibilityWarning>,
}

/// Locate and parse the `%PDF-` header tolerantly.
fn parse_header(data: &[u8]) -> HeaderInfo {
    let limit = data.len().min(HEADER_SCAN_LIMIT);
    let found = data
        .get(..limit)
        .and_then(|prefix| prefix.windows(5).position(|window| window == b"%PDF-"));

    let Some(offset) = found else {
        return HeaderInfo {
            version: PdfVersion::DEFAULT,
            offset: None,
            warnings: vec![CompatibilityWarning::HeaderMissing {
                assumed: PdfVersion::DEFAULT,
            }],
        };
    };

    let mut warnings = Vec::new();
    if offset != 0 {
        warnings.push(CompatibilityWarning::HeaderNotAtStart { offset });
    }

    let after = offset + 5;
    let mut cursor = after;
    while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
        cursor += 1;
    }
    let token_start = cursor;
    while data
        .get(cursor)
        .is_some_and(|byte| byte.is_ascii_digit() || *byte == b'.')
    {
        cursor += 1;
    }
    let raw = String::from_utf8_lossy(&data[token_start..cursor]).into_owned();
    let version = PdfVersion::parse(&raw).unwrap_or(PdfVersion::DEFAULT);
    if version == PdfVersion::DEFAULT && PdfVersion::parse(&raw).is_none() {
        warnings.push(CompatibilityWarning::HeaderVersionUnreadable {
            raw,
            assumed: PdfVersion::DEFAULT,
        });
    }

    HeaderInfo {
        version,
        offset: Some(offset),
        warnings,
    }
}

/// Return `true` when a byte terminates a PDF name or regular token.
fn is_name_terminator(byte: u8) -> bool {
    byte.is_ascii_whitespace() || matches!(byte, b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')')
}

/// Read a catalog `/Version` name from a raw byte scan.
///
/// Used as a fallback when the structural layer is damaged and the catalog
/// cannot be resolved through the xref table. Only `/Catalog` dictionaries are
/// considered, and the `/Version` name must appear within a bounded window.
fn scan_catalog_version(data: &[u8]) -> Option<PdfVersion> {
    const WINDOW: usize = 8192;
    let mut from = 0usize;
    while let Some(relative) = data
        .get(from..)
        .and_then(|tail| tail.windows(8).position(|window| window == b"/Catalog"))
    {
        let pos = from + relative;
        // Confirm the `/Catalog` name belongs to a `/Type` entry.
        let before = data.get(..pos).unwrap_or(&[]);
        let mut trimmed_end = before.len();
        while trimmed_end > 0 && before[trimmed_end - 1].is_ascii_whitespace() {
            trimmed_end -= 1;
        }
        if !before
            .get(..trimmed_end)
            .is_some_and(|prefix| prefix.ends_with(b"/Type"))
        {
            from = pos + 8;
            continue;
        }
        let window_end = pos.saturating_add(WINDOW).min(data.len());
        if let Some(version_tail) = data
            .get(pos..window_end)
            .and_then(|window| window.windows(8).position(|part| part == b"/Version"))
        {
            let mut cursor = pos + version_tail + 8;
            while data.get(cursor).is_some_and(u8::is_ascii_whitespace) {
                cursor += 1;
            }
            if data.get(cursor) == Some(&b'/') {
                let name_start = cursor + 1;
                let mut name_end = name_start;
                while data
                    .get(name_end)
                    .is_some_and(|byte| !is_name_terminator(*byte))
                {
                    name_end += 1;
                }
                let name = String::from_utf8_lossy(&data[name_start..name_end]);
                if let Some(version) = PdfVersion::parse(&name) {
                    return Some(version);
                }
            }
        }
        from = pos + 8;
    }
    None
}

/// Resolve the catalog `/Version` override, preferring the structural parser.
fn catalog_version(data: &[u8]) -> Option<PdfVersion> {
    if let Ok(structure) = parser::parse_document(data)
        && let Ok(object) = parser::load_object(data, &structure.xref, structure.root_id)
        && let Some(dictionary) = object.as_dictionary()
        && let Some(Object::Name(name)) = dictionary.get("Version")
        && let Some(version) = PdfVersion::parse(name)
    {
        return Some(version);
    }
    scan_catalog_version(data)
}

/// Analyze the version compatibility of a raw PDF byte stream.
///
/// Header detection tolerates leading garbage and white-space padding, and the
/// catalog `/Version` override is honored. The returned report is deterministic
/// and can be produced even when the document body is damaged.
pub fn analyze_compatibility(data: &[u8]) -> CompatibilityReport {
    let header = parse_header(data);
    let catalog = catalog_version(data);
    let (effective_version, source) = match catalog {
        Some(version) => (version, VersionSource::Catalog),
        None => (header.version, VersionSource::Header),
    };

    let mut warnings = header.warnings;
    if let Some(catalog_version) = catalog
        && catalog_version != header.version
    {
        warnings.push(CompatibilityWarning::CatalogVersionOverride {
            header: header.version,
            catalog: catalog_version,
        });
    }
    if !effective_version.is_supported() {
        warnings.push(CompatibilityWarning::UnsupportedVersion {
            version: effective_version,
        });
    }

    CompatibilityReport {
        header_version: header.version,
        catalog_version: catalog,
        effective_version,
        source,
        header_offset: header.offset,
        warnings,
    }
}

/// A PDF construct whose availability depends on the document version.
///
/// Each variant names a feature that was introduced in a specific PDF version.
/// Feature gates are used to report when a document's *effective* version (see
/// [`CompatibilityReport::effective_version`]) is too old for a construct a
/// caller wants to write or has found in a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PdfFeature {
    /// Transparency, soft masks and extended graphics states (PDF 1.4+).
    Transparency,
    /// Tagged PDF structure trees (PDF 1.4+).
    TaggedPdf,
    /// Cross-reference streams (PDF 1.5+).
    XrefStreams,
    /// Object streams (`/ObjStm`, PDF 1.5+).
    ObjectStreams,
    /// Optional content groups and layers (PDF 1.5+).
    OptionalContent,
    /// Crypt filters and public-key security handlers (PDF 1.6+).
    CryptFilters,
    /// 3D annotations and 3D streams (PDF 1.6+).
    Annotations3D,
    /// Embedded files and file-specification dictionaries (PDF 1.7+).
    EmbeddedFiles,
    /// Rich-media and screen/sound annotations (PDF 1.7+).
    Multimedia,
    /// AES-256 encryption (`/V 5`, PDF 2.0+).
    Aes256Encryption,
    /// Associated files and `/AF` relationships (PDF 2.0+).
    AssociatedFiles,
    /// Encrypted metadata streams (PDF 2.0+).
    EncryptedMetadata,
}

impl PdfFeature {
    /// Every feature in a stable, declarative order.
    pub const ALL: &'static [Self] = &[
        Self::Transparency,
        Self::TaggedPdf,
        Self::XrefStreams,
        Self::ObjectStreams,
        Self::OptionalContent,
        Self::CryptFilters,
        Self::Annotations3D,
        Self::EmbeddedFiles,
        Self::Multimedia,
        Self::Aes256Encryption,
        Self::AssociatedFiles,
        Self::EncryptedMetadata,
    ];

    /// The first PDF version in which this feature is available.
    #[must_use]
    pub const fn min_version(self) -> PdfVersion {
        match self {
            Self::Transparency | Self::TaggedPdf => PdfVersion { major: 1, minor: 4 },
            Self::XrefStreams | Self::ObjectStreams | Self::OptionalContent => {
                PdfVersion { major: 1, minor: 5 }
            }
            Self::CryptFilters | Self::Annotations3D => PdfVersion { major: 1, minor: 6 },
            Self::EmbeddedFiles | Self::Multimedia => PdfVersion { major: 1, minor: 7 },
            Self::Aes256Encryption | Self::AssociatedFiles | Self::EncryptedMetadata => {
                PdfVersion::V2_0
            }
        }
    }

    /// A short human-readable label for diagnostics.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Transparency => "transparency",
            Self::TaggedPdf => "tagged PDF",
            Self::XrefStreams => "cross-reference streams",
            Self::ObjectStreams => "object streams",
            Self::OptionalContent => "optional content",
            Self::CryptFilters => "crypt filters",
            Self::Annotations3D => "3D annotations",
            Self::EmbeddedFiles => "embedded files",
            Self::Multimedia => "multimedia annotations",
            Self::Aes256Encryption => "AES-256 encryption",
            Self::AssociatedFiles => "associated files",
            Self::EncryptedMetadata => "encrypted metadata",
        }
    }
}

impl PdfVersion {
    /// Return `true` when this version provides `feature`.
    #[must_use]
    pub fn supports(self, feature: PdfFeature) -> bool {
        self >= feature.min_version()
    }
}

/// Result of checking one version-dependent feature gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FeatureSupport {
    /// The effective version is new enough for the feature.
    Supported,
    /// The effective version predates the feature.
    Requires {
        /// The first version that provides the feature.
        min: PdfVersion,
        /// The effective version of the document under test.
        effective: PdfVersion,
    },
}

impl FeatureSupport {
    /// Return `true` when the gate is satisfied.
    #[must_use]
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

/// Evaluate a single feature gate against a compatibility report.
#[must_use]
pub fn feature_gate(report: &CompatibilityReport, feature: PdfFeature) -> FeatureSupport {
    let effective = report.effective_version();
    if effective.supports(feature) {
        FeatureSupport::Supported
    } else {
        FeatureSupport::Requires {
            min: feature.min_version(),
            effective,
        }
    }
}

impl CompatibilityReport {
    /// Return `true` when the effective version provides `feature`.
    #[must_use]
    pub fn supports(&self, feature: PdfFeature) -> bool {
        self.effective_version().supports(feature)
    }

    /// Return every feature whose minimum version exceeds the effective one.
    ///
    /// The result follows [`PdfFeature::ALL`] and is therefore deterministic.
    #[must_use]
    pub fn unsupported_features(&self) -> Vec<PdfFeature> {
        PdfFeature::ALL
            .iter()
            .copied()
            .filter(|feature| !self.supports(*feature))
            .collect()
    }

    /// Evaluate every version-dependent feature gate into one deterministic report.
    ///
    /// This is the aggregate companion to [`crate::feature_gate`]: it records a
    /// result for every member of [`PdfFeature::ALL`] in a stable order, so the
    /// report (including its [`FeatureGateReport::summary`]) is byte-identical
    /// for the same effective version.
    #[must_use]
    pub fn feature_gate_report(&self) -> FeatureGateReport {
        FeatureGateReport::new(self)
    }
}

/// One feature-gate result inside a [`FeatureGateReport`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FeatureGateEntry {
    feature: PdfFeature,
    support: FeatureSupport,
}

impl FeatureGateEntry {
    /// The feature whose gate was evaluated.
    #[must_use]
    pub const fn feature(&self) -> PdfFeature {
        self.feature
    }

    /// Whether the effective version provides the feature.
    #[must_use]
    pub const fn support(&self) -> FeatureSupport {
        self.support
    }
}

/// A deterministic, ordered report of every version-dependent feature gate.
///
/// Entries follow [`PdfFeature::ALL`], so two reports for the same effective
/// version are always equal, and their [`Self::summary`] strings are identical.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FeatureGateReport {
    effective: PdfVersion,
    entries: Vec<FeatureGateEntry>,
}

impl FeatureGateReport {
    /// Evaluate all feature gates against a compatibility report.
    #[must_use]
    pub fn new(report: &CompatibilityReport) -> Self {
        let effective = report.effective_version();
        let entries = PdfFeature::ALL
            .iter()
            .copied()
            .map(|feature| FeatureGateEntry {
                feature,
                support: feature_gate(report, feature),
            })
            .collect();
        Self { effective, entries }
    }

    /// The effective version the gates were evaluated against.
    #[must_use]
    pub const fn effective_version(&self) -> PdfVersion {
        self.effective
    }

    /// All gate results, in [`PdfFeature::ALL`] order.
    #[must_use]
    pub fn entries(&self) -> &[FeatureGateEntry] {
        &self.entries
    }

    /// Features whose minimum version exceeds the effective one, in order.
    #[must_use]
    pub fn unsupported(&self) -> Vec<PdfFeature> {
        self.entries
            .iter()
            .filter(|entry| !entry.support.is_supported())
            .map(|entry| entry.feature)
            .collect()
    }

    /// Return `true` when every feature gate is satisfied.
    #[must_use]
    pub fn all_supported(&self) -> bool {
        self.entries
            .iter()
            .all(|entry| entry.support.is_supported())
    }

    /// A stable, human-readable summary of all gates.
    ///
    /// The form is `effective=<M.m> supported=<n>/<total> unsupported=<list>`;
    /// `<list>` is `-` when nothing is missing, otherwise comma-separated
    /// `<label>(need <version>)` items in [`PdfFeature::ALL`] order.
    #[must_use]
    pub fn summary(&self) -> String {
        let missing: Vec<&FeatureGateEntry> = self
            .entries
            .iter()
            .filter(|entry| !entry.support.is_supported())
            .collect();
        let supported = self.entries.len() - missing.len();
        let mut out = format!(
            "effective={} supported={}/{} unsupported=",
            self.effective,
            supported,
            self.entries.len()
        );
        if missing.is_empty() {
            out.push('-');
        } else {
            let parts: Vec<String> = missing
                .iter()
                .map(|entry| match entry.support {
                    FeatureSupport::Requires { min, .. } => {
                        format!("{}(need {})", entry.feature.label(), min)
                    }
                    FeatureSupport::Supported => String::new(),
                })
                .filter(|part| !part.is_empty())
                .collect();
            out.push_str(&parts.join(","));
        }
        out
    }
}

impl std::fmt::Display for FeatureGateReport {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.summary())
    }
}

/// Return `true` when `data` contains `token` as a contiguous byte sequence.
fn contains_token(data: &[u8], token: &[u8]) -> bool {
    !token.is_empty() && token.len() <= data.len() && data.windows(token.len()).any(|w| w == token)
}

/// Heuristically detect version-dependent constructs from raw PDF bytes.
///
/// This is a *token-presence scan*, not a structural parse: it reports every
/// feature whose characteristic PDF name appears anywhere in `data`, in
/// [`PdfFeature::ALL`] order. It is a fast compatibility signal for damaged or
/// partially available files; a caller should confirm a finding by resolving
/// the corresponding object through the structural layer. Stream contents are
/// not excluded, so a token embedded in a stream is a documented false-positive
/// possibility rather than a correctness bug.
#[must_use]
pub fn detect_features(data: &[u8]) -> Vec<PdfFeature> {
    const MARKERS: &[(PdfFeature, &[&[u8]])] = &[
        (PdfFeature::Transparency, &[b"/SMask", b"/ExtGState"]),
        (PdfFeature::TaggedPdf, &[b"/StructTreeRoot"]),
        (PdfFeature::XrefStreams, &[b"/Type /XRef", b"/XRefStm"]),
        (PdfFeature::ObjectStreams, &[b"/ObjStm"]),
        (PdfFeature::OptionalContent, &[b"/OCProperties"]),
        (PdfFeature::CryptFilters, &[b"/CF <<", b"/CF<<"]),
        (PdfFeature::Annotations3D, &[b"/3DD", b"/3D"]),
        (PdfFeature::EmbeddedFiles, &[b"/EmbeddedFiles"]),
        (
            PdfFeature::Multimedia,
            &[b"/RichMedia", b"/Movie", b"/Screen"],
        ),
        (PdfFeature::Aes256Encryption, &[b"/V 5"]),
        (
            PdfFeature::AssociatedFiles,
            &[b"/AF ", b"/AF\n", b"/AF\t", b"/AF[", b"/AF<<", b"/AF <<"],
        ),
        (PdfFeature::EncryptedMetadata, &[b"/EncryptMetadata"]),
    ];
    PdfFeature::ALL
        .iter()
        .copied()
        .filter(|feature| {
            MARKERS.iter().any(|(candidate, tokens)| {
                *candidate == *feature && tokens.iter().any(|token| contains_token(data, token))
            })
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn minimal_pdf_with(version_line: &[u8], catalog_extra: &[u8]) -> Vec<u8> {
        let mut out = version_line.to_vec();
        out.extend_from_slice(b"\n");
        let objects: Vec<Vec<u8>> = vec![
            {
                let mut o = b"<< /Type /Catalog /Pages 2 0 R".to_vec();
                o.extend_from_slice(catalog_extra);
                o.extend_from_slice(b" >>");
                o
            },
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 10 10] >>".to_vec(),
        ];
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(body);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn version_parse_accepts_valid_and_rejects_junk() {
        assert_eq!(PdfVersion::parse("1.7"), Some(PdfVersion::V1_7));
        assert_eq!(PdfVersion::parse(" 2.0 "), Some(PdfVersion::V2_0));
        assert_eq!(PdfVersion::parse("1.4"), Some(PdfVersion::DEFAULT));
        assert_eq!(PdfVersion::parse("1.7junk"), None);
        assert_eq!(PdfVersion::parse("abc"), None);
        assert_eq!(PdfVersion::parse("1"), None);
    }

    #[test]
    fn version_support_range_is_exact() {
        assert!(PdfVersion::V1_0.is_supported());
        assert!(PdfVersion::V1_7.is_supported());
        assert!(PdfVersion::V2_0.is_supported());
        assert!(!PdfVersion { major: 2, minor: 1 }.is_supported());
        assert!(!PdfVersion { major: 3, minor: 0 }.is_supported());
    }

    #[test]
    fn header_variants_are_tolerated() {
        let standard = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.7", b""));
        assert_eq!(standard.header_version(), PdfVersion::V1_7);
        assert_eq!(standard.header_offset(), Some(0));
        assert!(standard.warnings().is_empty());

        let padded = analyze_compatibility(&minimal_pdf_with(b"%PDF-  1.7  ", b""));
        assert_eq!(padded.header_version(), PdfVersion::V1_7);
        assert_eq!(padded.effective_version(), PdfVersion::V1_7);

        let leading = {
            let mut pdf = b"garbage-bytes\n".to_vec();
            pdf.extend_from_slice(&minimal_pdf_with(b"%PDF-1.4", b""));
            analyze_compatibility(&pdf)
        };
        assert_eq!(leading.header_offset(), Some(14));
        assert!(
            leading
                .warnings()
                .iter()
                .any(|w| matches!(w, CompatibilityWarning::HeaderNotAtStart { offset: 14 }))
        );
    }

    #[test]
    fn binary_marker_absence_and_padded_versions_are_fine() {
        // A header with no following binary comment line and extra whitespace
        // after the version token must still report V1.7 with no issue.
        let pdf = minimal_pdf_with(b"%PDF-1.7   ", b"");
        let report = analyze_compatibility(&pdf);
        assert_eq!(report.effective_version(), PdfVersion::V1_7);
        assert!(report.warnings().is_empty());
    }

    #[test]
    fn missing_header_reports_assumed_version() {
        let report = analyze_compatibility(b"no header here");
        assert_eq!(report.header_version(), PdfVersion::DEFAULT);
        assert_eq!(report.header_offset(), None);
        assert!(
            report
                .warnings()
                .iter()
                .any(|w| matches!(w, CompatibilityWarning::HeaderMissing { .. }))
        );
    }

    #[test]
    fn catalog_version_override_is_reported() {
        let pdf = minimal_pdf_with(b"%PDF-1.4", b" /Version /2.0");
        let report = analyze_compatibility(&pdf);
        assert_eq!(report.header_version(), PdfVersion::DEFAULT);
        assert_eq!(report.catalog_version(), Some(PdfVersion::V2_0));
        assert_eq!(report.effective_version(), PdfVersion::V2_0);
        assert_eq!(report.source(), VersionSource::Catalog);
        assert!(report.is_supported());
        assert!(
            report
                .warnings()
                .iter()
                .any(|w| matches!(w, CompatibilityWarning::CatalogVersionOverride { .. }))
        );
    }

    #[test]
    fn catalog_version_override_works_on_broken_structure() {
        // The xref is unusable, so the structured lookup fails; the raw scan
        // must still recover the `/Version` name.
        let mut pdf = minimal_pdf_with(b"%PDF-1.4", b" /Version /1.7");
        let marker = b"startxref\n";
        if let Some(pos) = pdf.windows(marker.len()).position(|w| w == marker) {
            let value_start = pos + marker.len();
            if let Some(tail) = pdf.get_mut(value_start..) {
                for byte in tail {
                    if byte.is_ascii_digit() {
                        *byte = b'x';
                    }
                    if *byte == b'\n' {
                        break;
                    }
                }
            }
        }
        let report = analyze_compatibility(&pdf);
        assert_eq!(report.catalog_version(), Some(PdfVersion::V1_7));
        assert_eq!(report.effective_version(), PdfVersion::V1_7);
    }

    #[test]
    fn unsupported_effective_version_is_flagged() {
        let pdf = minimal_pdf_with(b"%PDF-3.0", b"");
        let report = analyze_compatibility(&pdf);
        assert_eq!(
            report.effective_version(),
            PdfVersion { major: 3, minor: 0 }
        );
        assert!(!report.is_supported());
        assert!(
            report
                .warnings()
                .iter()
                .any(|w| matches!(w, CompatibilityWarning::UnsupportedVersion { .. }))
        );
    }

    #[test]
    fn warning_order_is_deterministic() {
        let mut pdf = b"junk\n".to_vec();
        pdf.extend_from_slice(&minimal_pdf_with(b"%PDF-1.4", b" /Version /3.0"));
        let a = analyze_compatibility(&pdf);
        let b = analyze_compatibility(&pdf);
        assert_eq!(a, b);
        assert_eq!(a.warnings().len(), 3);
        assert!(matches!(
            a.warnings()[0],
            CompatibilityWarning::HeaderNotAtStart { .. }
        ));
        assert!(matches!(
            a.warnings()[1],
            CompatibilityWarning::CatalogVersionOverride { .. }
        ));
        assert!(matches!(
            a.warnings()[2],
            CompatibilityWarning::UnsupportedVersion { .. }
        ));
    }

    // ---- T-0118 version-dependent feature gates ----

    #[test]
    fn feature_min_versions_match_the_pdf_specification() {
        assert_eq!(
            PdfFeature::Transparency.min_version(),
            PdfVersion::new(1, 4)
        );
        assert_eq!(PdfFeature::TaggedPdf.min_version(), PdfVersion::new(1, 4));
        assert_eq!(PdfFeature::XrefStreams.min_version(), PdfVersion::new(1, 5));
        assert_eq!(
            PdfFeature::ObjectStreams.min_version(),
            PdfVersion::new(1, 5)
        );
        assert_eq!(
            PdfFeature::OptionalContent.min_version(),
            PdfVersion::new(1, 5)
        );
        assert_eq!(
            PdfFeature::CryptFilters.min_version(),
            PdfVersion::new(1, 6)
        );
        assert_eq!(
            PdfFeature::Annotations3D.min_version(),
            PdfVersion::new(1, 6)
        );
        assert_eq!(
            PdfFeature::EmbeddedFiles.min_version(),
            PdfVersion::new(1, 7)
        );
        assert_eq!(PdfFeature::Multimedia.min_version(), PdfVersion::new(1, 7));
        assert_eq!(PdfFeature::Aes256Encryption.min_version(), PdfVersion::V2_0);
        assert_eq!(PdfFeature::AssociatedFiles.min_version(), PdfVersion::V2_0);
        assert_eq!(
            PdfFeature::EncryptedMetadata.min_version(),
            PdfVersion::V2_0
        );
    }

    #[test]
    fn feature_support_boundaries_are_exact() {
        let v14 = PdfVersion::new(1, 4);
        assert!(v14.supports(PdfFeature::Transparency));
        assert!(!v14.supports(PdfFeature::XrefStreams));
        let v15 = PdfVersion::new(1, 5);
        assert!(v15.supports(PdfFeature::XrefStreams));
        assert!(!v15.supports(PdfFeature::EmbeddedFiles));
        let v20 = PdfVersion::V2_0;
        assert!(v20.supports(PdfFeature::EncryptedMetadata));
        assert!(PdfVersion::new(2, 1).supports(PdfFeature::EncryptedMetadata));
        assert!(!PdfVersion::V1_7.supports(PdfFeature::Aes256Encryption));
    }

    #[test]
    fn feature_gate_reports_required_version() {
        let report = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.4", b""));
        assert_eq!(
            feature_gate(&report, PdfFeature::Transparency),
            FeatureSupport::Supported
        );
        assert_eq!(
            feature_gate(&report, PdfFeature::XrefStreams),
            FeatureSupport::Requires {
                min: PdfVersion::new(1, 5),
                effective: PdfVersion::new(1, 4),
            }
        );
        assert!(!feature_gate(&report, PdfFeature::XrefStreams).is_supported());
        assert!(feature_gate(&report, PdfFeature::Transparency).is_supported());
    }

    #[test]
    fn catalog_override_changes_feature_gates() {
        let report = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.4", b" /Version /2.0"));
        assert_eq!(report.effective_version(), PdfVersion::V2_0);
        assert!(report.supports(PdfFeature::Aes256Encryption));
        assert!(report.supports(PdfFeature::AssociatedFiles));
        assert!(report.unsupported_features().is_empty());
    }

    #[test]
    fn unsupported_features_are_ordered_and_deterministic() {
        let report = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.4", b""));
        let features = report.unsupported_features();
        assert_eq!(features, report.unsupported_features());
        let expected = [
            PdfFeature::XrefStreams,
            PdfFeature::ObjectStreams,
            PdfFeature::OptionalContent,
            PdfFeature::CryptFilters,
            PdfFeature::Annotations3D,
            PdfFeature::EmbeddedFiles,
            PdfFeature::Multimedia,
            PdfFeature::Aes256Encryption,
            PdfFeature::AssociatedFiles,
            PdfFeature::EncryptedMetadata,
        ];
        assert_eq!(features, expected);
        assert!(features.iter().all(|feature| !report.supports(*feature)));
    }

    #[test]
    fn every_feature_has_a_nonempty_label_and_all_is_unique() {
        for feature in PdfFeature::ALL {
            assert!(!feature.label().is_empty());
            assert!(feature.min_version().is_supported());
        }
        let mut seen = std::collections::BTreeSet::new();
        for feature in PdfFeature::ALL {
            assert!(seen.insert(*feature), "duplicate feature in ALL");
        }
        assert_eq!(seen.len(), PdfFeature::ALL.len());
    }

    // ---- T-0172 deterministic feature-gate report and raw-byte detection ----

    #[test]
    fn feature_gate_report_is_ordered_and_deterministic() {
        let report = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.4", b""));
        let gates = report.feature_gate_report();
        assert_eq!(gates.effective_version(), PdfVersion::new(1, 4));
        assert_eq!(gates.entries().len(), PdfFeature::ALL.len());
        let features: Vec<PdfFeature> = gates
            .entries()
            .iter()
            .map(FeatureGateEntry::feature)
            .collect();
        assert_eq!(features, PdfFeature::ALL);
        assert_eq!(gates, report.feature_gate_report());
        assert_eq!(gates.summary(), gates.to_string());
        assert_eq!(gates.unsupported(), report.unsupported_features());
        assert!(!gates.all_supported());
    }

    #[test]
    fn feature_gate_report_summary_is_stable_and_reflects_version() {
        let pdf = minimal_pdf_with(b"%PDF-1.4", b"");
        let gates = analyze_compatibility(&pdf).feature_gate_report();
        let summary = gates.summary();
        assert!(summary.starts_with("effective=1.4 supported=2/12 unsupported="));
        assert!(summary.contains("cross-reference streams(need 1.5)"));
        assert!(summary.contains("AES-256 encryption(need 2.0)"));
        // Rebuilding the report from the same bytes yields identical text.
        assert_eq!(
            summary,
            analyze_compatibility(&pdf).feature_gate_report().summary()
        );
        // A newer version changes the summary deterministically.
        let v17 = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.7", b""));
        assert!(
            v17.feature_gate_report()
                .summary()
                .starts_with("effective=1.7 supported=9/12")
        );
    }

    #[test]
    fn feature_gate_report_reflects_catalog_override() {
        let report = analyze_compatibility(&minimal_pdf_with(b"%PDF-1.4", b" /Version /2.0"));
        let gates = report.feature_gate_report();
        assert_eq!(gates.effective_version(), PdfVersion::V2_0);
        assert!(gates.all_supported());
        assert!(gates.unsupported().is_empty());
        assert_eq!(
            gates.summary(),
            "effective=2.0 supported=12/12 unsupported=-"
        );
    }

    #[test]
    fn detect_features_finds_version_dependent_tokens() {
        let bytes = b"%PDF-1.7 /Type /XRef /ObjStm /StructTreeRoot /OCProperties \
                      /SMask /EmbeddedFiles /RichMedia /EncryptMetadata /V 5 /AF << >>";
        let detected = detect_features(bytes);
        let expected = [
            PdfFeature::Transparency,
            PdfFeature::TaggedPdf,
            PdfFeature::XrefStreams,
            PdfFeature::ObjectStreams,
            PdfFeature::OptionalContent,
            PdfFeature::EmbeddedFiles,
            PdfFeature::Multimedia,
            PdfFeature::Aes256Encryption,
            PdfFeature::AssociatedFiles,
            PdfFeature::EncryptedMetadata,
        ];
        assert_eq!(detected, expected);
        assert_eq!(detected, detect_features(bytes));
    }

    #[test]
    fn detect_features_is_empty_for_plain_14_pdf() {
        let pdf = minimal_pdf_with(b"%PDF-1.4", b"");
        assert!(detect_features(&pdf).is_empty());
        assert!(detect_features(b"").is_empty());
    }
}
