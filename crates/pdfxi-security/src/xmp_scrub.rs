//! Minimal XMP namespace/property removal with verification (SEC-009).
//!
//! This module completes XMP scrubbing at the library level on top of the
//! structured [`XmpPacket`] parser:
//!
//! - [`scrub_xmp`] parses a canonical packet with [`XmpPacket::from_xml`],
//!   removes only the namespace URIs and property keys named in an
//!   [`XmpRemovalSpec`], and re-serialises deterministically.
//! - Properties the caller did not select are **preserved** — including ones
//!   in namespaces this crate has no special knowledge of — because they are
//!   typed, escaped, and emitted in a stable order. Nothing is silently
//!   rewritten or dropped.
//! - Malformed or oversized XML is rejected with a typed
//!   [`SecurityError::ScrubXmpMalformed`]; it is never partially scrubbed.
//! - [`verify_xmp_scrub`] re-parses the result and reports any selected
//!   namespace or property that survived, so a caller can prove the scrub was
//!   effective rather than trusting the writer.
//!
//! No property values or document contents are included in reports or errors.

use std::collections::BTreeSet;
use std::fmt;

use crate::error::{Result, SecurityError};
use crate::scrub::{ScrubCategory, XmpPacket, XmpPropertyKey};

/// Whether a removal record names a whole namespace or a single property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum XmpRemovalKind {
    /// Every property in a namespace URI was selected.
    Namespace,
    /// A single `(namespace URI, local name)` property was selected.
    Property,
}

impl XmpRemovalKind {
    /// Stable string identifier.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Namespace => "namespace",
            Self::Property => "property",
        }
    }
}

impl fmt::Display for XmpRemovalKind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// A record of one effective removal.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct XmpRemovalRecord {
    kind: XmpRemovalKind,
    target: String,
}

impl XmpRemovalRecord {
    /// Build a removal record.
    pub fn new(kind: XmpRemovalKind, target: impl Into<String>) -> Self {
        Self {
            kind,
            target: target.into(),
        }
    }

    /// What kind of selection was applied.
    pub const fn kind(&self) -> XmpRemovalKind {
        self.kind
    }

    /// Stable identifier of the removed namespace or property (no values).
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// An explicit selection of XMP namespaces and properties to remove.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct XmpRemovalSpec {
    namespaces: BTreeSet<String>,
    properties: BTreeSet<XmpPropertyKey>,
}

impl XmpRemovalSpec {
    /// An empty selection (nothing is removed).
    pub fn empty() -> Self {
        Self::default()
    }

    /// Select every property in a namespace URI.
    pub fn remove_namespace(&mut self, namespace_uri: impl Into<String>) {
        self.namespaces.insert(namespace_uri.into());
    }

    /// Select a single property by key.
    pub fn remove_property(&mut self, key: XmpPropertyKey) {
        self.properties.insert(key);
    }

    /// Selected namespace URIs in deterministic order.
    pub fn namespaces(&self) -> impl Iterator<Item = &str> {
        self.namespaces.iter().map(String::as_str)
    }

    /// Selected property keys in deterministic order.
    pub fn properties(&self) -> impl Iterator<Item = &XmpPropertyKey> {
        self.properties.iter()
    }

    /// Whether the specification selects anything.
    pub fn is_empty(&self) -> bool {
        self.namespaces.is_empty() && self.properties.is_empty()
    }

    /// Whether `namespace_uri` is selected.
    pub fn removes_namespace(&self, namespace_uri: &str) -> bool {
        self.namespaces.contains(namespace_uri)
    }

    /// Whether `key` is selected either directly or through its namespace.
    pub fn removes_property(&self, key: &XmpPropertyKey) -> bool {
        self.namespaces.contains(key.namespace_uri()) || self.properties.contains(key)
    }
}

/// Deterministic report of an XMP scrub run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpScrubReport {
    spec: XmpRemovalSpec,
    removed: Vec<XmpRemovalRecord>,
    retained: usize,
}

impl XmpScrubReport {
    /// The specification that was applied.
    pub fn spec(&self) -> &XmpRemovalSpec {
        &self.spec
    }

    /// Effective removals in canonical order.
    pub fn removed(&self) -> &[XmpRemovalRecord] {
        &self.removed
    }

    /// Number of unselected properties preserved in the output.
    pub const fn retained(&self) -> usize {
        self.retained
    }
}

/// The owned result of an XMP scrub.
#[derive(Debug, Clone)]
pub struct XmpScrubOutput {
    xml: String,
    packet: XmpPacket,
    report: XmpScrubReport,
}

impl XmpScrubOutput {
    /// Re-serialised, canonical XMP packet text.
    pub fn xml(&self) -> &str {
        &self.xml
    }

    /// The structured packet after removals.
    pub fn packet(&self) -> &XmpPacket {
        &self.packet
    }

    /// Deterministic removal report.
    pub fn report(&self) -> &XmpScrubReport {
        &self.report
    }
}

/// Parse `xml`, remove the selected namespaces and properties, and reserialise.
///
/// Properties not named by `spec` are preserved verbatim (as typed values) and
/// emitted in a stable order, so unknown-but-safe content survives the scrub.
///
/// # Errors
///
/// Returns [`SecurityError::ScrubXmpMalformed`] (or a typed identifier error)
/// if `xml` is not a bounded packet in the canonical structured subset.
pub fn scrub_xmp(xml: &str, spec: &XmpRemovalSpec) -> Result<XmpScrubOutput> {
    let packet = XmpPacket::from_xml(xml)?;
    let mut removed = BTreeSet::new();

    for namespace in &spec.namespaces {
        if packet
            .namespace_uris()
            .any(|present| present == namespace.as_str())
        {
            removed.insert(XmpRemovalRecord::new(
                XmpRemovalKind::Namespace,
                namespace.clone(),
            ));
        }
    }
    for property in packet.properties() {
        let key = property.key();
        if spec.properties.contains(&key) {
            removed.insert(XmpRemovalRecord::new(
                XmpRemovalKind::Property,
                key.to_string(),
            ));
        }
    }

    let mut kept = packet.clone();
    kept.retain(|property| !spec.removes_property(&property.key()));
    let xml_out = kept.to_xml();
    let retained = kept.len();

    Ok(XmpScrubOutput {
        xml: xml_out,
        packet: kept,
        report: XmpScrubReport {
            spec: spec.clone(),
            removed: removed.into_iter().collect(),
            retained,
        },
    })
}

/// A residual selected namespace or property found during verification.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct XmpResidual {
    target: String,
}

impl XmpResidual {
    /// Build a residual record.
    pub fn new(target: impl Into<String>) -> Self {
        Self {
            target: target.into(),
        }
    }

    /// Stable identifier of the surviving namespace or property (no values).
    pub fn target(&self) -> &str {
        &self.target
    }
}

/// Result of verifying that a scrub removed everything it selected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct XmpScrubVerification {
    spec: XmpRemovalSpec,
    residuals: Vec<XmpResidual>,
}

impl XmpScrubVerification {
    /// The specification that was checked.
    pub fn spec(&self) -> &XmpRemovalSpec {
        &self.spec
    }

    /// Residuals in canonical order.
    pub fn residuals(&self) -> &[XmpResidual] {
        &self.residuals
    }

    /// Whether no selected namespace or property survived.
    pub fn is_clean(&self) -> bool {
        self.residuals.is_empty()
    }

    /// Enforce verification.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::ScrubVerificationFailed`] for the first
    /// residual, scoped to [`ScrubCategory::Xmp`].
    pub fn check(&self) -> Result<()> {
        if let Some(residual) = self.residuals.first() {
            return Err(SecurityError::ScrubVerificationFailed {
                category: ScrubCategory::Xmp,
                target: residual.target.clone(),
            });
        }
        Ok(())
    }
}

/// Re-parse `xml` and confirm that every selected namespace and property is
/// absent.
///
/// This is a genuine post-scrub check: it reads the bytes a caller would
/// persist, not the in-memory packet used to produce them.
///
/// # Errors
///
/// Returns a typed XML error for malformed input; residuals are reported via
/// [`XmpScrubVerification::check`].
pub fn verify_xmp_scrub(xml: &str, spec: &XmpRemovalSpec) -> Result<XmpScrubVerification> {
    let packet = XmpPacket::from_xml(xml)?;
    let mut residuals = BTreeSet::new();

    for property in packet.properties() {
        if spec.removes_property(&property.key()) {
            residuals.insert(XmpResidual::new(property.key().to_string()));
        }
    }
    // A namespace URI may still be declared even if no property uses it.
    for namespace in &spec.namespaces {
        if xml.contains(namespace.as_str()) {
            residuals.insert(XmpResidual::new(namespace.clone()));
        }
    }

    Ok(XmpScrubVerification {
        spec: spec.clone(),
        residuals: residuals.into_iter().collect(),
    })
}

/// Convenience: scrub `xml` and verify the produced bytes in one call.
///
/// # Errors
///
/// See [`scrub_xmp`] and [`XmpScrubVerification::check`].
pub fn scrub_and_verify_xmp(xml: &str, spec: &XmpRemovalSpec) -> Result<XmpScrubOutput> {
    let output = scrub_xmp(xml, spec)?;
    verify_xmp_scrub(output.xml(), spec)?.check()?;
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scrub::XmpProperty;

    fn prop(prefix: &str, uri: &str, local: &str, value: &str) -> XmpProperty {
        XmpProperty::new(prefix, uri, local, value).expect("valid fixture property")
    }

    fn fixture_xml() -> String {
        let packet = XmpPacket::new([
            prop("dc", "http://purl.org/dc/elements/1.1/", "creator", "Alice"),
            prop("dc", "http://purl.org/dc/elements/1.1/", "title", "Secret"),
            prop("pdf", "http://ns.adobe.com/pdf/1.3/", "Producer", "PDFXI"),
            prop("xmp", "http://ns.adobe.com/xap/1.0/", "CreatorTool", "Tool"),
            prop(
                "custom",
                "http://example.com/unknown-ns",
                "Note",
                "safe note",
            ),
        ]);
        packet.to_xml()
    }

    #[test]
    fn removal_kind_identifiers_round_trip() {
        assert_eq!(XmpRemovalKind::Namespace.as_id(), "namespace");
        assert_eq!(XmpRemovalKind::Property.as_id(), "property");
        assert_eq!(XmpRemovalKind::Namespace.to_string(), "namespace");
        let spec = XmpRemovalSpec::empty();
        assert!(spec.is_empty());
    }

    #[test]
    fn remove_namespace_removes_only_that_namespace_and_preserves_unknown() {
        let xml = fixture_xml();
        let mut spec = XmpRemovalSpec::empty();
        spec.remove_namespace("http://purl.org/dc/elements/1.1/");
        let output = scrub_and_verify_xmp(&xml, &spec).expect("scrub");
        assert!(
            verify_xmp_scrub(output.xml(), &spec)
                .expect("verify")
                .is_clean()
        );
        assert!(!output.xml().contains("purl.org/dc/elements"));
        assert!(!output.xml().contains("Alice"));
        assert!(!output.xml().contains("Secret"));
        // Unknown namespace and other namespaces survive deterministically.
        assert!(output.xml().contains("example.com/unknown-ns"));
        assert!(output.xml().contains("safe note"));
        assert!(output.xml().contains("PDFXI"));
        assert!(output.xml().contains("Tool"));
        assert_eq!(output.report().retained(), 3);
        assert_eq!(
            output.report().removed(),
            &[XmpRemovalRecord::new(
                XmpRemovalKind::Namespace,
                "http://purl.org/dc/elements/1.1/"
            )]
        );
    }

    #[test]
    fn remove_property_removes_only_the_selected_property() {
        let xml = fixture_xml();
        let mut spec = XmpRemovalSpec::empty();
        spec.remove_property(XmpPropertyKey::new(
            "http://purl.org/dc/elements/1.1/",
            "title",
        ));
        let output = scrub_and_verify_xmp(&xml, &spec).expect("scrub");
        assert!(!output.xml().contains(">Secret<"));
        assert!(output.xml().contains(">Alice<"));
        assert_eq!(output.report().retained(), 4);
    }

    #[test]
    fn malformed_xmp_is_a_typed_error() {
        let mut spec = XmpRemovalSpec::empty();
        spec.remove_namespace("http://purl.org/dc/elements/1.1/");
        for bad in [
            "not xml at all",
            "<?xpacket?><rdf:RDF>",
            "<rdf:RDF></rdf:RDF><extra/>",
            "<rdf:RDF><rdf:RDF><rdf:RDF>",
        ] {
            assert!(matches!(
                scrub_xmp(bad, &spec),
                Err(SecurityError::ScrubXmpMalformed)
                    | Err(SecurityError::ScrubInvalidXmpPrefix)
                    | Err(SecurityError::ScrubInvalidXmpLocalName)
                    | Err(SecurityError::ScrubInvalidFieldIdentifier)
            ));
        }
    }

    #[test]
    fn verify_scrub_detects_a_residual_as_a_typed_error() {
        let xml = fixture_xml();
        let mut spec = XmpRemovalSpec::empty();
        spec.remove_namespace("http://purl.org/dc/elements/1.1/");
        // Deliberately verify the original (unscrubbed) bytes.
        let verification = verify_xmp_scrub(&xml, &spec).expect("parse");
        assert!(!verification.is_clean());
        assert!(!verification.residuals().is_empty());
        assert!(matches!(
            verification.check(),
            Err(SecurityError::ScrubVerificationFailed {
                category: ScrubCategory::Xmp,
                ..
            })
        ));
    }

    #[test]
    fn scrub_is_deterministic_and_source_is_unchanged() {
        let xml = fixture_xml();
        let original = xml.clone();
        let mut spec = XmpRemovalSpec::empty();
        spec.remove_property(XmpPropertyKey::new(
            "http://ns.adobe.com/xap/1.0/",
            "CreatorTool",
        ));
        let first = scrub_xmp(&xml, &spec).expect("first");
        let second = scrub_xmp(&xml, &spec).expect("second");
        assert_eq!(first.xml(), second.xml());
        assert_eq!(first.report(), second.report());
        assert_eq!(xml, original);
    }

    #[test]
    fn empty_spec_preserves_all_properties_deterministically() {
        let xml = fixture_xml();
        let spec = XmpRemovalSpec::empty();
        let output = scrub_and_verify_xmp(&xml, &spec).expect("noop scrub");
        assert_eq!(output.packet().len(), 5);
        assert_eq!(output.report().retained(), 5);
        assert!(output.report().removed().is_empty());
        assert_eq!(
            output.xml(),
            XmpPacket::from_xml(&xml).expect("parse").to_xml()
        );
    }

    #[test]
    fn removal_records_and_spec_are_deterministic() {
        let xml = fixture_xml();
        let build = || {
            let mut spec = XmpRemovalSpec::empty();
            spec.remove_namespace("http://ns.adobe.com/pdf/1.3/");
            spec.remove_property(XmpPropertyKey::new(
                "http://ns.adobe.com/xap/1.0/",
                "CreatorTool",
            ));
            spec
        };
        let a = build();
        let b = build();
        assert_eq!(a, b);
        assert_eq!(
            a.namespaces().collect::<Vec<_>>(),
            b.namespaces().collect::<Vec<_>>()
        );
        let first = scrub_xmp(&xml, &a).expect("first");
        let second = scrub_xmp(&xml, &b).expect("second");
        assert_eq!(first.report().removed(), second.report().removed());
    }
}
