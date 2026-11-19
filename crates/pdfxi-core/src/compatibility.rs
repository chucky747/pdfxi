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
}
