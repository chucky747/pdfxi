//! Deterministic Office adapter contract for CREATE-006 (ADR 0008).
//!
//! [`crate::office_import`] already converts a caller-supplied
//! [`OfficeBundle`] into the [`crate::OfficeDocument`] model consumed by the
//! existing DOCX/XLSX/PPTX exporters. This module closes the remaining
//! *library-level* gap: an explicit envelope-validation entry point, a stable
//! conversion plan naming the target format and deterministic output part, and
//! a typed representation of every construct that could not be converted.
//!
//! Nothing here opens an archive, starts Office, uses COM, or touches the
//! network. The caller supplies markup it has already extracted; the plan is a
//! description of what the adapters will write, with a byte-for-byte stable
//! summary suitable for a conversion report.

use crate::office_export::OfficeFormat;
use crate::office_import::{
    OFFICE_BUNDLE_MAX_BYTES, OfficeBundle, OfficeBundleKind, OfficeImportError,
    OfficeImportFinding, import_office_bundle_with_report,
};

/// Maximum title length accepted by the envelope validator, in bytes.
pub const OFFICE_CONTRACT_MAX_TITLE_BYTES: usize = 512;

/// Map an input bundle family to the canonical output adapter format.
#[must_use]
pub const fn target_format(kind: OfficeBundleKind) -> OfficeFormat {
    match kind {
        OfficeBundleKind::Word => OfficeFormat::Docx,
        OfficeBundleKind::Excel => OfficeFormat::Xlsx,
        OfficeBundleKind::PowerPoint => OfficeFormat::Pptx,
    }
}

/// A typed construct that the Office intake recorded instead of converting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum OfficeUnsupported {
    /// Media content that is listed by name but never embedded.
    Media {
        /// The tag or reference that triggered the finding.
        token: String,
    },
    /// A tag outside the documented markup subset.
    UnknownTag {
        /// The unrecognised tag name.
        token: String,
    },
    /// A finding kind added by the importer after this contract was written.
    Other {
        /// Stable machine-readable kind from the importer.
        kind: &'static str,
        /// The token that triggered the finding.
        token: String,
    },
}

impl OfficeUnsupported {
    /// Convert an importer finding into its typed representation.
    #[must_use]
    pub fn from_finding(finding: &OfficeImportFinding) -> Self {
        match finding.kind {
            "media" => Self::Media {
                token: finding.token.clone(),
            },
            "unknown-tag" => Self::UnknownTag {
                token: finding.token.clone(),
            },
            other => Self::Other {
                kind: other,
                token: finding.token.clone(),
            },
        }
    }

    /// Return a stable machine-readable kind label.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Media { .. } => "media",
            Self::UnknownTag { .. } => "unknown-tag",
            Self::Other { kind, .. } => kind,
        }
    }

    /// Return the token that triggered the finding.
    #[must_use]
    pub fn token(&self) -> &str {
        match self {
            Self::Media { token } | Self::UnknownTag { token } | Self::Other { token, .. } => token,
        }
    }
}

/// A deterministic, host-consumable Office conversion plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OfficeConversionPlan {
    target: OfficeFormat,
    output_name: String,
    page_count: usize,
    block_count: usize,
    unsupported: Vec<OfficeUnsupported>,
    summary: String,
}

impl OfficeConversionPlan {
    /// Target adapter format selected from the bundle family.
    #[must_use]
    pub const fn target(&self) -> OfficeFormat {
        self.target
    }

    /// Sanitized, deterministic name of the output part, including extension.
    #[must_use]
    pub fn output_name(&self) -> &str {
        &self.output_name
    }

    /// Number of pages the importer produced.
    #[must_use]
    pub const fn page_count(&self) -> usize {
        self.page_count
    }

    /// Number of content blocks the importer produced.
    #[must_use]
    pub const fn block_count(&self) -> usize {
        self.block_count
    }

    /// Typed constructs that were recorded but not converted.
    #[must_use]
    pub fn unsupported(&self) -> &[OfficeUnsupported] {
        &self.unsupported
    }

    /// Stable, byte-for-byte deterministic report line for this plan.
    #[must_use]
    pub fn summary(&self) -> &str {
        &self.summary
    }
}

/// A typed failure while validating or planning an Office conversion.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum OfficeEnvelopeError {
    /// The declared title was empty.
    #[error("Office bundle title must not be empty")]
    EmptyTitle,
    /// The declared title exceeded [`OFFICE_CONTRACT_MAX_TITLE_BYTES`].
    #[error("Office bundle title exceeds {limit} bytes")]
    TitleTooLong {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The markup carried no non-whitespace content.
    #[error("Office bundle markup must not be empty")]
    EmptyMarkup,
    /// The markup exceeded [`OFFICE_BUNDLE_MAX_BYTES`].
    #[error("Office bundle markup exceeds {limit} bytes")]
    TooLarge {
        /// Enforced byte limit.
        limit: usize,
    },
    /// The importer rejected the markup.
    #[error("Office import failed: {0}")]
    Import(#[from] OfficeImportError),
    /// A recorded finding was promoted to an error in strict mode.
    #[error("unsupported Office construct '{kind}': {detail}")]
    UnsupportedConstruct {
        /// Stable machine-readable kind.
        kind: &'static str,
        /// Deterministic explanation.
        detail: String,
    },
}

/// Validate only the envelope fields, without parsing markup.
///
/// # Errors
///
/// Returns [`OfficeEnvelopeError::EmptyTitle`],
/// [`OfficeEnvelopeError::TitleTooLong`], [`OfficeEnvelopeError::EmptyMarkup`],
/// or [`OfficeEnvelopeError::TooLarge`] for an invalid envelope.
pub fn validate_office_envelope(bundle: &OfficeBundle) -> Result<(), OfficeEnvelopeError> {
    if bundle.title.is_empty() {
        return Err(OfficeEnvelopeError::EmptyTitle);
    }
    if bundle.title.len() > OFFICE_CONTRACT_MAX_TITLE_BYTES {
        return Err(OfficeEnvelopeError::TitleTooLong {
            limit: OFFICE_CONTRACT_MAX_TITLE_BYTES,
        });
    }
    if bundle.markup.trim().is_empty() {
        return Err(OfficeEnvelopeError::EmptyMarkup);
    }
    if bundle.markup.len() > OFFICE_BUNDLE_MAX_BYTES {
        return Err(OfficeEnvelopeError::TooLarge {
            limit: OFFICE_BUNDLE_MAX_BYTES,
        });
    }
    Ok(())
}

/// Derive a deterministic, path-safe output part name from a title.
fn output_name(title: &str, target: OfficeFormat) -> String {
    let mut stem: String = title
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() {
                character.to_ascii_lowercase()
            } else {
                '_'
            }
        })
        .take(64)
        .collect();
    while stem.ends_with('_') {
        let _ = stem.pop();
    }
    if stem.is_empty() {
        stem.push_str("document");
    }
    format!("{stem}.{}", target.extension())
}

/// Build a deterministic conversion plan for a caller-supplied Office bundle.
///
/// The bundle envelope is validated first, then the existing importer produces
/// the document and its report. Constructs the importer skipped are exposed as
/// typed [`OfficeUnsupported`] values rather than silently dropped.
///
/// # Errors
///
/// Returns an [`OfficeEnvelopeError`] for an invalid envelope or an importer
/// rejection (including active content, event handlers, and remote
/// references).
pub fn build_office_conversion_plan(
    bundle: &OfficeBundle,
) -> Result<OfficeConversionPlan, OfficeEnvelopeError> {
    validate_office_envelope(bundle)?;
    let imported = import_office_bundle_with_report(bundle)?;
    let target = target_format(bundle.kind);
    let output_name = output_name(&bundle.title, target);
    let unsupported: Vec<OfficeUnsupported> = imported
        .report
        .findings
        .iter()
        .map(OfficeUnsupported::from_finding)
        .collect();
    let kinds: Vec<&str> = unsupported.iter().map(OfficeUnsupported::kind).collect();
    let summary = format!(
        "kind={} target={} output={} pages={} blocks={} unsupported=[{}]",
        bundle.kind.label(),
        target.label(),
        output_name,
        imported.report.page_count,
        imported.report.block_count,
        kinds.join(", ")
    );
    Ok(OfficeConversionPlan {
        target,
        output_name,
        page_count: imported.report.page_count,
        block_count: imported.report.block_count,
        unsupported,
        summary,
    })
}

/// Build a conversion plan, rejecting any construct that had to be skipped.
///
/// # Errors
///
/// Propagates every [`OfficeEnvelopeError`] from
/// [`build_office_conversion_plan`] and additionally returns
/// [`OfficeEnvelopeError::UnsupportedConstruct`] when the plan contains a
/// finding.
pub fn build_office_conversion_plan_strict(
    bundle: &OfficeBundle,
) -> Result<OfficeConversionPlan, OfficeEnvelopeError> {
    let plan = build_office_conversion_plan(bundle)?;
    if let Some(first) = plan.unsupported.first() {
        return Err(OfficeEnvelopeError::UnsupportedConstruct {
            kind: first.kind(),
            detail: format!("output {}: {}", plan.output_name(), first.token()),
        });
    }
    Ok(plan)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn word_bundle(title: &str, markup: &str) -> OfficeBundle {
        OfficeBundle::new(OfficeBundleKind::Word, title, markup)
    }

    #[test]
    fn office_conversion_plan_is_deterministic_and_maps_target_by_kind() {
        let bundle = word_bundle(
            "Quarterly Report",
            "<p>Hello</p><!--pdfxi:page--><p>World</p>",
        );
        let first = build_office_conversion_plan(&bundle);
        let second = build_office_conversion_plan(&bundle);
        assert_eq!(first, second);
        let plan = first.expect("conversion plan");
        assert_eq!(plan.target(), OfficeFormat::Docx);
        assert_eq!(plan.output_name(), "quarterly_report.docx");
        assert_eq!(plan.page_count(), 2);
        assert!(plan.summary().contains("kind=Word"));
        assert!(plan.summary().contains("target=Word"));
        assert_eq!(target_format(OfficeBundleKind::Excel), OfficeFormat::Xlsx);
        assert_eq!(
            target_format(OfficeBundleKind::PowerPoint),
            OfficeFormat::Pptx
        );
    }

    #[test]
    fn office_envelope_validation_rejects_empty_and_oversized_fields() {
        assert_eq!(
            validate_office_envelope(&word_bundle("", "<p>x</p>")),
            Err(OfficeEnvelopeError::EmptyTitle)
        );
        let long_title = "t".repeat(OFFICE_CONTRACT_MAX_TITLE_BYTES + 1);
        assert_eq!(
            validate_office_envelope(&word_bundle(&long_title, "<p>x</p>")),
            Err(OfficeEnvelopeError::TitleTooLong {
                limit: OFFICE_CONTRACT_MAX_TITLE_BYTES
            })
        );
        assert_eq!(
            validate_office_envelope(&word_bundle("title", "  \n ")),
            Err(OfficeEnvelopeError::EmptyMarkup)
        );
        let long_markup = format!("<p>{}</p>", "x".repeat(OFFICE_BUNDLE_MAX_BYTES));
        assert_eq!(
            validate_office_envelope(&word_bundle("title", &long_markup)),
            Err(OfficeEnvelopeError::TooLarge {
                limit: OFFICE_BUNDLE_MAX_BYTES
            })
        );
    }

    #[test]
    fn office_conversion_plan_types_unsupported_constructs() {
        let bundle = word_bundle("Report", "<p>kept</p><img src=\"a.png\"/><widget/>");
        let plan = build_office_conversion_plan(&bundle);
        assert!(plan.is_ok());
        let plan = plan.expect("conversion plan");
        let kinds: Vec<&str> = plan
            .unsupported()
            .iter()
            .map(OfficeUnsupported::kind)
            .collect();
        assert!(kinds.contains(&"media"));
        assert!(kinds.contains(&"unknown-tag"));
        for item in plan.unsupported() {
            assert!(!item.token().is_empty());
        }
    }

    #[test]
    fn strict_office_conversion_promotes_findings_to_typed_error() {
        let clean = word_bundle("Clean", "<p>only text</p>");
        assert!(build_office_conversion_plan_strict(&clean).is_ok());
        let bundle = word_bundle("Report", "<p>kept</p><img src=\"a.png\"/>");
        assert_eq!(
            build_office_conversion_plan_strict(&bundle),
            Err(OfficeEnvelopeError::UnsupportedConstruct {
                kind: "media",
                detail: "output report.docx: img".to_owned(),
            })
        );
    }

    #[test]
    fn active_content_is_reported_as_a_typed_import_error() {
        let bundle = word_bundle("Report", "<script>alert(1)</script>");
        let result = build_office_conversion_plan(&bundle);
        assert!(matches!(
            result,
            Err(OfficeEnvelopeError::Import(
                OfficeImportError::UnsupportedConstruct { .. }
            ))
        ));
    }
}
