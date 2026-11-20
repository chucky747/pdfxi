//! Copy-on-write metadata and XMP scrubbing for documents.
//!
//! This module removes identifying or hidden information from the *structured*
//! metadata of a document: the document information dictionary (`/Info`), the
//! XMP metadata stream (`/Metadata`), piece information, thumbnails, embedded
//! files, annotation authorship/content fields (`/Contents`, `/T`, `/M`),
//! optional content (layer) names, and interactive form values.
//!
//! # Design and policy
//!
//! The crate is intentionally headless: it never opens a PDF and never logs
//! document contents. Callers own the object graph and map a [`ScrubDocument`]
//! onto real PDF objects. Scrubbing is copy-on-write — [`ScrubDocument::scrub`]
//! borrows the source immutably and returns a fresh [`ScrubOutcome`].
//!
//! ## XMP policy
//!
//! Robust byte-level surgery on arbitrary XML is both unsafe (comments,
//! whitespace, and CDATA can hide data) and dependency-heavy. Instead, XMP is
//! modelled as a set of typed [`XmpProperty`] values identified by namespace
//! URI, prefix, and local name. The scrubber either:
//!
//! - [`XmpStrategy::RemovePacket`] — removes the `/Metadata` stream entirely,
//! - [`XmpStrategy::ReplaceCanonical`] (the default) — deterministically
//!   replaces it with a minimal RDF/XML packet containing only allowlisted
//!   properties, or
//! - [`XmpStrategy::Selective`] — parses a canonical packet with
//!   [`XmpPacket::from_xml`], filters properties against the allowlist, and
//!   re-serialises the packet.
//!
//! ### What is validated and what is escaped
//!
//! [`XmpPacket::to_xml`] does not perform general XML validation. Instead it
//! relies on construction-time guarantees and escaping:
//!
//! - `prefix` and `local_name` are validated as XML NCNames when an
//!   [`XmpProperty`] is constructed or deserialised: they must be non-empty,
//!   must not contain `:`, must start with an ASCII letter or `_`, and may
//!   contain only ASCII letters, digits, `_`, `-`, and `.` thereafter. Invalid
//!   identifiers return [`SecurityError::ScrubInvalidXmpPrefix`] or
//!   [`SecurityError::ScrubInvalidXmpLocalName`] (empty values return
//!   [`SecurityError::ScrubInvalidFieldIdentifier`]). Names are therefore safe
//!   to interpolate into element and attribute names without further escaping.
//! - `namespace_uri` and property `value` are XML-escaped: `&`, `<`, `>`, `"`,
//!   and `'` are replaced by their entity forms before serialization. This
//!   keeps attribute values and element text well-formed for arbitrary values;
//!   it is escaping, not validation.
//!
//! [`XmpPacket::from_xml`] is deliberately **not** a general XML parser. It
//! accepts only the bounded, canonical structured subset this module emits:
//! the input length, tag count, and `rdf:RDF` nesting depth are capped; a
//! case-insensitive `<rdf:RDF ...>` root (optionally wrapped in `x:xmpmeta`
//! and preceded by a prolog) must appear in document order with a matching
//! `</rdf:RDF>` close tag. Arbitrary text that merely contains the substring
//! `rdf:RDF`, a truncated root, a mismatched close tag, or excessive nesting
//! is rejected with [`SecurityError::ScrubXmpMalformed`]. For third-party XMP,
//! callers supply the structured properties directly rather than raw XML.
//!
//! ## Verification
//!
//! [`ScrubReport`] carries deterministic before/after presence inventories,
//! the ordered action list, and residuals found by scanning the scrubbed
//! document for every removed non-empty string. In strict mode
//! [`ScrubReport::verify`] fails when any residual remains. Report types use a
//! redacted `Debug` implementation so document values are never printed.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

use crate::error::{Result, SecurityError};

/// Canonical packet opener from the public XMP specification.
const XMP_BEGIN: &str = "<?xpacket begin=\"\" id=\"W5M0MpCehiHzreSzNTczkc9d\"?>";
/// Canonical packet closer from the public XMP specification.
const XMP_END: &str = "<?xpacket end=\"w\"?>";
/// Public ISO 32000 / XMP namespace for the PDF producer property.
const PDF_NAMESPACE_URI: &str = "http://ns.adobe.com/pdf/1.3/";
/// Info dictionary key for the producer.
const PRODUCER_INFO_KEY: &str = "Producer";
/// XMP local name for the producer.
const PRODUCER_XMP_LOCAL: &str = "Producer";
/// Upper bound on the length of a packet accepted by [`XmpPacket::from_xml`].
///
/// Bounds work so hostile input cannot force unbounded scanning. Canonical
/// packets emitted by [`XmpPacket::to_xml`] are far smaller than this.
const MAX_XMP_BYTES: usize = 1 << 20;
/// Upper bound on the number of element tags processed while parsing a packet.
const MAX_XMP_TAGS: usize = 50_000;
/// Upper bound on nested `rdf:RDF` elements in a parsed packet.
const MAX_RDF_DEPTH: usize = 32;

/// A category of metadata that can be scrubbed.
///
/// Declaration order is the canonical ordering used by plans, inventories, and
/// reports.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScrubCategory {
    /// Document information dictionary (`/Info`).
    Info,
    /// XMP metadata stream (`/Metadata`).
    Xmp,
    /// Piece information dictionaries (`/PieceInfo`).
    PieceInfo,
    /// Page thumbnail images (`/Thumb`).
    Thumbnails,
    /// Embedded file attachments (the embedded-files name tree).
    EmbeddedFiles,
    /// Annotation `/Contents`, `/T` (title/author), and `/M` (modification) fields.
    AnnotationMetadata,
    /// Optional content (layer) names in `/OCProperties`.
    LayerNames,
    /// Interactive form field values (`/AcroForm`).
    FormValues,
}

impl ScrubCategory {
    /// Every category in canonical order.
    pub const ALL: [Self; 8] = [
        Self::Info,
        Self::Xmp,
        Self::PieceInfo,
        Self::Thumbnails,
        Self::EmbeddedFiles,
        Self::AnnotationMetadata,
        Self::LayerNames,
        Self::FormValues,
    ];

    /// Stable string identifier suitable for persistence or comparison.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Xmp => "xmp",
            Self::PieceInfo => "piece_info",
            Self::Thumbnails => "thumbnails",
            Self::EmbeddedFiles => "embedded_files",
            Self::AnnotationMetadata => "annotation_metadata",
            Self::LayerNames => "layer_names",
            Self::FormValues => "form_values",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    pub fn from_id(id: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|category| category.as_id() == id)
    }
}

impl fmt::Display for ScrubCategory {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// How a scrub action changes its target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum ScrubOperation {
    /// Delete the entry or stream entirely.
    Remove,
    /// Clear stored values while keeping the containing structure.
    Clear,
    /// Replace content with a deterministic canonical form.
    Replace,
}

impl ScrubOperation {
    /// Stable string identifier for this operation.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Remove => "remove",
            Self::Clear => "clear",
            Self::Replace => "replace",
        }
    }
}

impl fmt::Display for ScrubOperation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// One canonical step in a scrub plan.
///
/// Actions are ordered by category, then target, then operation, so a plan is
/// byte-for-byte reproducible. Targets are field identifiers (for example
/// `Info/Author`), never document values.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ScrubAction {
    category: ScrubCategory,
    target: String,
    operation: ScrubOperation,
}

impl ScrubAction {
    /// Build an action from its category, target identifier, and operation.
    pub fn new(
        category: ScrubCategory,
        target: impl Into<String>,
        operation: ScrubOperation,
    ) -> Self {
        Self {
            category,
            target: target.into(),
            operation,
        }
    }

    /// Category this action belongs to.
    pub const fn category(&self) -> ScrubCategory {
        self.category
    }

    /// Stable object-graph identifier this action operates on.
    pub fn target(&self) -> &str {
        &self.target
    }

    /// What the action does to the target.
    pub const fn operation(&self) -> ScrubOperation {
        self.operation
    }
}

impl fmt::Display for ScrubAction {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}: {}", self.category, self.target, self.operation)
    }
}

/// Identity of an XMP property: its namespace URI and local name.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct XmpPropertyKey {
    namespace_uri: String,
    local_name: String,
}

impl XmpPropertyKey {
    /// Build a property key.
    pub fn new(namespace_uri: impl Into<String>, local_name: impl Into<String>) -> Self {
        Self {
            namespace_uri: namespace_uri.into(),
            local_name: local_name.into(),
        }
    }

    /// Namespace URI of the property.
    pub fn namespace_uri(&self) -> &str {
        &self.namespace_uri
    }

    /// Local name of the property.
    pub fn local_name(&self) -> &str {
        &self.local_name
    }
}

impl fmt::Display for XmpPropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{{{}}}{}", self.namespace_uri, self.local_name)
    }
}

/// A single typed XMP property (namespace, prefix, local name, and value).
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct XmpProperty {
    prefix: String,
    namespace_uri: String,
    local_name: String,
    value: String,
}

impl XmpProperty {
    /// Build a property, validating its identifiers as XML NCNames.
    ///
    /// `prefix` and `local_name` must be non-empty XML NCNames in the ASCII
    /// subset: no colon, first character an ASCII letter or `_`, and remaining
    /// characters ASCII letters, digits, `_`, `-`, or `.`. This prevents
    /// identifier injection when the property is serialized by
    /// [`XmpPacket::to_xml`]. `namespace_uri` must be non-empty.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::ScrubInvalidFieldIdentifier`] when `prefix`,
    ///   `namespace_uri`, or `local_name` is empty or whitespace-only.
    /// - [`SecurityError::ScrubInvalidXmpPrefix`] when `prefix` is not a valid
    ///   NCName.
    /// - [`SecurityError::ScrubInvalidXmpLocalName`] when `local_name` is not a
    ///   valid NCName (including when it contains `:`).
    pub fn new(
        prefix: impl Into<String>,
        namespace_uri: impl Into<String>,
        local_name: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<Self> {
        let prefix = prefix.into();
        let namespace_uri = namespace_uri.into();
        let local_name = local_name.into();
        if prefix.is_empty() || namespace_uri.trim().is_empty() || local_name.is_empty() {
            return Err(SecurityError::ScrubInvalidFieldIdentifier);
        }
        if !is_ncname(&prefix) {
            return Err(SecurityError::ScrubInvalidXmpPrefix);
        }
        if !is_ncname(&local_name) {
            return Err(SecurityError::ScrubInvalidXmpLocalName);
        }
        Ok(Self {
            prefix,
            namespace_uri,
            local_name,
            value: value.into(),
        })
    }

    /// Namespace prefix used when serialising this property.
    pub fn prefix(&self) -> &str {
        &self.prefix
    }

    /// Namespace URI of the property.
    pub fn namespace_uri(&self) -> &str {
        &self.namespace_uri
    }

    /// Local name of the property.
    pub fn local_name(&self) -> &str {
        &self.local_name
    }

    /// Property value.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Identity of this property (namespace URI + local name).
    pub fn key(&self) -> XmpPropertyKey {
        XmpPropertyKey::new(self.namespace_uri.clone(), self.local_name.clone())
    }
}

// Redacted Debug: never print property values.
impl fmt::Debug for XmpProperty {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XmpProperty")
            .field("prefix", &self.prefix)
            .field("namespace_uri", &self.namespace_uri)
            .field("local_name", &self.local_name)
            .field("value", &"<redacted>")
            .finish()
    }
}

/// A structured XMP packet: an ordered set of typed properties.
#[derive(Clone, PartialEq, Eq, Default)]
pub struct XmpPacket {
    properties: Vec<XmpProperty>,
}

impl XmpPacket {
    /// Build a packet from properties.
    pub fn new(properties: impl IntoIterator<Item = XmpProperty>) -> Self {
        Self {
            properties: properties.into_iter().collect(),
        }
    }

    /// An empty packet.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Properties in insertion order.
    pub fn properties(&self) -> &[XmpProperty] {
        &self.properties
    }

    /// Return `true` when the packet has no properties.
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Number of properties in the packet.
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    /// Remove every property whose namespace URI equals `namespace_uri`.
    ///
    /// Returns the number of removed properties. Unknown namespaces are left
    /// untouched so safe, unrecognised content survives a scrub round trip.
    pub fn remove_namespace(&mut self, namespace_uri: &str) -> usize {
        let before = self.properties.len();
        self.properties
            .retain(|property| property.namespace_uri() != namespace_uri);
        before - self.properties.len()
    }

    /// Remove every property whose `(namespace URI, local name)` key matches.
    ///
    /// Returns the number of removed properties.
    pub fn remove_property(&mut self, key: &XmpPropertyKey) -> usize {
        let before = self.properties.len();
        self.properties.retain(|property| {
            property.namespace_uri() != key.namespace_uri()
                || property.local_name() != key.local_name()
        });
        before - self.properties.len()
    }

    /// Keep only properties for which `predicate` returns `true`.
    pub fn retain(&mut self, predicate: impl FnMut(&XmpProperty) -> bool) {
        self.properties.retain(predicate);
    }

    /// Distinct namespace URIs in deterministic (sorted) order.
    pub fn namespace_uris(&self) -> impl Iterator<Item = &str> {
        let uris: BTreeSet<&str> = self
            .properties
            .iter()
            .map(|property| property.namespace_uri())
            .collect();
        uris.into_iter()
    }

    /// Serialise the packet to a deterministic, well-formed RDF/XML XMP packet.
    pub fn to_xml(&self) -> String {
        let mut groups: BTreeMap<(String, String), Vec<&XmpProperty>> = BTreeMap::new();
        for property in &self.properties {
            groups
                .entry((property.prefix.clone(), property.namespace_uri.clone()))
                .or_default()
                .push(property);
        }

        let mut out = String::new();
        out.push_str(XMP_BEGIN);
        out.push('\n');
        out.push_str("<x:xmpmeta xmlns:x=\"adobe:ns:meta/\">\n");
        out.push_str(" <rdf:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n");
        for ((prefix, namespace_uri), properties) in groups {
            out.push_str(&format!(
                "  <rdf:Description xmlns:{prefix}=\"{}\">\n",
                xml_escape(&namespace_uri)
            ));
            for property in properties {
                out.push_str(&format!(
                    "   <{prefix}:{}>{}</{prefix}:{}>\n",
                    property.local_name,
                    xml_escape(&property.value),
                    property.local_name
                ));
            }
            out.push_str("  </rdf:Description>\n");
        }
        out.push_str(" </rdf:RDF>\n");
        out.push_str("</x:xmpmeta>\n");
        out.push_str(XMP_END);
        out
    }

    /// Parse a packet produced by [`Self::to_xml`] (the canonical subset).
    ///
    /// This is deliberately not a general XML parser. It requires a bounded
    /// input with a case-insensitive `<rdf:RDF ...>` root in document order and
    /// a matching close tag (see the module documentation). Descriptions are
    /// parsed only within that root's element content.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubXmpMalformed`] when the root or its close
    /// tag is missing, the input exceeds a size/tag/depth bound, or required
    /// namespace declarations are absent. Identifier errors from parsed
    /// properties are returned as the typed `ScrubInvalid*` variants.
    pub fn from_xml(xml: &str) -> Result<Self> {
        let (content_start, content_end) = validate_rdf_root(xml)?;
        let mut properties = Vec::new();
        let mut rest = &xml[content_start..content_end];
        const OPEN: &str = "<rdf:Description";
        const CLOSE: &str = "</rdf:Description>";
        let mut tags = 0usize;
        while let Some(start_rel) = rest.find(OPEN) {
            tags += 1;
            if tags > MAX_XMP_TAGS {
                return Err(SecurityError::ScrubXmpMalformed);
            }
            let start = start_rel;
            let open_end_rel = rest[start..]
                .find('>')
                .ok_or(SecurityError::ScrubXmpMalformed)?;
            let open_end = start + open_end_rel;
            let open_tag = &rest[start..=open_end];
            let prefixes = parse_namespace_decls(open_tag)?;
            let close_rel = rest[open_end..]
                .find(CLOSE)
                .ok_or(SecurityError::ScrubXmpMalformed)?;
            let close = open_end + close_rel;
            let body = &rest[open_end + 1..close];
            parse_child_properties(body, &prefixes, &mut properties)?;
            rest = &rest[close + CLOSE.len()..];
        }
        Ok(Self { properties })
    }
}

// Redacted Debug: never print property values.
impl fmt::Debug for XmpPacket {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("XmpPacket")
            .field("properties", &self.properties.len())
            .finish()
    }
}

/// Return `true` when `name` is a non-empty XML NCName in the accepted ASCII
/// subset.
///
/// A valid name starts with an ASCII letter or `_` and continues with ASCII
/// letters, digits, `_`, `-`, or `.`. Colons, whitespace, quotes, angle
/// brackets, and every other character are rejected. This is a conservative
/// subset of the XML 1.0 NCName production and is sufficient for the canonical
/// packets this crate emits and parses.
fn is_ncname(name: &str) -> bool {
    let mut chars = name.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    if !(first.is_ascii_alphabetic() || first == '_') {
        return false;
    }
    chars.all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.'))
}

/// Case-insensitive ASCII `starts_with` over byte slices.
fn starts_with_ci(haystack: &[u8], needle: &[u8]) -> bool {
    haystack.len() >= needle.len() && haystack[..needle.len()].eq_ignore_ascii_case(needle)
}

/// Find the first case-insensitive ASCII occurrence of `needle` in `haystack`.
fn find_ci(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window.eq_ignore_ascii_case(needle))
}

/// Advance past ASCII whitespace starting at `pos`.
fn skip_ws(bytes: &[u8], mut pos: usize) -> usize {
    while pos < bytes.len() && bytes[pos].is_ascii_whitespace() {
        pos += 1;
    }
    pos
}

/// Return `true` when the byte at `pos` ends an element name (whitespace or
/// `>`), so `<rdf:RDF` cannot match `<rdf:RDFFoo`.
fn is_name_end(bytes: &[u8], pos: usize) -> bool {
    bytes
        .get(pos)
        .is_some_and(|byte| byte.is_ascii_whitespace() || *byte == b'>' || *byte == b'/')
}

/// Locate the `>` that closes the tag beginning at `pos`, within the bound.
fn tag_end(bytes: &[u8], pos: usize) -> Result<usize> {
    bytes[pos..]
        .iter()
        .position(|byte| *byte == b'>')
        .map(|offset| pos + offset)
        .ok_or(SecurityError::ScrubXmpMalformed)
}

/// Escape five XML special characters so a value stays well-formed XML.
fn xml_escape(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            '"' => out.push_str("&quot;"),
            '\'' => out.push_str("&apos;"),
            _ => out.push(ch),
        }
    }
    out
}

/// Reverse [`xml_escape`]. Order matters: `&amp;` is decoded last.
fn xml_unescape(value: &str) -> String {
    value
        .replace("&lt;", "<")
        .replace("&gt;", ">")
        .replace("&quot;", "\"")
        .replace("&apos;", "'")
        .replace("&amp;", "&")
}

/// Validate that `xml` has a bounded `rdf:RDF` root element in document order.
///
/// Returns the byte range `[content_start, content_end)` covering the root's
/// element content. An XML prolog (whitespace, processing instructions, and
/// comments) and a single `x:xmpmeta` wrapper are skipped; any other content
/// before the root, a missing or mismatched close tag, or work beyond the
/// configured bounds is rejected with [`SecurityError::ScrubXmpMalformed`].
fn validate_rdf_root(xml: &str) -> Result<(usize, usize)> {
    if xml.len() > MAX_XMP_BYTES {
        return Err(SecurityError::ScrubXmpMalformed);
    }
    let bytes = xml.as_bytes();
    let mut pos = 0usize;
    let mut tags = 0usize;

    // Skip the prolog: whitespace, processing instructions, and comments.
    loop {
        pos = skip_ws(bytes, pos);
        if tags > MAX_XMP_TAGS {
            return Err(SecurityError::ScrubXmpMalformed);
        }
        match bytes.get(pos) {
            Some(b'<') => {}
            _ => return Err(SecurityError::ScrubXmpMalformed),
        }
        if starts_with_ci(&bytes[pos..], b"<?") {
            let end = find_ci(&bytes[pos..], b"?>").ok_or(SecurityError::ScrubXmpMalformed)?;
            pos += end + 2;
            tags += 1;
            continue;
        }
        if starts_with_ci(&bytes[pos..], b"<!--") {
            let end = find_ci(&bytes[pos..], b"-->").ok_or(SecurityError::ScrubXmpMalformed)?;
            pos += end + 3;
            tags += 1;
            continue;
        }
        break;
    }

    // Skip the single `x:xmpmeta` wrapper emitted by `to_xml`, if present.
    const META: &[u8] = b"<x:xmpmeta";
    if starts_with_ci(&bytes[pos..], META) && is_name_end(bytes, pos + META.len()) {
        pos = tag_end(bytes, pos + 1)? + 1;
        pos = skip_ws(bytes, pos);
        tags += 1;
    }

    // The next element must be the `rdf:RDF` root (case-insensitive).
    const RDF_OPEN: &[u8] = b"<rdf:RDF";
    if !starts_with_ci(&bytes[pos..], RDF_OPEN) || !is_name_end(bytes, pos + RDF_OPEN.len()) {
        return Err(SecurityError::ScrubXmpMalformed);
    }
    let content_start = tag_end(bytes, pos + 1)? + 1;
    tags += 1;
    if tags > MAX_XMP_TAGS {
        return Err(SecurityError::ScrubXmpMalformed);
    }

    let content_end = find_matching_rdf_close(bytes, content_start, &mut tags)?;
    // Nothing may be hidden after the closing root tag. Only whitespace,
    // processing instructions, comments, and the matching x:xmpmeta close are
    // accepted; any other element or free text is rejected.
    validate_trailing_content(bytes, content_end + "</rdf:RDF>".len(), &mut tags)?;
    Ok((content_start, content_end))
}

/// Reject content after the `rdf:RDF` root's closing tag.
fn validate_trailing_content(bytes: &[u8], mut pos: usize, tags: &mut usize) -> Result<()> {
    const META_CLOSE: &[u8] = b"</x:xmpmeta";
    loop {
        pos = skip_ws(bytes, pos);
        if pos >= bytes.len() {
            return Ok(());
        }
        if starts_with_ci(&bytes[pos..], b"<?") {
            let end = find_ci(&bytes[pos..], b"?>").ok_or(SecurityError::ScrubXmpMalformed)?;
            pos += end + 2;
        } else if starts_with_ci(&bytes[pos..], b"<!--") {
            let end = find_ci(&bytes[pos..], b"-->").ok_or(SecurityError::ScrubXmpMalformed)?;
            pos += end + 3;
        } else if starts_with_ci(&bytes[pos..], META_CLOSE)
            && is_name_end(bytes, pos + META_CLOSE.len())
        {
            pos = tag_end(bytes, pos + 1)? + 1;
        } else {
            return Err(SecurityError::ScrubXmpMalformed);
        }
        *tags += 1;
        if *tags > MAX_XMP_TAGS {
            return Err(SecurityError::ScrubXmpMalformed);
        }
    }
}

/// Find the byte index where the `</rdf:RDF>` matching the root at `pos` opens.
///
/// Nested roots are tracked up to [`MAX_RDF_DEPTH`]; every candidate tag is
/// charged to the shared `tags` budget so hostile input cannot spin forever.
fn find_matching_rdf_close(bytes: &[u8], mut pos: usize, tags: &mut usize) -> Result<usize> {
    const OPEN: &[u8] = b"<rdf:RDF";
    const CLOSE: &[u8] = b"</rdf:RDF";
    let mut depth = 1usize;
    while pos < bytes.len() {
        if *tags > MAX_XMP_TAGS {
            return Err(SecurityError::ScrubXmpMalformed);
        }
        let Some(rel) = find_ci(&bytes[pos..], b"<") else {
            break;
        };
        pos += rel;
        if starts_with_ci(&bytes[pos..], OPEN) && is_name_end(bytes, pos + OPEN.len()) {
            depth += 1;
            if depth > MAX_RDF_DEPTH {
                return Err(SecurityError::ScrubXmpMalformed);
            }
            *tags += 1;
            pos += OPEN.len();
        } else if starts_with_ci(&bytes[pos..], CLOSE) && is_name_end(bytes, pos + CLOSE.len()) {
            *tags += 1;
            depth -= 1;
            if depth == 0 {
                return Ok(pos);
            }
            pos += CLOSE.len();
        } else {
            pos += 1;
            *tags += 1;
        }
    }
    Err(SecurityError::ScrubXmpMalformed)
}

/// Extract `xmlns:prefix="uri"` declarations from an opening tag.
fn parse_namespace_decls(tag: &str) -> Result<BTreeMap<String, String>> {
    let mut map = BTreeMap::new();
    let mut rest = tag;
    while let Some(index) = rest.find("xmlns:") {
        rest = &rest[index + "xmlns:".len()..];
        let eq = rest.find('=').ok_or(SecurityError::ScrubXmpMalformed)?;
        let prefix = &rest[..eq];
        rest = &rest[eq + 1..];
        let quote = rest
            .as_bytes()
            .first()
            .copied()
            .ok_or(SecurityError::ScrubXmpMalformed)?;
        if quote != b'"' && quote != b'\'' {
            return Err(SecurityError::ScrubXmpMalformed);
        }
        rest = &rest[1..];
        let end = rest
            .find(quote as char)
            .ok_or(SecurityError::ScrubXmpMalformed)?;
        let uri = &rest[..end];
        if !prefix.is_empty() {
            map.insert(prefix.to_string(), uri.to_string());
        }
        rest = &rest[end + 1..];
    }
    Ok(map)
}

/// Extract typed child properties from the body of an `rdf:Description`.
fn parse_child_properties(
    body: &str,
    prefixes: &BTreeMap<String, String>,
    out: &mut Vec<XmpProperty>,
) -> Result<()> {
    let mut rest = body;
    while let Some(open_rel) = rest.find('<') {
        let after = &rest[open_rel + 1..];
        if after.starts_with('/') || after.starts_with('?') || after.starts_with('!') {
            let Some(gt) = rest[open_rel..].find('>') else {
                break;
            };
            rest = &rest[open_rel + gt + 1..];
            continue;
        }
        let Some(gt_rel) = after.find('>') else {
            break;
        };
        let raw_name = &after[..gt_rel];
        let name = raw_name.split_whitespace().next().unwrap_or(raw_name);
        let content_start = open_rel + 1 + gt_rel + 1;
        let close_tag = format!("</{name}>");
        let Some(close_rel) = rest[content_start..].find(&close_tag) else {
            return Err(SecurityError::ScrubXmpMalformed);
        };
        let value = &rest[content_start..content_start + close_rel];
        let (prefix, local_name) = match name.split_once(':') {
            Some((prefix, local_name)) => (prefix, local_name),
            None => ("", name),
        };
        let namespace_uri = prefixes
            .get(prefix)
            .ok_or(SecurityError::ScrubXmpMalformed)?;
        out.push(XmpProperty::new(
            prefix,
            namespace_uri.clone(),
            local_name,
            xml_unescape(value),
        )?);
        rest = &rest[content_start + close_rel + close_tag.len()..];
    }
    Ok(())
}

/// An allowlist of identifiers that survive scrubbing.
///
/// Only info-dictionary keys and XMP properties can be allowlisted; structural
/// hidden data (thumbnails, attachments, annotation metadata, and so on) is
/// always removed when its category is selected.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Allowlist {
    info_keys: BTreeSet<String>,
    xmp_properties: BTreeSet<XmpPropertyKey>,
}

impl Allowlist {
    /// An empty allowlist (scrub everything).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Allow an info-dictionary key to survive.
    pub fn allow_info_key(&mut self, key: impl Into<String>) {
        self.info_keys.insert(key.into());
    }

    /// Allow an XMP property (by namespace URI and local name) to survive.
    pub fn allow_xmp_property(
        &mut self,
        namespace_uri: impl Into<String>,
        local_name: impl Into<String>,
    ) {
        self.xmp_properties
            .insert(XmpPropertyKey::new(namespace_uri, local_name));
    }

    /// Return `true` when `key` is allowlisted.
    pub fn allows_info_key(&self, key: &str) -> bool {
        self.info_keys.contains(key)
    }

    /// Return `true` when the XMP property is allowlisted.
    pub fn allows_xmp_property(&self, namespace_uri: &str, local_name: &str) -> bool {
        self.xmp_properties
            .contains(&XmpPropertyKey::new(namespace_uri, local_name))
    }

    /// Allowlisted info keys.
    pub fn info_keys(&self) -> impl Iterator<Item = &str> {
        self.info_keys.iter().map(String::as_str)
    }

    /// Allowlisted XMP property identities.
    pub fn xmp_properties(&self) -> impl Iterator<Item = &XmpPropertyKey> {
        self.xmp_properties.iter()
    }
}

/// How aggressively a scrubbing profile preserves selected identifiers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum ScrubProfile {
    /// Remove every selected item.
    #[default]
    All,
    /// Preserve only the PDF producer in `/Info` and XMP.
    KeepProducer,
    /// Preserve exactly the identifiers in a custom [`Allowlist`].
    Custom(Allowlist),
}

impl ScrubProfile {
    /// Resolve this profile into a concrete allowlist.
    pub fn effective_allowlist(&self) -> Allowlist {
        match self {
            Self::All => Allowlist::empty(),
            Self::KeepProducer => {
                let mut allowlist = Allowlist::empty();
                allowlist.allow_info_key(PRODUCER_INFO_KEY);
                allowlist.allow_xmp_property(PDF_NAMESPACE_URI, PRODUCER_XMP_LOCAL);
                allowlist
            }
            Self::Custom(allowlist) => allowlist.clone(),
        }
    }
}

impl fmt::Display for ScrubProfile {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::All => write!(f, "all"),
            Self::KeepProducer => write!(f, "keep_producer"),
            Self::Custom(_) => write!(f, "custom"),
        }
    }
}

/// How the XMP metadata stream is written after properties are filtered.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum XmpStrategy {
    /// Remove the `/Metadata` stream entirely.
    RemovePacket,
    /// Replace it with a deterministic, well-formed canonical packet.
    #[default]
    ReplaceCanonical,
    /// Parse a canonical packet, filter properties, and re-serialise it.
    Selective,
}

impl XmpStrategy {
    /// Stable string identifier for this strategy.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::RemovePacket => "remove_packet",
            Self::ReplaceCanonical => "replace_canonical",
            Self::Selective => "selective",
        }
    }
}

impl fmt::Display for XmpStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// Configuration for a scrub operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubConfig {
    categories: BTreeSet<ScrubCategory>,
    profile: ScrubProfile,
    xmp_strategy: XmpStrategy,
    strict: bool,
}

impl Default for ScrubConfig {
    fn default() -> Self {
        Self {
            categories: ScrubCategory::ALL.into_iter().collect(),
            profile: ScrubProfile::All,
            xmp_strategy: XmpStrategy::ReplaceCanonical,
            strict: false,
        }
    }
}

impl ScrubConfig {
    /// Select all categories with the default (remove-all) profile.
    pub fn all() -> Self {
        Self::default()
    }

    /// Select no categories (a no-op scrub).
    pub fn empty() -> Self {
        Self {
            categories: BTreeSet::new(),
            ..Self::default()
        }
    }

    /// Add `category` to the selection.
    pub fn include(&mut self, category: ScrubCategory) {
        self.categories.insert(category);
    }

    /// Remove `category` from the selection.
    pub fn exclude(&mut self, category: ScrubCategory) {
        self.categories.remove(&category);
    }

    /// Return `true` when `category` is selected.
    pub fn contains(&self, category: ScrubCategory) -> bool {
        self.categories.contains(&category)
    }

    /// Selected categories in canonical order.
    pub fn categories(&self) -> impl Iterator<Item = ScrubCategory> + '_ {
        self.categories.iter().copied()
    }

    /// Scrubbing profile.
    pub fn profile(&self) -> &ScrubProfile {
        &self.profile
    }

    /// Set the scrubbing profile.
    pub fn set_profile(&mut self, profile: ScrubProfile) {
        self.profile = profile;
    }

    /// XMP write strategy.
    pub const fn xmp_strategy(&self) -> XmpStrategy {
        self.xmp_strategy
    }

    /// Set the XMP write strategy.
    pub fn set_xmp_strategy(&mut self, strategy: XmpStrategy) {
        self.xmp_strategy = strategy;
    }

    /// Whether verification is strict.
    pub const fn strict(&self) -> bool {
        self.strict
    }

    /// Set strict verification.
    pub fn set_strict(&mut self, strict: bool) {
        self.strict = strict;
    }
}

/// One annotation's scrub-able metadata fields.
#[derive(Clone, PartialEq, Eq)]
pub struct AnnotationRecord {
    index: usize,
    contents: Option<String>,
    title: Option<String>,
    modified: Option<String>,
}

impl AnnotationRecord {
    /// Build an annotation record; `contents`, `title`, and `modified`
    /// correspond to `/Contents`, `/T`, and `/M` respectively.
    pub fn new(
        index: usize,
        contents: Option<String>,
        title: Option<String>,
        modified: Option<String>,
    ) -> Self {
        Self {
            index,
            contents,
            title,
            modified,
        }
    }

    /// 0-based annotation index (used only as a stable target identifier).
    pub const fn index(&self) -> usize {
        self.index
    }

    /// Annotation `/Contents`.
    pub fn contents(&self) -> Option<&str> {
        self.contents.as_deref()
    }

    /// Annotation `/T` (title/author).
    pub fn title(&self) -> Option<&str> {
        self.title.as_deref()
    }

    /// Annotation `/M` (modification date).
    pub fn modified(&self) -> Option<&str> {
        self.modified.as_deref()
    }
}

// Redacted Debug: never print annotation field values.
impl fmt::Debug for AnnotationRecord {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AnnotationRecord")
            .field("index", &self.index)
            .field("contents", &self.contents.as_ref().map(|_| "<redacted>"))
            .field("title", &self.title.as_ref().map(|_| "<redacted>"))
            .field("modified", &self.modified.as_ref().map(|_| "<redacted>"))
            .finish()
    }
}

/// The structured metadata of a document, owned entirely by the caller.
///
/// Cloning this value is cheap relative to a full document and underpins the
/// copy-on-write scrub operation. Values are never logged; [`fmt::Debug`] is
/// redacted to counts only.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct ScrubDocument {
    info: BTreeMap<String, String>,
    xmp: Option<XmpPacket>,
    piece_info: BTreeMap<String, String>,
    thumbnails: BTreeMap<String, String>,
    embedded_files: BTreeMap<String, String>,
    annotations: Vec<AnnotationRecord>,
    layer_names: Vec<String>,
    form_values: BTreeMap<String, String>,
}

impl ScrubDocument {
    /// Create an empty document.
    pub fn new() -> Self {
        Self::default()
    }

    /// Set an info-dictionary entry.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] when `key` is
    /// empty or whitespace-only.
    pub fn set_info(&mut self, key: impl Into<String>, value: impl Into<String>) -> Result<()> {
        let key = key.into();
        ensure_identifier(&key)?;
        self.info.insert(key, value.into());
        Ok(())
    }

    /// Info entries as `(key, value)` pairs in key order.
    pub fn info(&self) -> &BTreeMap<String, String> {
        &self.info
    }

    /// Set the XMP metadata packet.
    pub fn set_xmp(&mut self, packet: XmpPacket) {
        self.xmp = Some(packet);
    }

    /// The XMP metadata packet, if present.
    pub fn xmp(&self) -> Option<&XmpPacket> {
        self.xmp.as_ref()
    }

    /// Set a piece-information entry.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] for an empty key.
    pub fn set_piece_info(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<()> {
        let key = key.into();
        ensure_identifier(&key)?;
        self.piece_info.insert(key, value.into());
        Ok(())
    }

    /// Piece-information entries.
    pub fn piece_info(&self) -> &BTreeMap<String, String> {
        &self.piece_info
    }

    /// Add a thumbnail with a stable identifier and opaque data placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] for an empty id.
    pub fn add_thumbnail(&mut self, id: impl Into<String>, data: impl Into<String>) -> Result<()> {
        let id = id.into();
        ensure_identifier(&id)?;
        self.thumbnails.insert(id, data.into());
        Ok(())
    }

    /// Thumbnail entries.
    pub fn thumbnails(&self) -> &BTreeMap<String, String> {
        &self.thumbnails
    }

    /// Add an embedded file with a stable name and opaque data placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] for an empty name.
    pub fn add_embedded_file(
        &mut self,
        name: impl Into<String>,
        data: impl Into<String>,
    ) -> Result<()> {
        let name = name.into();
        ensure_identifier(&name)?;
        self.embedded_files.insert(name, data.into());
        Ok(())
    }

    /// Embedded file entries.
    pub fn embedded_files(&self) -> &BTreeMap<String, String> {
        &self.embedded_files
    }

    /// Add an annotation record.
    pub fn add_annotation(&mut self, annotation: AnnotationRecord) {
        self.annotations.push(annotation);
    }

    /// Annotation records.
    pub fn annotations(&self) -> &[AnnotationRecord] {
        &self.annotations
    }

    /// Add an optional content (layer) name.
    pub fn add_layer_name(&mut self, name: impl Into<String>) {
        self.layer_names.push(name.into());
    }

    /// Optional content (layer) names.
    pub fn layer_names(&self) -> &[String] {
        &self.layer_names
    }

    /// Set a form field value.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] for an empty name.
    pub fn set_form_value(
        &mut self,
        field: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<()> {
        let field = field.into();
        ensure_identifier(&field)?;
        self.form_values.insert(field, value.into());
        Ok(())
    }

    /// Form field values.
    pub fn form_values(&self) -> &BTreeMap<String, String> {
        &self.form_values
    }

    /// Validate all identifiers held by this document.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubInvalidFieldIdentifier`] if any key, name,
    /// or identifier is empty or whitespace-only.
    pub fn validate(&self) -> Result<()> {
        for key in self.info.keys() {
            ensure_identifier(key)?;
        }
        for key in self.piece_info.keys() {
            ensure_identifier(key)?;
        }
        for key in self.thumbnails.keys() {
            ensure_identifier(key)?;
        }
        for key in self.embedded_files.keys() {
            ensure_identifier(key)?;
        }
        for key in self.form_values.keys() {
            ensure_identifier(key)?;
        }
        Ok(())
    }

    /// Scrub this document according to `config`, returning a fresh outcome.
    ///
    /// The source document is never mutated (copy-on-write).
    ///
    /// # Errors
    ///
    /// Returns an error when the document has invalid identifiers or when a
    /// selective XMP packet cannot be parsed.
    pub fn scrub(&self, config: &ScrubConfig) -> Result<ScrubOutcome> {
        self.validate()?;
        let allowlist = config.profile().effective_allowlist();
        let before = ScrubInventory::from_document(self);
        let mut cleaned = self.clone();
        let mut actions = Vec::new();
        let mut removed: Vec<(ScrubCategory, String, String)> = Vec::new();

        for category in config.categories() {
            match category {
                ScrubCategory::Info => {
                    let keys: Vec<String> = self.info.keys().cloned().collect();
                    for key in keys {
                        if allowlist.allows_info_key(&key) {
                            continue;
                        }
                        if let Some(value) = cleaned.info.remove(&key) {
                            let target = format!("Info/{key}");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                    }
                }
                ScrubCategory::Xmp => match config.xmp_strategy() {
                    XmpStrategy::RemovePacket => {
                        if let Some(packet) = cleaned.xmp.take() {
                            for property in packet.properties() {
                                removed.push((
                                    category,
                                    xmp_target(property),
                                    property.value().to_string(),
                                ));
                            }
                            actions.push(ScrubAction::new(category, "XMP", ScrubOperation::Remove));
                        }
                    }
                    XmpStrategy::ReplaceCanonical | XmpStrategy::Selective => {
                        let source = if config.xmp_strategy() == XmpStrategy::Selective {
                            self.xmp
                                .as_ref()
                                .map(|packet| XmpPacket::from_xml(&packet.to_xml()))
                                .transpose()?
                        } else {
                            self.xmp.clone()
                        };
                        if let Some(packet) = source {
                            let mut kept = Vec::new();
                            for property in packet.properties() {
                                if allowlist.allows_xmp_property(
                                    property.namespace_uri(),
                                    property.local_name(),
                                ) {
                                    kept.push(property.clone());
                                } else {
                                    removed.push((
                                        category,
                                        xmp_target(property),
                                        property.value().to_string(),
                                    ));
                                    actions.push(ScrubAction::new(
                                        category,
                                        xmp_target(property),
                                        ScrubOperation::Remove,
                                    ));
                                }
                            }
                            cleaned.xmp = Some(XmpPacket::new(kept));
                            actions.push(ScrubAction::new(
                                category,
                                "XMP",
                                ScrubOperation::Replace,
                            ));
                        }
                    }
                },
                ScrubCategory::PieceInfo => {
                    let keys: Vec<String> = self.piece_info.keys().cloned().collect();
                    for key in keys {
                        if let Some(value) = cleaned.piece_info.remove(&key) {
                            let target = format!("PieceInfo/{key}");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                    }
                }
                ScrubCategory::Thumbnails => {
                    let ids: Vec<String> = self.thumbnails.keys().cloned().collect();
                    for id in ids {
                        if let Some(value) = cleaned.thumbnails.remove(&id) {
                            let target = format!("Thumb/{id}");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                    }
                }
                ScrubCategory::EmbeddedFiles => {
                    let names: Vec<String> = self.embedded_files.keys().cloned().collect();
                    for name in names {
                        if let Some(value) = cleaned.embedded_files.remove(&name) {
                            let target = format!("EmbeddedFile/{name}");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                    }
                }
                ScrubCategory::AnnotationMetadata => {
                    for (index, annotation) in cleaned.annotations.iter_mut().enumerate() {
                        if let Some(value) = annotation.contents.take() {
                            let target = format!("Annots/{index}/Contents");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                        if let Some(value) = annotation.title.take() {
                            let target = format!("Annots/{index}/T");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                        if let Some(value) = annotation.modified.take() {
                            let target = format!("Annots/{index}/M");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(
                                category,
                                target,
                                ScrubOperation::Remove,
                            ));
                        }
                    }
                }
                ScrubCategory::LayerNames => {
                    for (index, name) in cleaned.layer_names.drain(..).enumerate() {
                        removed.push((category, format!("Layer/{index}"), name));
                        actions.push(ScrubAction::new(
                            category,
                            format!("Layer/{index}"),
                            ScrubOperation::Remove,
                        ));
                    }
                }
                ScrubCategory::FormValues => {
                    let fields: Vec<String> = self.form_values.keys().cloned().collect();
                    for field in fields {
                        if let Some(value) = cleaned.form_values.remove(&field) {
                            let target = format!("AcroForm/{field}");
                            removed.push((category, target.clone(), value));
                            actions.push(ScrubAction::new(category, target, ScrubOperation::Clear));
                        }
                    }
                }
            }
        }

        actions.sort();
        let after = ScrubInventory::from_document(&cleaned);
        let residuals = residual_scan(&cleaned, &removed);
        let report = ScrubReport {
            before,
            after,
            actions,
            residuals,
            profile: config.profile().clone(),
            xmp_strategy: config.xmp_strategy(),
            strict: config.strict(),
        };
        Ok(ScrubOutcome {
            document: cleaned,
            report,
        })
    }

    // Concatenate every remaining string value for residual scanning.
    fn haystack(&self) -> String {
        let mut parts = Vec::new();
        for value in self.info.values() {
            parts.push(value.clone());
        }
        if let Some(packet) = &self.xmp {
            for property in packet.properties() {
                parts.push(property.value().to_string());
            }
            parts.push(packet.to_xml());
        }
        for value in self.piece_info.values() {
            parts.push(value.clone());
        }
        for value in self.thumbnails.values() {
            parts.push(value.clone());
        }
        for value in self.embedded_files.values() {
            parts.push(value.clone());
        }
        for annotation in &self.annotations {
            if let Some(value) = &annotation.contents {
                parts.push(value.clone());
            }
            if let Some(value) = &annotation.title {
                parts.push(value.clone());
            }
            if let Some(value) = &annotation.modified {
                parts.push(value.clone());
            }
        }
        for name in &self.layer_names {
            parts.push(name.clone());
        }
        for value in self.form_values.values() {
            parts.push(value.clone());
        }
        parts.join("\n")
    }
}

// Redacted Debug: never print document values, only structure/counts.
impl fmt::Debug for ScrubDocument {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ScrubDocument")
            .field("info_entries", &self.info.len())
            .field(
                "xmp_properties",
                &self.xmp.as_ref().map_or(0, XmpPacket::len),
            )
            .field("piece_info_entries", &self.piece_info.len())
            .field("thumbnails", &self.thumbnails.len())
            .field("embedded_files", &self.embedded_files.len())
            .field("annotations", &self.annotations.len())
            .field("layer_names", &self.layer_names.len())
            .field("form_values", &self.form_values.len())
            .finish()
    }
}

fn ensure_identifier(value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(SecurityError::ScrubInvalidFieldIdentifier);
    }
    Ok(())
}

fn xmp_target(property: &XmpProperty) -> String {
    format!("XMP/{}:{}", property.prefix(), property.local_name())
}

/// Scan the scrubbed document for any removed non-empty string.
fn residual_scan(
    document: &ScrubDocument,
    removed: &[(ScrubCategory, String, String)],
) -> Vec<Residual> {
    let haystack = document.haystack();
    let mut residuals = Vec::new();
    for (category, target, value) in removed {
        let trimmed = value.trim();
        if trimmed.is_empty() {
            continue;
        }
        if haystack.contains(trimmed) {
            residuals.push(Residual::new(*category, target.clone()));
        }
    }
    residuals.sort();
    residuals.dedup();
    residuals
}

/// Presence counts for every metadata category in a document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubInventory {
    counts: BTreeMap<ScrubCategory, usize>,
}

impl ScrubInventory {
    fn from_document(document: &ScrubDocument) -> Self {
        let annotation_count = document
            .annotations
            .iter()
            .filter(|annotation| {
                annotation.contents.is_some()
                    || annotation.title.is_some()
                    || annotation.modified.is_some()
            })
            .count();
        let counts = ScrubCategory::ALL
            .into_iter()
            .map(|category| {
                let count = match category {
                    ScrubCategory::Info => document.info.len(),
                    ScrubCategory::Xmp => document.xmp.as_ref().map_or(0, XmpPacket::len),
                    ScrubCategory::PieceInfo => document.piece_info.len(),
                    ScrubCategory::Thumbnails => document.thumbnails.len(),
                    ScrubCategory::EmbeddedFiles => document.embedded_files.len(),
                    ScrubCategory::AnnotationMetadata => annotation_count,
                    ScrubCategory::LayerNames => document.layer_names.len(),
                    ScrubCategory::FormValues => document.form_values.len(),
                };
                (category, count)
            })
            .collect();
        Self { counts }
    }

    /// An inventory with nothing present.
    pub fn empty() -> Self {
        Self {
            counts: ScrubCategory::ALL
                .into_iter()
                .map(|category| (category, 0))
                .collect(),
        }
    }

    /// Count for `category`.
    pub fn get(&self, category: ScrubCategory) -> usize {
        self.counts.get(&category).copied().unwrap_or(0)
    }

    /// `(category, count)` pairs in canonical order.
    pub fn entries(&self) -> impl Iterator<Item = (ScrubCategory, usize)> + '_ {
        self.counts
            .iter()
            .map(|(&category, &count)| (category, count))
    }

    /// Total number of present items across all categories.
    pub fn total(&self) -> usize {
        self.counts.values().sum()
    }
}

/// A removed string that was still found in the scrubbed document.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct Residual {
    category: ScrubCategory,
    target: String,
}

impl Residual {
    /// Build a residual record.
    pub fn new(category: ScrubCategory, target: impl Into<String>) -> Self {
        Self {
            category,
            target: target.into(),
        }
    }

    /// Category of the residual.
    pub const fn category(&self) -> ScrubCategory {
        self.category
    }

    /// Identifier of the field whose removed value was found.
    pub fn target(&self) -> &str {
        &self.target
    }
}

impl fmt::Display for Residual {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}/{}", self.category, self.target)
    }
}

/// Result of a scrub operation, containing the scrubbed document and report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubOutcome {
    document: ScrubDocument,
    report: ScrubReport,
}

impl ScrubOutcome {
    /// The scrubbed (copy-on-write) document.
    pub fn document(&self) -> &ScrubDocument {
        &self.document
    }

    /// The verification report.
    pub fn report(&self) -> &ScrubReport {
        &self.report
    }

    /// Consume the outcome and return the scrubbed document.
    pub fn into_document(self) -> ScrubDocument {
        self.document
    }
}

/// Deterministic verification report for a scrub operation.
///
/// Contains before/after presence counts, the ordered action list, and any
/// residuals. No document values are stored or logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScrubReport {
    before: ScrubInventory,
    after: ScrubInventory,
    actions: Vec<ScrubAction>,
    residuals: Vec<Residual>,
    profile: ScrubProfile,
    xmp_strategy: XmpStrategy,
    strict: bool,
}

impl ScrubReport {
    /// Presence counts before scrubbing.
    pub fn before(&self) -> &ScrubInventory {
        &self.before
    }

    /// Presence counts after scrubbing.
    pub fn after(&self) -> &ScrubInventory {
        &self.after
    }

    /// Ordered scrub actions.
    pub fn actions(&self) -> &[ScrubAction] {
        &self.actions
    }

    /// Residual identifiers in deterministic order.
    pub fn residuals(&self) -> &[Residual] {
        &self.residuals
    }

    /// Profile used for the scrub.
    pub fn profile(&self) -> &ScrubProfile {
        &self.profile
    }

    /// XMP strategy used for the scrub.
    pub const fn xmp_strategy(&self) -> XmpStrategy {
        self.xmp_strategy
    }

    /// Whether this report enforces strict verification.
    pub const fn strict(&self) -> bool {
        self.strict
    }

    /// Return `true` when no removed string was found in the scrubbed document.
    pub fn is_clean(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Enforce the verification policy.
    ///
    /// In strict mode returns
    /// [`SecurityError::ScrubVerificationFailed`] for the first residual. In
    /// permissive mode this always succeeds; residuals remain available via
    /// [`Self::residuals`].
    ///
    /// # Errors
    ///
    /// See above.
    pub fn verify(&self) -> Result<()> {
        if self.strict
            && let Some(residual) = self.residuals.first()
        {
            return Err(SecurityError::ScrubVerificationFailed {
                category: residual.category(),
                target: residual.target().to_string(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_document() -> Result<ScrubDocument> {
        let mut doc = ScrubDocument::new();
        doc.set_info("Author", "Jane Doe")?;
        doc.set_info("Producer", "PDFXI")?;
        doc.set_xmp(XmpPacket::new([
            XmpProperty::new(
                "dc",
                "http://purl.org/dc/elements/1.1/",
                "creator",
                "Jane Doe",
            )?,
            XmpProperty::new("pdf", PDF_NAMESPACE_URI, "Producer", "PDFXI")?,
            XmpProperty::new(
                "xmp",
                "http://ns.adobe.com/xap/1.0/",
                "CreatorTool",
                "Internal",
            )?,
        ]));
        doc.set_piece_info("LastModified", "2026-01-01")?;
        doc.add_thumbnail("thumb-0", "thumbnail-image-data")?;
        doc.add_embedded_file("secret.txt", "attachment-bytes")?;
        doc.add_annotation(AnnotationRecord::new(
            0,
            Some("private note".to_string()),
            Some("Reviewer".to_string()),
            Some("D:20260101000000Z".to_string()),
        ));
        doc.add_layer_name("Internal Draft Layer");
        doc.set_form_value("field.name", "Ada Lovelace")?;
        Ok(doc)
    }

    #[test]
    fn info_removal_drops_entries_and_reports_before_after() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::Info);
        let outcome = doc.scrub(&config)?;
        assert!(outcome.document().info().is_empty());
        assert_eq!(outcome.document().info().len(), 0);
        // Other categories were not selected and must be untouched.
        assert!(outcome.document().xmp().is_some());
        assert_eq!(outcome.report().before().get(ScrubCategory::Info), 2);
        assert_eq!(outcome.report().after().get(ScrubCategory::Info), 0);
        assert!(
            outcome
                .report()
                .actions()
                .iter()
                .all(|action| action.category() == ScrubCategory::Info)
        );
        Ok(())
    }

    #[test]
    fn xmp_packet_can_be_removed_or_replaced() -> Result<()> {
        let doc = sample_document()?;

        let mut remove = ScrubConfig::empty();
        remove.include(ScrubCategory::Xmp);
        remove.set_xmp_strategy(XmpStrategy::RemovePacket);
        let removed = doc.scrub(&remove)?;
        assert!(removed.document().xmp().is_none());

        let mut replace = ScrubConfig::empty();
        replace.include(ScrubCategory::Xmp);
        replace.set_xmp_strategy(XmpStrategy::ReplaceCanonical);
        let replaced = doc.scrub(&replace)?;
        let packet = replaced
            .document()
            .xmp()
            .expect("replacement packet present");
        assert!(packet.properties().is_empty());
        let xml = packet.to_xml();
        assert!(xml.contains("<?xpacket"));
        assert!(xml.contains("rdf:RDF"));
        assert!(!xml.contains("Jane Doe"));
        Ok(())
    }

    #[test]
    fn allowlist_preserves_selected_entries() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::default();
        config.set_profile(ScrubProfile::KeepProducer);
        config.set_xmp_strategy(XmpStrategy::ReplaceCanonical);
        let outcome = doc.scrub(&config)?;
        // Only producer survives in Info and XMP.
        assert_eq!(outcome.document().info().len(), 1);
        assert_eq!(
            outcome
                .document()
                .info()
                .get("Producer")
                .map(String::as_str),
            Some("PDFXI")
        );
        assert!(!outcome.document().info().contains_key("Author"));
        let properties = outcome
            .document()
            .xmp()
            .expect("packet present")
            .properties();
        assert_eq!(properties.len(), 1);
        assert_eq!(properties[0].local_name(), "Producer");
        Ok(())
    }

    #[test]
    fn custom_allowlist_preserves_named_info_key_and_xmp_property() -> Result<()> {
        let doc = sample_document()?;
        let mut allowlist = Allowlist::empty();
        allowlist.allow_info_key("Author");
        allowlist.allow_xmp_property("http://ns.adobe.com/xap/1.0/", "CreatorTool");
        let mut config = ScrubConfig::default();
        config.set_profile(ScrubProfile::Custom(allowlist));
        let outcome = doc.scrub(&config)?;
        assert!(outcome.document().info().contains_key("Author"));
        assert!(!outcome.document().info().contains_key("Producer"));
        let locals: Vec<&str> = outcome
            .document()
            .xmp()
            .expect("packet present")
            .properties()
            .iter()
            .map(XmpProperty::local_name)
            .collect();
        assert_eq!(locals, vec!["CreatorTool"]);
        Ok(())
    }

    #[test]
    fn piece_info_and_thumbnails_are_removed() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::PieceInfo);
        config.include(ScrubCategory::Thumbnails);
        let outcome = doc.scrub(&config)?;
        assert!(outcome.document().piece_info().is_empty());
        assert!(outcome.document().thumbnails().is_empty());
        assert_eq!(outcome.report().before().get(ScrubCategory::PieceInfo), 1);
        assert_eq!(outcome.report().before().get(ScrubCategory::Thumbnails), 1);
        Ok(())
    }

    #[test]
    fn embedded_files_are_removed() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::EmbeddedFiles);
        let outcome = doc.scrub(&config)?;
        assert!(outcome.document().embedded_files().is_empty());
        assert_eq!(
            outcome.report().after().get(ScrubCategory::EmbeddedFiles),
            0
        );
        Ok(())
    }

    #[test]
    fn annotation_contents_author_and_time_are_cleared() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::AnnotationMetadata);
        let outcome = doc.scrub(&config)?;
        let annotation = &outcome.document().annotations()[0];
        assert_eq!(annotation.contents(), None);
        assert_eq!(annotation.title(), None);
        assert_eq!(annotation.modified(), None);
        let targets: Vec<&str> = outcome
            .report()
            .actions()
            .iter()
            .map(ScrubAction::target)
            .collect();
        assert!(targets.contains(&"Annots/0/Contents"));
        assert!(targets.contains(&"Annots/0/T"));
        assert!(targets.contains(&"Annots/0/M"));
        Ok(())
    }

    #[test]
    fn form_values_and_layer_names_are_cleared() -> Result<()> {
        let doc = sample_document()?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::FormValues);
        config.include(ScrubCategory::LayerNames);
        let outcome = doc.scrub(&config)?;
        assert!(outcome.document().form_values().is_empty());
        assert!(outcome.document().layer_names().is_empty());
        assert!(
            outcome
                .report()
                .actions()
                .iter()
                .any(|action| action.operation() == ScrubOperation::Clear)
        );
        Ok(())
    }

    #[test]
    fn residual_verification_detects_and_accepts() -> Result<()> {
        // A value that survives in another field becomes a residual.
        let mut doc = ScrubDocument::new();
        doc.set_info("Author", "Shared Name")?;
        doc.set_form_value("field.name", "Shared Name")?;
        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::Info);
        config.set_strict(true);
        let outcome = doc.scrub(&config)?;
        assert!(!outcome.report().is_clean());
        assert_eq!(outcome.report().residuals().len(), 1);
        assert_eq!(
            outcome.report().residuals()[0].category(),
            ScrubCategory::Info
        );
        assert_eq!(
            outcome.report().verify(),
            Err(SecurityError::ScrubVerificationFailed {
                category: ScrubCategory::Info,
                target: "Info/Author".to_string(),
            })
        );

        // Removing the surviving copy leaves the report clean.
        let mut clean_config = ScrubConfig::empty();
        clean_config.include(ScrubCategory::Info);
        clean_config.include(ScrubCategory::FormValues);
        clean_config.set_strict(true);
        let clean = doc.scrub(&clean_config)?;
        assert!(clean.report().is_clean());
        assert!(clean.report().verify().is_ok());
        Ok(())
    }

    #[test]
    fn report_is_deterministic_for_equivalent_configs() -> Result<()> {
        let doc = sample_document()?;
        let mut forward = ScrubConfig::default();
        forward.include(ScrubCategory::FormValues);
        forward.set_profile(ScrubProfile::All);
        let mut reverse = ScrubConfig::default();
        reverse.include(ScrubCategory::FormValues);
        reverse.set_profile(ScrubProfile::All);
        let a = doc.scrub(&forward)?;
        let b = doc.scrub(&reverse)?;
        assert_eq!(a.report(), b.report());
        assert!(
            a.report()
                .actions()
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
        assert!(
            a.report()
                .residuals()
                .windows(2)
                .all(|pair| pair[0] <= pair[1])
        );
        Ok(())
    }

    #[test]
    fn no_op_when_nothing_is_selected() -> Result<()> {
        let doc = sample_document()?;
        let config = ScrubConfig::empty();
        let outcome = doc.scrub(&config)?;
        assert_eq!(outcome.document(), &doc);
        assert!(outcome.report().actions().is_empty());
        assert!(outcome.report().is_clean());
        assert_eq!(outcome.report().before(), outcome.report().after());
        Ok(())
    }

    #[test]
    fn source_is_unchanged_after_copy_on_write_scrub() -> Result<()> {
        let doc = sample_document()?;
        let snapshot = doc.clone();
        let _ = doc.scrub(&ScrubConfig::all())?;
        assert_eq!(doc, snapshot);
        Ok(())
    }

    #[test]
    fn selective_xmp_parse_filter_and_reserialize_round_trips() -> Result<()> {
        let doc = sample_document()?;
        let original_xml = doc.xmp().expect("packet present").to_xml();
        let parsed = XmpPacket::from_xml(&original_xml)?;
        assert_eq!(parsed.len(), 3);
        assert_eq!(parsed.to_xml(), original_xml);

        let mut config = ScrubConfig::empty();
        config.include(ScrubCategory::Xmp);
        config.set_xmp_strategy(XmpStrategy::Selective);
        config.set_profile(ScrubProfile::KeepProducer);
        let outcome = doc.scrub(&config)?;
        let packet = outcome.document().xmp().expect("packet present");
        assert_eq!(packet.len(), 1);
        assert_eq!(packet.properties()[0].local_name(), "Producer");
        assert!(packet.to_xml().contains("rdf:RDF"));
        Ok(())
    }

    #[test]
    fn xmp_values_are_xml_escaped_and_parser_unescapes_them() -> Result<()> {
        let property = XmpProperty::new(
            "dc",
            "http://purl.org/dc/elements/1.1/",
            "description",
            "Tom & Jerry <draft>",
        )?;
        let packet = XmpPacket::new([property]);
        let xml = packet.to_xml();
        assert!(xml.contains("Tom &amp; Jerry &lt;draft&gt;"));
        let parsed = XmpPacket::from_xml(&xml)?;
        assert_eq!(parsed.properties()[0].value(), "Tom & Jerry <draft>");
        Ok(())
    }

    #[test]
    fn malformed_xmp_and_empty_identifiers_are_typed_errors() {
        assert_eq!(
            XmpPacket::from_xml("<x:xmpmeta>no rdf here</x:xmpmeta>"),
            Err(SecurityError::ScrubXmpMalformed)
        );
        assert_eq!(
            XmpProperty::new("", "urn:test", "name", "value"),
            Err(SecurityError::ScrubInvalidFieldIdentifier)
        );
        let mut doc = ScrubDocument::new();
        assert_eq!(
            doc.set_info("   ", "value"),
            Err(SecurityError::ScrubInvalidFieldIdentifier)
        );
    }

    #[test]
    fn debug_output_never_contains_document_values() -> Result<()> {
        let doc = sample_document()?;
        let formatted = format!("{doc:?}");
        assert!(!formatted.contains("Jane Doe"));
        assert!(!formatted.contains("attachment-bytes"));
        assert!(!formatted.contains("Ada Lovelace"));
        let xml = doc.xmp().expect("packet present").to_xml();
        assert!(!format!("{:?}", doc.xmp()).contains("Jane Doe"));
        assert!(xml.contains("Jane Doe")); // the value itself is still in the data
        Ok(())
    }

    #[test]
    fn category_and_operation_identifiers_round_trip() {
        for category in ScrubCategory::ALL {
            assert_eq!(ScrubCategory::from_id(category.as_id()), Some(category));
        }
        assert_eq!(ScrubCategory::from_id("unknown"), None);
        assert_eq!(ScrubOperation::Remove.as_id(), "remove");
        assert_eq!(ScrubOperation::Clear.to_string(), "clear");
        assert_eq!(ScrubProfile::All.to_string(), "all");
        assert_eq!(ScrubProfile::KeepProducer.to_string(), "keep_producer");
        assert_eq!(XmpStrategy::Selective.as_id(), "selective");
    }

    #[test]
    fn hostile_xmp_identifiers_are_rejected_as_typed_errors() {
        // Empty identifiers keep the original typed error.
        assert_eq!(
            XmpProperty::new("", "urn:test", "name", "value"),
            Err(SecurityError::ScrubInvalidFieldIdentifier)
        );
        assert_eq!(
            XmpProperty::new("dc", "urn:test", "", "value"),
            Err(SecurityError::ScrubInvalidFieldIdentifier)
        );
        // A quote, a space, and a colon in the prefix would break attribute
        // and element names in to_xml; each is a typed prefix error.
        for prefix in ["dc\"evil", "dc evil", "a:b", "1bad", "-lead"] {
            assert_eq!(
                XmpProperty::new(prefix, "urn:test", "name", "value"),
                Err(SecurityError::ScrubInvalidXmpPrefix),
                "prefix {prefix} must be rejected"
            );
        }
        // Angle brackets, a colon, and quotes in the local name are rejected.
        for local in ["na<me", "a:b", "na\"me", "with space", "9lives"] {
            assert_eq!(
                XmpProperty::new("dc", "urn:test", local, "value"),
                Err(SecurityError::ScrubInvalidXmpLocalName),
                "local name {local} must be rejected"
            );
        }
        // Valid NCNames still construct successfully.
        assert!(XmpProperty::new("dc_1", "urn:test", "my-name.ok", "v").is_ok());
    }

    #[test]
    fn from_xml_rejects_non_root_rdf_and_malformed_roots() -> Result<()> {
        // Arbitrary text that merely mentions rdf:RDF is not a packet.
        for xml in [
            "just some rdf:RDF text",
            "prefix <rdf:RDF></rdf:RDF>",
            "<foo><rdf:RDF></rdf:RDF></foo>",
        ] {
            assert_eq!(
                XmpPacket::from_xml(xml),
                Err(SecurityError::ScrubXmpMalformed),
                "non-root input {xml:?} must be rejected"
            );
        }
        // Truncated root: no close tag.
        assert_eq!(
            XmpPacket::from_xml("<rdf:RDF xmlns:rdf=\"urn:rdf\">"),
            Err(SecurityError::ScrubXmpMalformed)
        );
        // Mismatched close tag.
        assert_eq!(
            XmpPacket::from_xml("<rdf:RDF></rdf:Description>"),
            Err(SecurityError::ScrubXmpMalformed)
        );
        // A canonical packet still round-trips (guards against over-rejection).
        let original = sample_document()?.xmp().expect("packet present").to_xml();
        assert_eq!(XmpPacket::from_xml(&original)?.to_xml(), original);
        Ok(())
    }

    #[test]
    fn from_xml_rejects_trailing_content_after_root() {
        // Content after the closing rdf:RDF root could hide data from a scrub
        // round trip, so it is rejected rather than silently dropped.
        for xml in [
            "<rdf:RDF></rdf:RDF><extra/>",
            "<rdf:RDF></rdf:RDF>trailing text",
        ] {
            assert_eq!(
                XmpPacket::from_xml(xml),
                Err(SecurityError::ScrubXmpMalformed)
            );
        }
        // Whitespace, comments, processing instructions and the canonical
        // x:xmpmeta close remain acceptable.
        let canonical = XmpPacket::empty().to_xml();
        assert!(XmpPacket::from_xml(&canonical).is_ok());
    }

    #[test]
    fn from_xml_is_case_insensitive_and_accepts_a_bare_root() -> Result<()> {
        let bare = "<RDF:RDF xmlns:rdf=\"http://www.w3.org/1999/02/22-rdf-syntax-ns#\">\n \
                    <rdf:Description xmlns:dc=\"http://purl.org/dc/elements/1.1/\">\n \
                    <dc:title>Hello</dc:title>\n \
                    </rdf:Description>\n \
                    </RDF:RDF>";
        let parsed = XmpPacket::from_xml(bare)?;
        assert_eq!(parsed.len(), 1);
        assert_eq!(parsed.properties()[0].local_name(), "title");
        assert_eq!(parsed.properties()[0].value(), "Hello");
        Ok(())
    }

    #[test]
    fn from_xml_bounds_huge_nesting_and_oversized_input() {
        // Deeply nested rdf:RDF roots are rejected once the depth bound is hit.
        let mut deep = String::from("<rdf:RDF>");
        for _ in 0..(MAX_RDF_DEPTH + 2) {
            deep.push_str("<rdf:RDF>");
        }
        for _ in 0..(MAX_RDF_DEPTH + 2) {
            deep.push_str("</rdf:RDF>");
        }
        assert_eq!(
            XmpPacket::from_xml(&deep),
            Err(SecurityError::ScrubXmpMalformed)
        );

        // An input larger than the byte cap is rejected before any scanning.
        let mut huge = String::from("<rdf:RDF>");
        huge.push_str(&"x".repeat(MAX_XMP_BYTES));
        huge.push_str("</rdf:RDF>");
        assert_eq!(
            XmpPacket::from_xml(&huge),
            Err(SecurityError::ScrubXmpMalformed)
        );
    }
}
