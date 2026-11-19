//! Profile-targeted PDF export and normalization (declared subset only).
//!
//! This module takes raw PDF bytes plus a conformance request and produces a
//! deterministic, normalized byte stream for one of four preservation /
//! production targets: PDF/A-1b, PDF/A-2b, PDF/X-4 and PDF/E-1 (PDF/A-3b and
//! PDF/X-1a share the same rule tables and are accepted too). It is a
//! *normalizer*, not a certifier: no certification is claimed, and the returned
//! [`ConformanceExportReport`] states exactly which constraints were enforced
//! and which are reported only.
//!
//! # Enforced subset
//!
//! - The `%PDF-x.y` header is raised to the target's minimum version, never
//!   lowered, when version normalization is enabled.
//! - A catalog `/Metadata` indirection is added, when absent, for profiles that
//!   require XMP. The stream is a labelled placeholder.
//! - A catalog `/OutputIntents` entry with the target's `/S` subtype and a
//!   placeholder `/DestOutputProfile` is added, when absent, for profiles that
//!   use output intents.
//! - Existing metadata, output intents, page content, objects and trailer keys
//!   are preserved; the xref table and `/Size` are regenerated deterministically.
//!
//! # Reported-only constraints
//!
//! ICC validity, real XMP content, font embedding, transparency, colour spaces,
//! image filters, annotations, forbidden entries, page boxes, encryption and
//! content-stream semantics are **not** enforced. A caller must still run a real
//! preflight checker (for example `pdfxi-standards`, or veraPDF in CI) and
//! supply real fonts, ICC profiles and metadata. Encryption, xref streams and
//! incremental-update chains are rejected with typed errors instead of being
//! silently mishandled.
//!
//! # Input subset
//!
//! Classic-xref, unencrypted PDFs whose header is at byte zero and whose
//! latest xref is a normal table are supported. Compressed xref streams,
//! `/Prev` chains and encrypted documents are rejected. The module has no
//! dependencies beyond `thiserror`, spawns no process, and never calls a
//! validator.

use std::fmt;

use thiserror::Error;

/// A conformance target this exporter can normalize for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ConformanceTarget {
    /// ISO 19005-1 Level B (PDF 1.4).
    PdfA1b,
    /// ISO 19005-2 Level B (PDF 1.7).
    PdfA2b,
    /// ISO 19005-3 Level B (PDF 1.7).
    PdfA3b,
    /// ISO 15930-1 Level A (PDF 1.3).
    PdfX1a,
    /// ISO 15930-7 (PDF 1.6).
    PdfX4,
    /// ISO 14739-1 (PDF 1.6).
    PdfE1,
}

impl ConformanceTarget {
    /// Every supported target, in a stable order.
    pub const ALL: &'static [Self] = &[
        Self::PdfA1b,
        Self::PdfA2b,
        Self::PdfA3b,
        Self::PdfX1a,
        Self::PdfX4,
        Self::PdfE1,
    ];

    /// Returns every supported target in a stable order.
    #[must_use]
    pub fn all() -> &'static [Self] {
        Self::ALL
    }

    /// Parses a canonical label, ignoring ASCII case.
    #[must_use]
    pub fn from_label(label: &str) -> Option<Self> {
        Self::ALL
            .iter()
            .copied()
            .find(|target| target.label().eq_ignore_ascii_case(label))
    }

    /// Returns the canonical report label.
    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::PdfA1b => "PDF/A-1b",
            Self::PdfA2b => "PDF/A-2b",
            Self::PdfA3b => "PDF/A-3b",
            Self::PdfX1a => "PDF/X-1a",
            Self::PdfX4 => "PDF/X-4",
            Self::PdfE1 => "PDF/E-1",
        }
    }

    /// Returns the minimum PDF version this target requires as `(major, minor)`.
    #[must_use]
    pub const fn minimum_version(self) -> (u8, u8) {
        match self {
            Self::PdfA1b => (1, 4),
            Self::PdfA2b | Self::PdfA3b => (1, 7),
            Self::PdfX1a => (1, 3),
            Self::PdfX4 | Self::PdfE1 => (1, 6),
        }
    }

    /// Returns the required output-intent subtype, if this target uses one.
    #[must_use]
    pub const fn output_intent_subtype(self) -> Option<&'static str> {
        match self {
            Self::PdfA1b | Self::PdfA2b | Self::PdfA3b => Some("GTS_PDFA1"),
            Self::PdfX1a | Self::PdfX4 => Some("GTS_PDFX"),
            Self::PdfE1 => None,
        }
    }

    /// Returns `true` when this target requires an XMP `/Metadata` stream.
    #[must_use]
    pub const fn requires_xmp(self) -> bool {
        matches!(
            self,
            Self::PdfA1b | Self::PdfA2b | Self::PdfA3b | Self::PdfX4
        )
    }
}

impl fmt::Display for ConformanceTarget {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.label())
    }
}

/// A typed reason a conformance export could not be produced.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum ConformanceExportError {
    /// The requested profile is not one this exporter supports.
    #[error("unsupported conformance profile '{label}'")]
    UnsupportedProfile {
        /// The label as supplied by the caller.
        label: String,
    },
    /// The request combines requirements that contradict the target profile.
    #[error("contradictory conformance request: {reason}")]
    ContradictoryRequest {
        /// A human-readable explanation of the conflict.
        reason: String,
    },
    /// The input is not a parseable classic-xref PDF.
    #[error("malformed PDF: {reason}")]
    MalformedPdf {
        /// A human-readable explanation of the parse failure.
        reason: String,
    },
    /// The input uses a PDF feature this subset deliberately does not handle.
    #[error("unsupported PDF structure: {reason}")]
    UnsupportedStructure {
        /// A human-readable explanation of the unsupported feature.
        reason: String,
    },
    /// The input is encrypted and cannot be normalized for the target.
    #[error("encrypted PDF cannot be exported to '{label}'")]
    Encrypted {
        /// The requested target label.
        label: String,
    },
}

/// A caller-supplied conformance export request.
///
/// The target label is resolved with [`ConformanceTarget::from_label`]. An
/// optional explicit output-intent subtype is validated against the target so
/// contradictory requests (for example `GTS_PDFX` for PDF/A-2b, or any subtype
/// for PDF/E-1) fail with [`ConformanceExportError::ContradictoryRequest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceRequest {
    target_label: String,
    output_intent_subtype: Option<String>,
}

impl ConformanceRequest {
    /// Creates a request for the profile named by `target_label`.
    #[must_use]
    pub fn new(target_label: impl Into<String>) -> Self {
        Self {
            target_label: target_label.into(),
            output_intent_subtype: None,
        }
    }

    /// Adds an explicit output-intent subtype to the request.
    #[must_use]
    pub fn with_output_intent_subtype(mut self, subtype: impl Into<String>) -> Self {
        self.output_intent_subtype = Some(subtype.into());
        self
    }

    /// Returns the requested target label.
    #[must_use]
    pub fn target_label(&self) -> &str {
        &self.target_label
    }

    /// Returns the explicit subtype, if one was supplied.
    #[must_use]
    pub fn output_intent_subtype(&self) -> Option<&str> {
        self.output_intent_subtype.as_deref()
    }

    /// Resolves and validates the request against the supported targets.
    pub fn resolve(&self) -> Result<ConformanceTarget, ConformanceExportError> {
        let target = ConformanceTarget::from_label(&self.target_label).ok_or_else(|| {
            ConformanceExportError::UnsupportedProfile {
                label: self.target_label.clone(),
            }
        })?;

        if let Some(supplied) = &self.output_intent_subtype {
            let normalized = supplied.strip_prefix('/').unwrap_or(supplied);
            match target.output_intent_subtype() {
                Some(expected) if normalized.eq_ignore_ascii_case(expected) => {}
                Some(expected) => {
                    return Err(ConformanceExportError::ContradictoryRequest {
                        reason: format!(
                            "{} requires output-intent subtype {expected}, not {supplied}",
                            target.label()
                        ),
                    });
                }
                None => {
                    return Err(ConformanceExportError::ContradictoryRequest {
                        reason: format!(
                            "{} does not use output intents, but subtype {supplied} was requested",
                            target.label()
                        ),
                    });
                }
            }
        }
        Ok(target)
    }
}

/// Opt-in switches for the mechanical normalizations this exporter performs.
///
/// [`Default`] enables all three families; [`Self::none`] disables them so a
/// caller can still benefit from deterministic xref/trailer rewriting without
/// changing metadata, intents, or version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NormalizationOptions {
    /// Raise the header version to the target minimum when it is too low.
    pub normalize_version: bool,
    /// Add a catalog XMP metadata placeholder when the target requires one.
    pub add_xmp_metadata: bool,
    /// Add a catalog output-intent placeholder when the target requires one.
    pub add_output_intent: bool,
}

impl NormalizationOptions {
    /// Returns options with every normalization enabled.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            normalize_version: true,
            add_xmp_metadata: true,
            add_output_intent: true,
        }
    }

    /// Returns options with every normalization disabled.
    #[must_use]
    pub const fn none() -> Self {
        Self {
            normalize_version: false,
            add_xmp_metadata: false,
            add_output_intent: false,
        }
    }
}

impl Default for NormalizationOptions {
    fn default() -> Self {
        Self::all()
    }
}

/// Severity of an export finding.
///
/// Export findings are advisory: a placeholder addition is recorded so a caller
/// can replace it, but it never by itself establishes conformance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ExportSeverity {
    /// Informational note about an applied normalization.
    Info,
    /// A reminder that a placeholder still needs real content.
    Warning,
}

impl fmt::Display for ExportSeverity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Info => f.write_str("info"),
            Self::Warning => f.write_str("warning"),
        }
    }
}

/// One stable, actionable export finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceFinding {
    severity: ExportSeverity,
    code: String,
    message: String,
    location: String,
}

impl ConformanceFinding {
    fn new(severity: ExportSeverity, code: &str, message: String, location: &str) -> Self {
        Self {
            severity,
            code: code.to_owned(),
            message,
            location: location.to_owned(),
        }
    }

    /// Returns the severity.
    #[must_use]
    pub const fn severity(&self) -> ExportSeverity {
        self.severity
    }

    /// Returns the stable machine code, for example `EXPORT.XMP_PLACEHOLDER_ADDED`.
    #[must_use]
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the actionable message.
    #[must_use]
    pub fn message(&self) -> &str {
        &self.message
    }

    /// Returns the object or header location the finding refers to.
    #[must_use]
    pub fn location(&self) -> &str {
        &self.location
    }
}

/// The normalized bytes and an honest report of what was done.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceExport {
    /// The normalized PDF byte stream.
    pub bytes: Vec<u8>,
    /// The report describing the enforced and reported-only constraints.
    pub report: ConformanceExportReport,
}

/// Constraints this exporter does not enforce, listed in stable order.
pub const REPORTED_ONLY_CONSTRAINTS: &[&str] = &[
    "icc_profile_validity",
    "xmp_metadata_content",
    "font_embedding",
    "transparency",
    "colour_space",
    "image_filters",
    "encryption",
    "annotations",
    "forbidden_entries",
    "page_boxes",
    "content_stream_semantics",
];

/// A short, stable statement of the exporter's scope.
pub const SCOPE_NOTE: &str = "Subset normalization only; no certification is claimed. \
XMP and ICC output-intent streams are placeholders that must be replaced with real \
metadata and a valid profile before conformance can be asserted. All other \
constraints are reported only and are not enforced by this exporter.";

/// A report of one normalization run.
///
/// The report never claims certification. [`Self::certification_claimed`] is
/// always `false`, and [`Self::scope_note`] explains that placeholders must be
/// replaced and that all other constraints are reported only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConformanceExportReport {
    target: ConformanceTarget,
    enforced: Vec<&'static str>,
    reported_only: Vec<&'static str>,
    applied: Vec<ConformanceFinding>,
    source_version: String,
    output_version: String,
}

impl ConformanceExportReport {
    fn new(target: ConformanceTarget, source_version: (u8, u8), output_version: (u8, u8)) -> Self {
        let mut enforced = vec!["minimum_version"];
        if target.requires_xmp() {
            enforced.push("xmp_metadata_presence");
        }
        if target.output_intent_subtype().is_some() {
            enforced.push("output_intent_presence");
            enforced.push("output_intent_subtype");
        }
        Self {
            target,
            enforced,
            reported_only: REPORTED_ONLY_CONSTRAINTS.to_vec(),
            applied: Vec::new(),
            source_version: format!("{}.{}", source_version.0, source_version.1),
            output_version: format!("{}.{}", output_version.0, output_version.1),
        }
    }

    /// Returns the target this export normalized for.
    #[must_use]
    pub const fn target(&self) -> ConformanceTarget {
        self.target
    }

    /// Returns the constraints that this exporter enforced in the output.
    #[must_use]
    pub fn enforced(&self) -> &[&'static str] {
        &self.enforced
    }

    /// Returns the constraints that are only reported, never enforced.
    #[must_use]
    pub fn reported_only(&self) -> &[&'static str] {
        &self.reported_only
    }

    /// Returns applied findings in canonical order (severity, then code).
    #[must_use]
    pub fn applied(&self) -> &[ConformanceFinding] {
        &self.applied
    }

    /// Returns the source header version as `major.minor`.
    #[must_use]
    pub fn source_version(&self) -> &str {
        &self.source_version
    }

    /// Returns the normalized header version as `major.minor`.
    #[must_use]
    pub fn output_version(&self) -> &str {
        &self.output_version
    }

    /// Always `false`: this exporter does not certify conformance.
    #[must_use]
    pub const fn certification_claimed(&self) -> bool {
        false
    }

    /// Returns the stable scope statement.
    #[must_use]
    pub const fn scope_note(&self) -> &'static str {
        SCOPE_NOTE
    }

    /// Returns a stable, human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}: normalized {} -> {} ({} applied, {} reported-only, certification=false)",
            self.target,
            self.source_version,
            self.output_version,
            self.applied.len(),
            self.reported_only.len()
        )
    }
}

// ---------------------------------------------------------------------------
// Minimal classic-xref reader (no dependencies, no panics)
// ---------------------------------------------------------------------------

#[derive(Debug)]
struct ParsedPdf {
    version: (u8, u8),
    xref_offset: usize,
    objects: Vec<Option<(u16, usize)>>,
    max_object: u32,
    trailer: Vec<u8>,
    catalog_dict: Vec<u8>,
}

fn is_ws(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\r' | b'\n' | 0x0c | 0x00)
}

fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && is_ws(bytes[pos]) {
        pos += 1;
    }
    pos
}

fn read_uint(bytes: &[u8], pos: &mut usize) -> Option<u32> {
    *pos = skip_ws(bytes, *pos);
    let start = *pos;
    let mut value: u32 = 0;
    while *pos < bytes.len() && bytes[*pos].is_ascii_digit() {
        value = value
            .checked_mul(10)
            .and_then(|v| v.checked_add(u32::from(bytes[*pos] - b'0')))?;
        *pos += 1;
    }
    if *pos == start { None } else { Some(value) }
}

fn skip_literal_string(bytes: &[u8], mut pos: usize) -> usize {
    pos += 1;
    let mut depth = 1usize;
    while pos < bytes.len() {
        match bytes[pos] {
            b'\\' => pos = pos.saturating_add(2).min(bytes.len()),
            b'(' => {
                depth += 1;
                pos += 1;
            }
            b')' => {
                depth -= 1;
                pos += 1;
                if depth == 0 {
                    return pos;
                }
            }
            _ => pos += 1,
        }
    }
    bytes.len()
}

/// Returns the index just past the `>>` closing a dictionary at `start`.
fn find_dict_end(bytes: &[u8], start: usize) -> Option<usize> {
    if bytes.get(start) != Some(&b'<') || bytes.get(start + 1) != Some(&b'<') {
        return None;
    }
    let mut pos = start + 2;
    let mut depth = 1usize;
    while pos < bytes.len() {
        match bytes[pos] {
            b'<' if bytes.get(pos + 1) == Some(&b'<') => {
                depth += 1;
                pos += 2;
            }
            b'<' => {
                pos += 1;
                while pos < bytes.len() && bytes[pos] != b'>' {
                    pos += 1;
                }
                pos = pos.saturating_add(1).min(bytes.len());
            }
            b'>' if bytes.get(pos + 1) == Some(&b'>') => {
                depth -= 1;
                pos += 2;
                if depth == 0 {
                    return Some(pos);
                }
            }
            b'>' => pos += 1,
            b'(' => pos = skip_literal_string(bytes, pos),
            b'%' => {
                while pos < bytes.len() && bytes[pos] != b'\n' && bytes[pos] != b'\r' {
                    pos += 1;
                }
            }
            _ => pos += 1,
        }
    }
    None
}

fn name_at(bytes: &[u8], pos: usize) -> Option<(&str, usize)> {
    if bytes.get(pos) != Some(&b'/') {
        return None;
    }
    let start = pos + 1;
    let mut end = start;
    while end < bytes.len()
        && !is_ws(bytes[end])
        && !matches!(bytes[end], b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')')
    {
        end += 1;
    }
    std::str::from_utf8(&bytes[start..end])
        .ok()
        .map(|name| (name, end))
}

/// Returns `true` when `key` is a top-level key of `dict` (which includes `<< >>`).
fn dict_has_key(dict: &[u8], key: &str) -> bool {
    let mut pos = 2usize;
    let mut depth = 1usize;
    while pos < dict.len() {
        match dict[pos] {
            b'<' if dict.get(pos + 1) == Some(&b'<') => {
                depth += 1;
                pos += 2;
            }
            b'<' => {
                pos += 1;
                while pos < dict.len() && dict[pos] != b'>' {
                    pos += 1;
                }
                pos = pos.saturating_add(1).min(dict.len());
            }
            b'>' if dict.get(pos + 1) == Some(&b'>') => {
                depth -= 1;
                pos += 2;
            }
            b'>' => pos += 1,
            b'(' => pos = skip_literal_string(dict, pos),
            b'/' if depth == 1 => {
                if let Some((name, end)) = name_at(dict, pos) {
                    if name.eq_ignore_ascii_case(key) {
                        return true;
                    }
                    pos = end;
                    continue;
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    false
}

/// Inserts `entry` just before the dictionary's closing `>>`.
fn dict_insert(dict: &[u8], entry: &str) -> Vec<u8> {
    let mut out = dict.to_vec();
    let insert_at = out.len().saturating_sub(2);
    let mut text = Vec::with_capacity(entry.len() + 2);
    text.push(b' ');
    text.extend_from_slice(entry.as_bytes());
    out.splice(insert_at..insert_at, text);
    out
}

/// Returns the index just past the value beginning at `pos`.
///
/// Handles the value shapes used by trailer dictionaries: names, numbers,
/// indirect references (`N G R`), literal and hex strings, and dictionaries.
fn value_end(dict: &[u8], pos: usize) -> usize {
    let mut pos = skip_ws(dict, pos);
    if pos >= dict.len() {
        return pos;
    }
    match dict[pos] {
        b'/' => name_at(dict, pos).map_or(pos + 1, |(_, end)| end),
        b'(' => skip_literal_string(dict, pos),
        b'<' if dict.get(pos + 1) == Some(&b'<') => find_dict_end(dict, pos).unwrap_or(dict.len()),
        b'<' => {
            pos += 1;
            while pos < dict.len() && dict[pos] != b'>' {
                pos += 1;
            }
            pos.saturating_add(1).min(dict.len())
        }
        b'-' | b'+' | b'0'..=b'9' => {
            let mut cursor = pos;
            let _first = read_uint(dict, &mut cursor);
            let reference_end = skip_ws(dict, cursor);
            let mut probe = reference_end;
            if read_uint(dict, &mut probe).is_some() {
                let after = skip_ws(dict, probe);
                if dict.get(after) == Some(&b'R') {
                    return after + 1;
                }
            }
            cursor
        }
        _ => {
            while pos < dict.len() && !is_ws(dict[pos]) {
                pos += 1;
            }
            pos
        }
    }
}

/// Removes a top-level `key` and its value from `dict`.
fn dict_remove_simple_value(dict: &[u8], key: &str) -> Vec<u8> {
    let mut pos = 2usize;
    let mut depth = 1usize;
    while pos < dict.len() {
        match dict[pos] {
            b'<' if dict.get(pos + 1) == Some(&b'<') => {
                depth += 1;
                pos += 2;
            }
            b'<' => {
                pos += 1;
                while pos < dict.len() && dict[pos] != b'>' {
                    pos += 1;
                }
                pos = pos.saturating_add(1).min(dict.len());
            }
            b'>' if dict.get(pos + 1) == Some(&b'>') => {
                depth -= 1;
                pos += 2;
            }
            b'>' => pos += 1,
            b'(' => pos = skip_literal_string(dict, pos),
            b'/' if depth == 1 => {
                if let Some((name, end)) = name_at(dict, pos) {
                    if name.eq_ignore_ascii_case(key) {
                        let value_pos = value_end(dict, end);
                        let mut out = Vec::with_capacity(dict.len());
                        out.extend_from_slice(&dict[..pos]);
                        out.extend_from_slice(&dict[value_pos..]);
                        return out;
                    }
                    pos = end;
                    continue;
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    dict.to_vec()
}

fn find_startxref(bytes: &[u8]) -> Option<usize> {
    if bytes.len() < 9 {
        return None;
    }
    let marker = b"startxref";
    let index = bytes
        .windows(marker.len())
        .rposition(|window| window == marker)?;
    let mut pos = index + marker.len();
    read_uint(bytes, &mut pos)
        .and_then(|offset| usize::try_from(offset).ok())
        .filter(|offset| *offset < bytes.len())
}

fn trailer_root(trailer: &[u8]) -> Option<u32> {
    let mut pos = 2usize;
    let mut depth = 1usize;
    while pos < trailer.len() {
        match trailer[pos] {
            b'<' if trailer.get(pos + 1) == Some(&b'<') => {
                depth += 1;
                pos += 2;
            }
            b'>' if trailer.get(pos + 1) == Some(&b'>') => {
                depth -= 1;
                pos += 2;
            }
            b'(' => pos = skip_literal_string(trailer, pos),
            b'/' if depth == 1 => {
                if let Some((name, end)) = name_at(trailer, pos) {
                    if name == "Root" {
                        let mut cursor = skip_ws(trailer, end);
                        let number = read_uint(trailer, &mut cursor)?;
                        let _generation = read_uint(trailer, &mut cursor)?;
                        cursor = skip_ws(trailer, cursor);
                        if trailer.get(cursor) == Some(&b'R') {
                            return Some(number);
                        }
                        return None;
                    }
                    pos = end;
                    continue;
                }
                pos += 1;
            }
            _ => pos += 1,
        }
    }
    None
}

fn parse_pdf(bytes: &[u8]) -> Result<ParsedPdf, ConformanceExportError> {
    let malformed = |reason: &str| ConformanceExportError::MalformedPdf {
        reason: reason.to_owned(),
    };
    let unsupported = |reason: &str| ConformanceExportError::UnsupportedStructure {
        reason: reason.to_owned(),
    };

    if !bytes.starts_with(b"%PDF-") {
        return Err(malformed("missing %PDF- header at byte zero"));
    }
    let version_text = bytes
        .get(5..8)
        .ok_or_else(|| malformed("truncated PDF header version"))?;
    if version_text[1] != b'.'
        || !version_text[0].is_ascii_digit()
        || !version_text[2].is_ascii_digit()
    {
        return Err(malformed("malformed PDF header version"));
    }
    let version = (version_text[0] - b'0', version_text[2] - b'0');

    let xref_offset =
        find_startxref(bytes).ok_or_else(|| malformed("missing or invalid startxref"))?;
    if !bytes[xref_offset..].starts_with(b"xref") {
        return Err(unsupported("compressed xref streams are not supported"));
    }

    let mut pos = xref_offset + 4;
    let mut objects: Vec<Option<(u16, usize)>> = Vec::new();
    loop {
        pos = skip_ws(bytes, pos);
        if bytes[pos..].starts_with(b"trailer") {
            break;
        }
        let start = read_uint(bytes, &mut pos)
            .ok_or_else(|| malformed("invalid xref subsection start"))?
            as usize;
        let count = read_uint(bytes, &mut pos)
            .ok_or_else(|| malformed("invalid xref subsection count"))?
            as usize;
        if start.saturating_add(count) > 1_000_000 {
            return Err(malformed("xref subsection is unreasonably large"));
        }
        if objects.len() < start.saturating_add(count) {
            objects.resize(start + count, None);
        }
        for index in 0..count {
            let offset =
                read_uint(bytes, &mut pos).ok_or_else(|| malformed("invalid xref entry offset"))?;
            let generation = read_uint(bytes, &mut pos)
                .ok_or_else(|| malformed("invalid xref entry generation"))?;
            let generation = u16::try_from(generation)
                .map_err(|_| malformed("xref entry generation is too large"))?;
            pos = skip_ws(bytes, pos);
            let flag = bytes
                .get(pos)
                .copied()
                .ok_or_else(|| malformed("truncated xref entry"))?;
            pos += 1;
            let offset =
                usize::try_from(offset).map_err(|_| malformed("xref entry offset is too large"))?;
            if offset >= bytes.len() {
                return Err(malformed("xref entry offset is outside the file"));
            }
            if flag == b'n' {
                objects[start + index] = Some((generation, offset));
            }
        }
    }

    pos = skip_ws(bytes, pos + b"trailer".len());
    if bytes.get(pos) != Some(&b'<') || bytes.get(pos + 1) != Some(&b'<') {
        return Err(malformed("missing trailer dictionary"));
    }
    let trailer_end =
        find_dict_end(bytes, pos).ok_or_else(|| malformed("unterminated trailer dictionary"))?;
    let trailer = bytes[pos..trailer_end].to_vec();
    if dict_has_key(&trailer, "Encrypt") {
        return Err(ConformanceExportError::Encrypted {
            label: String::new(),
        });
    }
    if dict_has_key(&trailer, "Prev") {
        return Err(unsupported(
            "incremental-update xref chains are not supported",
        ));
    }

    let root = trailer_root(&trailer).ok_or_else(|| malformed("trailer has no /Root reference"))?;
    let root_index =
        usize::try_from(root).map_err(|_| malformed("catalog object number is too large"))?;
    let (generation, object_offset) = objects
        .get(root_index)
        .and_then(|entry| entry.as_ref())
        .ok_or_else(|| malformed("trailer /Root is not an in-use object"))?;
    if *generation != 0 {
        return Err(unsupported(
            "non-zero generation catalog objects are not supported",
        ));
    }

    let mut cursor = *object_offset;
    let object_number =
        read_uint(bytes, &mut cursor).ok_or_else(|| malformed("catalog object has no number"))?;
    let object_generation = read_uint(bytes, &mut cursor)
        .ok_or_else(|| malformed("catalog object has no generation"))?;
    cursor = skip_ws(bytes, cursor);
    if object_number != root || object_generation != 0 || !bytes[cursor..].starts_with(b"obj") {
        return Err(malformed(
            "trailer /Root does not point at a catalog object",
        ));
    }
    cursor += 3;
    cursor = skip_ws(bytes, cursor);
    if bytes.get(cursor) != Some(&b'<') || bytes.get(cursor + 1) != Some(&b'<') {
        return Err(malformed("catalog object does not start with a dictionary"));
    }
    let catalog_end =
        find_dict_end(bytes, cursor).ok_or_else(|| malformed("unterminated catalog dictionary"))?;
    let catalog_dict = bytes[cursor..catalog_end].to_vec();

    let max_object = u32::try_from(objects.len().saturating_sub(1)).unwrap_or(u32::MAX);

    Ok(ParsedPdf {
        version,
        xref_offset,
        objects,
        max_object,
        trailer,
        catalog_dict,
    })
}

// ---------------------------------------------------------------------------
// Placeholder objects and deterministic serialization
// ---------------------------------------------------------------------------

const XMP_STREAM: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>\n\
<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n\
 <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n\
  <rdf:Description rdf:about=\"\" xmlns:pdf=\"http://ns.adobe.com/pdf/1.3/\">\n\
   <pdf:Producer>PDFXI conformance normalizer placeholder</pdf:Producer>\n\
  </rdf:Description>\n\
 </rdf:RDF>\n\
</x:xmpmeta>\n\
<?xpacket end=\"w\"?>\n";

fn xmp_object(number: u32) -> Vec<u8> {
    let mut out = format!(
        "{number} 0 obj\n<< /Type /Metadata /Subtype /XML /Length {} >>\nstream\n",
        XMP_STREAM.len()
    )
    .into_bytes();
    out.extend_from_slice(XMP_STREAM.as_bytes());
    out.extend_from_slice(b"endstream\nendobj\n");
    out
}

fn output_intent_object(number: u32, profile_number: u32, subtype: &str) -> Vec<u8> {
    format!(
        "{number} 0 obj\n<< /Type /OutputIntent /S /{subtype} \
/OutputConditionIdentifier (PDFXI placeholder) \
/Info (Placeholder - supply a real ICC profile) \
/DestOutputProfile {profile_number} 0 R >>\nendobj\n"
    )
    .into_bytes()
}

fn icc_placeholder_object(number: u32) -> Vec<u8> {
    format!("{number} 0 obj\n<< /Length 0 /N 3 >>\nstream\n\nendstream\nendobj\n").into_bytes()
}

/// Collapses runs of whitespace outside strings to one space so repeated
/// normalizations of the same document produce identical bytes.
fn collapse_spaces(dict: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(dict.len());
    let mut pos = 0usize;
    let mut in_string = false;
    let mut escaped = false;
    while pos < dict.len() {
        let byte = dict[pos];
        if in_string {
            out.push(byte);
            escaped = byte == b'\\' && !escaped;
            if !escaped && (byte == b')' || byte == b'>') {
                in_string = false;
            }
            pos += 1;
            continue;
        }
        if byte == b'<' && dict.get(pos + 1) == Some(&b'<') {
            out.extend_from_slice(b"<<");
            pos += 2;
        } else if byte == b'(' || byte == b'<' {
            in_string = true;
            out.push(byte);
            pos += 1;
        } else if is_ws(byte) {
            out.push(b' ');
            while pos < dict.len() && is_ws(dict[pos]) {
                pos += 1;
            }
        } else {
            out.push(byte);
            pos += 1;
        }
    }
    out
}

fn normalized_trailer(trailer: &[u8], size: u32, root_number: Option<u32>) -> Vec<u8> {
    let mut cleaned = dict_remove_simple_value(trailer, "Size");
    cleaned = dict_remove_simple_value(&cleaned, "Prev");
    if root_number.is_some() {
        cleaned = dict_remove_simple_value(&cleaned, "Root");
    }
    let cleaned = collapse_spaces(&cleaned);
    let mut cleaned = dict_insert(&cleaned, &format!("/Size {size}"));
    if let Some(number) = root_number {
        cleaned = dict_insert(&cleaned, &format!("/Root {number} 0 R"));
    }
    cleaned
}

fn serialize(
    source: &[u8],
    parsed: &ParsedPdf,
    version: (u8, u8),
    new_objects: &[Vec<u8>],
    size: u32,
    catalog_number: Option<u32>,
) -> Vec<u8> {
    let mut out = Vec::with_capacity(parsed.xref_offset.saturating_add(512));
    out.extend_from_slice(&source[..parsed.xref_offset]);
    out[5] = b'0' + version.0;
    out[7] = b'0' + version.1;

    // Object bytes before the original xref are copied verbatim: changing the
    // catalog in place would shift every later object and invalidate the xref.
    // A modified catalog is therefore appended as a new object and /Root is
    // repointed at it. The parsed catalog dictionary is kept as the parse
    // record even when the replacement object is what reaches the trailer.
    // Appended objects are numbered contiguously from max_object + 1. Record
    // their offsets so the regenerated xref marks them in use, not free.
    let first_appended = parsed.max_object.saturating_add(1);
    let mut appended_offsets = Vec::with_capacity(new_objects.len());
    for object in new_objects {
        appended_offsets.push(out.len());
        out.extend_from_slice(object);
    }

    let xref_offset = out.len();
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for number in 1..size {
        let appended_index = number.checked_sub(first_appended).and_then(|value| {
            usize::try_from(value)
                .ok()
                .filter(|index| *index < appended_offsets.len())
        });
        if let Some(offset) = appended_index.and_then(|index| appended_offsets.get(index)) {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
            continue;
        }
        let index = usize::try_from(number).unwrap_or(usize::MAX);
        match parsed.objects.get(index).and_then(|entry| entry.as_ref()) {
            Some((generation, offset)) => {
                out.extend_from_slice(format!("{offset:010} {generation:05} n \n").as_bytes());
            }
            None => out.extend_from_slice(b"0000000000 65535 f \n"),
        }
    }
    // Always rewrite /Root in the same position, whether or not a new catalog
    // object was appended, so a second normalization is byte-identical.
    let root_number = catalog_number.or_else(|| trailer_root(&parsed.trailer));
    out.extend_from_slice(b"trailer\n");
    out.extend_from_slice(&normalized_trailer(&parsed.trailer, size, root_number));
    out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    out
}

/// Normalizes `source` for `request` and returns bytes plus an honest report.
///
/// See the module documentation for the enforced and reported-only subset.
/// Every error case is typed; the source slice is never mutated.
pub fn export_conformance_pdf(
    source: &[u8],
    request: &ConformanceRequest,
    options: &NormalizationOptions,
) -> Result<ConformanceExport, ConformanceExportError> {
    let target = request.resolve()?;
    let parsed = parse_pdf(source).map_err(|error| match error {
        ConformanceExportError::Encrypted { .. } => ConformanceExportError::Encrypted {
            label: target.label().to_owned(),
        },
        other => other,
    })?;

    let minimum = target.minimum_version();
    let output_version = if options.normalize_version && parsed.version < minimum {
        minimum
    } else {
        parsed.version
    };

    let mut report = ConformanceExportReport::new(target, parsed.version, output_version);
    let mut catalog = parsed.catalog_dict.clone();
    let mut new_objects: Vec<Vec<u8>> = Vec::new();
    let first_appended = parsed.max_object.checked_add(1).ok_or_else(|| {
        ConformanceExportError::UnsupportedStructure {
            reason: "object number space is exhausted".to_owned(),
        }
    })?;
    let mut next_object = first_appended;
    let mut catalog_changed = false;

    if options.normalize_version && output_version != parsed.version {
        report.applied.push(ConformanceFinding::new(
            ExportSeverity::Info,
            "EXPORT.VERSION_NORMALIZED",
            format!(
                "header version raised from {}.{} to {}.{} for {}",
                parsed.version.0, parsed.version.1, output_version.0, output_version.1, target
            ),
            "header",
        ));
    }

    if options.add_xmp_metadata && target.requires_xmp() && !dict_has_key(&catalog, "Metadata") {
        let number = next_object;
        next_object = next_object.checked_add(1).ok_or_else(|| {
            ConformanceExportError::UnsupportedStructure {
                reason: "object number space is exhausted".to_owned(),
            }
        })?;
        new_objects.push(xmp_object(number));
        catalog = dict_insert(&catalog, &format!("/Metadata {number} 0 R"));
        catalog_changed = true;
        report.applied.push(ConformanceFinding::new(
            ExportSeverity::Warning,
            "EXPORT.XMP_PLACEHOLDER_ADDED",
            "XMP metadata placeholder added; replace it with real document metadata".to_owned(),
            &format!("object {number}"),
        ));
    }

    // An existing /OutputIntents entry is caller-supplied and is never
    // overwritten; only a missing one receives a placeholder.
    if options.add_output_intent
        && !dict_has_key(&catalog, "OutputIntents")
        && let Some(subtype) = target.output_intent_subtype()
    {
        let intent_number = next_object;
        let profile_number = intent_number.checked_add(1).ok_or_else(|| {
            ConformanceExportError::UnsupportedStructure {
                reason: "object number space is exhausted".to_owned(),
            }
        })?;
        next_object = profile_number.checked_add(1).ok_or_else(|| {
            ConformanceExportError::UnsupportedStructure {
                reason: "object number space is exhausted".to_owned(),
            }
        })?;
        new_objects.push(output_intent_object(intent_number, profile_number, subtype));
        new_objects.push(icc_placeholder_object(profile_number));
        catalog = dict_insert(&catalog, &format!("/OutputIntents [{intent_number} 0 R]"));
        catalog_changed = true;
        report.applied.push(ConformanceFinding::new(
            ExportSeverity::Warning,
            "EXPORT.OUTPUT_INTENT_PLACEHOLDER_ADDED",
            format!(
                "output-intent placeholder with /S /{subtype} added; supply a real ICC profile"
            ),
            &format!("object {intent_number}"),
        ));
    }

    // Findings are always canonical: severity ascending, then code, then
    // message. ExportSeverity orders Info before Warning.
    report.applied.sort_by(|a, b| {
        a.severity
            .cmp(&b.severity)
            .then_with(|| a.code.cmp(&b.code))
            .then_with(|| a.message.cmp(&b.message))
    });

    // A changed catalog is appended as a fresh object rather than patched in
    // place: patching in place would shift every later object and invalidate
    // the original xref offsets. /Root is repointed at the replacement.
    let mut catalog_number = None;
    if catalog_changed {
        let number = next_object;
        next_object = next_object.checked_add(1).ok_or_else(|| {
            ConformanceExportError::UnsupportedStructure {
                reason: "object number space is exhausted".to_owned(),
            }
        })?;
        let mut object = format!("{number} 0 obj\n").into_bytes();
        object.extend_from_slice(&catalog);
        object.extend_from_slice(b"\nendobj\n");
        new_objects.push(object);
        catalog_number = Some(number);
    }

    let size = next_object;
    let bytes = serialize(
        source,
        &parsed,
        output_version,
        &new_objects,
        size,
        catalog_number,
    );

    Ok(ConformanceExport { bytes, report })
}

/// Returns `true` when `bytes` has the structural markers of a PDF produced by
/// this exporter. This is a cheap probe, not a conformance claim.
#[must_use]
pub fn looks_like_normalized_pdf(bytes: &[u8]) -> bool {
    bytes.starts_with(b"%PDF-")
        && bytes
            .windows(b"startxref".len())
            .any(|window| window == b"startxref")
        && bytes
            .windows(b"%%EOF".len())
            .any(|window| window == b"%%EOF")
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Builds a small, valid classic-xref PDF with one page.
    fn fixture(version: &str, catalog_extra: &str, trailer_extra: &str) -> Vec<u8> {
        let catalog = format!("<< /Type /Catalog /Pages 2 0 R {catalog_extra}>>");
        let objects = [
            format!("1 0 obj\n{catalog}\nendobj\n"),
            "2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n".to_owned(),
            "3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] >>\nendobj\n".to_owned(),
        ];
        let mut out = format!("%PDF-{version}\n").into_bytes();
        out.extend_from_slice(&[0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
        let mut offsets = Vec::new();
        for object in objects {
            offsets.push(out.len());
            out.extend_from_slice(object.as_bytes());
        }
        let xref_offset = out.len();
        out.extend_from_slice(b"xref\n0 4\n0000000000 65535 f \n");
        for offset in offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size 4 /Root 1 0 R {trailer_extra}>>\nstartxref\n{xref_offset}\n%%EOF\n"
            )
            .as_bytes(),
        );
        out
    }

    fn text(bytes: &[u8]) -> String {
        String::from_utf8_lossy(bytes).into_owned()
    }

    fn export(source: &[u8], label: &str) -> Result<ConformanceExport, ConformanceExportError> {
        export_conformance_pdf(
            source,
            &ConformanceRequest::new(label),
            &NormalizationOptions::all(),
        )
    }

    #[test]
    fn t0093_export_a1b_adds_xmp_and_output_intent() {
        let result = export(&fixture("1.4", "", ""), "PDF/A-1b");
        let result = result.unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let output = text(&result.bytes);
        assert!(output.contains("/Metadata 4 0 R"));
        assert!(output.contains("/OutputIntents [5 0 R]"));
        assert!(output.contains("/S /GTS_PDFA1"));
        assert!(output.contains("/DestOutputProfile 6 0 R"));
        assert!(output.contains("/Type /Metadata /Subtype /XML"));
        assert!(output.starts_with("%PDF-1.4"));
        assert!(looks_like_normalized_pdf(&result.bytes));
        let codes: Vec<&str> = result
            .report
            .applied()
            .iter()
            .map(ConformanceFinding::code)
            .collect();
        assert_eq!(
            codes,
            vec![
                "EXPORT.OUTPUT_INTENT_PLACEHOLDER_ADDED",
                "EXPORT.XMP_PLACEHOLDER_ADDED"
            ]
        );
    }

    #[test]
    fn t0093_export_a2b_raises_version_and_sets_gts_pdfa1() {
        let result = export(&fixture("1.4", "", ""), "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        assert!(text(&result.bytes).starts_with("%PDF-1.7"));
        assert_eq!(result.report.source_version(), "1.4");
        assert_eq!(result.report.output_version(), "1.7");
        assert!(text(&result.bytes).contains("/S /GTS_PDFA1"));
        assert!(
            result
                .report
                .applied()
                .iter()
                .any(|finding| finding.code() == "EXPORT.VERSION_NORMALIZED")
        );
    }

    #[test]
    fn t0093_export_x4_uses_gts_pdfx() {
        let result = export(&fixture("1.4", "", ""), "PDF/X-4")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let output = text(&result.bytes);
        assert!(output.starts_with("%PDF-1.6"));
        assert!(output.contains("/S /GTS_PDFX"));
        assert!(!output.contains("GTS_PDFA1"));
    }

    #[test]
    fn t0093_export_e1_does_not_add_output_intent_or_xmp() {
        let result = export(&fixture("1.6", "", ""), "PDF/E-1")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let output = text(&result.bytes);
        assert!(output.contains("/Size 4"));
        assert!(!output.contains("/Metadata"));
        assert!(!output.contains("/OutputIntents"));
        assert!(result.report.applied().is_empty());
    }

    #[test]
    fn t0093_existing_metadata_and_intent_are_preserved() {
        let source = fixture("1.7", "/Metadata 9 0 R /OutputIntents [9 0 R] ", "");
        let result = export(&source, "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let output = text(&result.bytes);
        assert!(output.contains("/Metadata 9 0 R"));
        assert!(output.contains("/OutputIntents [9 0 R]"));
        assert!(output.contains("/Size 4"));
        assert!(result.report.applied().is_empty());
    }

    #[test]
    fn t0093_version_is_never_lowered() {
        let result = export(&fixture("2.0", "", ""), "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        assert!(text(&result.bytes).starts_with("%PDF-2.0"));
        assert!(
            !result
                .report
                .applied()
                .iter()
                .any(|finding| finding.code() == "EXPORT.VERSION_NORMALIZED")
        );
    }

    #[test]
    fn t0093_unsupported_profile_is_a_typed_error() {
        for label in ["PDF/VT-1", "PDF/UA-1", "nonsense"] {
            let error = export(&fixture("1.7", "", ""), label)
                .err()
                .unwrap_or_else(|| panic!("test: {label} must be rejected"));
            assert_eq!(
                error,
                ConformanceExportError::UnsupportedProfile {
                    label: label.to_owned()
                }
            );
            assert!(error.to_string().contains("unsupported"));
        }
    }

    #[test]
    fn t0093_contradictory_profile_is_a_typed_error() {
        let source = fixture("1.7", "", "");
        let wrong = export_conformance_pdf(
            &source,
            &ConformanceRequest::new("PDF/A-2b").with_output_intent_subtype("GTS_PDFX"),
            &NormalizationOptions::all(),
        );
        assert!(matches!(
            wrong,
            Err(ConformanceExportError::ContradictoryRequest { .. })
        ));
        let not_used = export_conformance_pdf(
            &source,
            &ConformanceRequest::new("PDF/E-1").with_output_intent_subtype("GTS_PDFA1"),
            &NormalizationOptions::all(),
        );
        assert!(matches!(
            not_used,
            Err(ConformanceExportError::ContradictoryRequest { .. })
        ));

        // A matching subtype, ignoring case and an optional slash, is accepted.
        let ok = export_conformance_pdf(
            &source,
            &ConformanceRequest::new("PDF/X-4").with_output_intent_subtype("/gts_pdfx"),
            &NormalizationOptions::all(),
        );
        assert!(ok.is_ok());
    }

    #[test]
    fn t0093_encrypted_source_is_a_typed_error() {
        let source = fixture("1.7", "", "/Encrypt 4 0 R ");
        let error = export(&source, "PDF/A-2b")
            .err()
            .unwrap_or_else(|| panic!("test: encryption must be rejected"));
        assert_eq!(
            error,
            ConformanceExportError::Encrypted {
                label: "PDF/A-2b".to_owned()
            }
        );
        assert!(error.to_string().contains("encrypted"));
    }

    #[test]
    fn t0093_xref_stream_and_malformed_inputs_are_typed_errors() {
        let stream = b"%PDF-1.7\n1 0 obj\n<< /Type /XRef >>\nendobj\nstartxref\n9\n%%EOF\n";
        assert!(matches!(
            export(stream, "PDF/A-2b"),
            Err(ConformanceExportError::UnsupportedStructure { .. })
        ));
        assert!(matches!(
            export(b"", "PDF/A-2b"),
            Err(ConformanceExportError::MalformedPdf { .. })
        ));
        assert!(matches!(
            export(b"not a pdf", "PDF/A-2b"),
            Err(ConformanceExportError::MalformedPdf { .. })
        ));
        assert!(matches!(
            export(b"%PDF-1.7 no xref here", "PDF/A-2b"),
            Err(ConformanceExportError::MalformedPdf { .. })
        ));

        let with_prev = fixture("1.7", "", "/Prev 0 ");
        assert!(matches!(
            export(&with_prev, "PDF/A-2b"),
            Err(ConformanceExportError::UnsupportedStructure { .. })
        ));
    }

    #[test]
    fn t0093_output_is_deterministic_and_findings_are_sorted() {
        let source = fixture("1.4", "", "");
        let a = export(&source, "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let b = export(&source, "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        assert_eq!(a, b);
        let severities: Vec<ExportSeverity> = a
            .report
            .applied()
            .iter()
            .map(ConformanceFinding::severity)
            .collect();
        assert!(severities.windows(2).all(|pair| pair[0] <= pair[1]));
        let keys: Vec<(ExportSeverity, &str)> = a
            .report
            .applied()
            .iter()
            .map(|finding| (finding.severity(), finding.code()))
            .collect();
        let mut sorted = keys.clone();
        sorted.sort_unstable();
        assert_eq!(keys, sorted, "findings follow severity then code order");
        for finding in a.report.applied() {
            assert!(finding.code().starts_with("EXPORT."));
            assert!(!finding.location().is_empty());
            assert!(!finding.message().is_empty());
        }
    }

    #[test]
    fn t0093_report_documents_enforced_reported_only_and_no_certification() {
        let result = export(&fixture("1.4", "", ""), "PDF/A-1b")
            .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let enforced = result.report.enforced();
        assert!(enforced.contains(&"minimum_version"));
        assert!(enforced.contains(&"xmp_metadata_presence"));
        assert!(enforced.contains(&"output_intent_presence"));
        assert!(enforced.contains(&"output_intent_subtype"));
        let reported = result.report.reported_only();
        for expected in [
            "icc_profile_validity",
            "font_embedding",
            "transparency",
            "encryption",
        ] {
            assert!(reported.contains(&expected));
        }
        assert!(!result.report.certification_claimed());
        assert!(result.report.scope_note().contains("no certification"));
        assert!(result.report.scope_note().contains("placeholders"));
        assert!(result.report.summary().contains("certification=false"));
    }

    #[test]
    fn t0093_normalization_options_can_disable_every_fixup() {
        let source = fixture("1.4", "", "");
        let result = export_conformance_pdf(
            &source,
            &ConformanceRequest::new("PDF/A-2b"),
            &NormalizationOptions::none(),
        )
        .unwrap_or_else(|error| panic!("test: export failed: {error}"));
        let output = text(&result.bytes);
        assert!(output.starts_with("%PDF-1.4"));
        assert!(!output.contains("/Metadata"));
        assert!(!output.contains("/OutputIntents"));
        assert!(result.report.applied().is_empty());
    }

    #[test]
    fn t0093_second_pass_is_idempotent() {
        let source = fixture("1.4", "", "");
        let first = export(&source, "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: first export failed: {error}"));
        let second = export(&first.bytes, "PDF/A-2b")
            .unwrap_or_else(|error| panic!("test: second export failed: {error}"));
        assert!(second.report.applied().is_empty());
        assert_eq!(text(&first.bytes).matches("/Size ").count(), 1);
        assert_eq!(second.bytes, first.bytes);
    }
}
