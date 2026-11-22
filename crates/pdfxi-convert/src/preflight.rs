//! Byte-level preflight adapter: run `pdfxi-standards` profiles over real PDF bytes.
//!
//! `pdfxi-standards` deliberately stays dependency-free: its
//! [`pdfxi_standards::PreflightInput`] is a plain-data snapshot that a caller
//! fills from a parsed document. This module is that caller. It loads PDF bytes
//! through [`pdfxi_core::Document`], derives the facts the profiles can check,
//! runs the selected deterministic profile, and â€” when explicitly requested â€”
//! applies the opt-in, byte-expressible fixups through the existing
//! [`crate::conformance`] normalizer.
//!
//! # Scope: subset preflight, not certification
//!
//! This is a deterministic *subset* adapter. The snapshot carries version,
//! catalog metadata (XMP, output intents, prohibited entries, tagging flags),
//! page-tree counts and per-page box **presence** flags. Font programs, ICC
//! payloads, colour spaces, transparency and annotation appearance streams are
//! not derived from bytes here, so the corresponding profile rules have nothing
//! to fire on; they remain reported-only constraints. [`BytePreflightReport`]
//! states exactly which checks were enforced in emitted bytes and which are
//! reported only, and [`BytePreflightReport::certification_claimed`] is always
//! `false`. A passing report means "no decidable rule in this subset fired",
//! not "certified conforming"; use veraPDF as an independent CI oracle before
//! making a conformance claim.
//!
//! The `--fix` path reuses [`crate::conformance::export_conformance_pdf`], so it
//! can raise the minimum version, add an XMP placeholder and add an
//! output-intent placeholder with the target `/S` subtype. Those streams are
//! placeholders and must be replaced with real metadata and a valid ICC
//! profile. No other fixup family (for example removing a prohibited entry) is
//! expressible by byte normalization in this subset.

use pdfxi_core::{BoxKind, Document, Object};
use pdfxi_standards::PageBoxes as StandardsPageBoxes;
use pdfxi_standards::{
    CatalogInfo, ConformanceProfile, Fixup, FixupPolicy, FixupReport, OutputIntent, PageInfo,
    PdfVersion, PreflightInput, Standard,
};
use thiserror::Error;

/// Re-exported so downstream adapters can name the report type without taking
/// a direct dependency on `pdfxi-standards`.
pub use pdfxi_standards::PreflightReport;

use crate::conformance::{
    ConformanceRequest, ConformanceTarget, NormalizationOptions, REPORTED_ONLY_CONSTRAINTS,
    export_conformance_pdf,
};

/// Constraints this adapter reports but never enforces in emitted bytes.
///
/// These are the same stable identifiers used by
/// [`crate::conformance::REPORTED_ONLY_CONSTRAINTS`], re-exported here under a
/// preflight-oriented name so callers do not have to reach into another module.
pub const REPORTED_ONLY_CHECKS: &[&str] = REPORTED_ONLY_CONSTRAINTS;

/// A short, stable statement of this adapter's scope.
pub const BYTE_PREFLIGHT_SCOPE_NOTE: &str = "Subset byte-level preflight: deterministic \
checks run over facts derived from real PDF bytes. --fix applies only opt-in, mechanical \
normalizations (minimum version, XMP presence, output-intent presence/subtype). XMP and ICC \
output-intent streams added by --fix are placeholders that must be replaced; all other \
constraints are reported only and no conformance certification is claimed.";

/// Opt-in switches for a byte-level preflight run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PreflightOptions {
    /// Apply opt-in, byte-expressible fixups and return normalized bytes.
    ///
    /// The snapshot fixups are still validated and audited through
    /// [`pdfxi_standards::apply_fixups`], but the bytes are produced by the
    /// conformance normalizer so they can be reparsed and inspected.
    pub fix: bool,
}

/// A typed reason a byte-level preflight run could not complete.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub enum BytePreflightError {
    /// The profile label does not name a standard this adapter can check.
    #[error("unsupported preflight profile '{label}'")]
    UnsupportedProfile {
        /// The label as supplied by the caller.
        label: String,
    },
    /// The input does not have a parseable `%PDF-` header.
    #[error("malformed PDF: {reason}")]
    MalformedPdf {
        /// A human-readable explanation of the parse failure.
        reason: String,
    },
    /// The catalog could not be read as a dictionary.
    #[error("cannot read document catalog: {reason}")]
    Catalog {
        /// A human-readable explanation.
        reason: String,
    },
    /// A page's box data could not be resolved.
    #[error("page {index} has invalid boxes: {reason}")]
    PageBoxes {
        /// Zero-based page index that failed.
        index: usize,
        /// A human-readable explanation.
        reason: String,
    },
    /// The requested profile has no byte-expressible fixups in this subset.
    #[error("profile '{label}' has no byte-level fixups in this subset")]
    FixupsUnavailable {
        /// The profile label supplied by the caller.
        label: String,
    },
    /// A fixup request or byte normalization failed.
    #[error("fixup application failed: {reason}")]
    Fixup {
        /// A human-readable explanation.
        reason: String,
    },
    /// Normalized bytes failed to reparse or lost structural information.
    #[error("normalized bytes failed to reparse: {reason}")]
    Reparse {
        /// A human-readable explanation.
        reason: String,
    },
}

/// The result of one byte-level preflight run.
///
/// The report is deterministic for a given input, profile label and options.
/// It always carries the before-fix report; with [`PreflightOptions::fix`] it
/// also carries the after-fix report, the audited fixups, the normalized bytes
/// and the byte-level constraints that were enforced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BytePreflightReport {
    standard: Standard,
    before: PreflightReport,
    after: Option<PreflightReport>,
    fixups: FixupReport,
    output_bytes: Option<Vec<u8>>,
    enforced: Vec<&'static str>,
    reported_only: Vec<&'static str>,
}

impl BytePreflightReport {
    /// Returns the standard that was checked.
    #[must_use]
    pub const fn standard(&self) -> Standard {
        self.standard
    }

    /// Returns the canonical profile label, for example `PDF/A-2b`.
    #[must_use]
    pub fn profile_label(&self) -> &'static str {
        self.standard.label()
    }

    /// Returns the pre-fix report.
    #[must_use]
    pub const fn before(&self) -> &PreflightReport {
        &self.before
    }

    /// Returns the post-fix report, when fixups were requested.
    #[must_use]
    pub const fn after(&self) -> Option<&PreflightReport> {
        self.after.as_ref()
    }

    /// Returns the report that describes the final state of the bytes.
    ///
    /// This is the post-fix report when fixups were applied, otherwise the
    /// pre-fix report.
    #[must_use]
    pub fn final_report(&self) -> &PreflightReport {
        self.after.as_ref().unwrap_or(&self.before)
    }

    /// Returns the audited fixups that were applied.
    #[must_use]
    pub const fn fixups(&self) -> &FixupReport {
        &self.fixups
    }

    /// Returns the normalized bytes, when fixups were requested.
    #[must_use]
    pub fn output_bytes(&self) -> Option<&[u8]> {
        self.output_bytes.as_deref()
    }

    /// Returns the byte-level constraints enforced in the emitted bytes.
    ///
    /// Empty when no fixups were requested: a check-only run changes no bytes.
    #[must_use]
    pub fn enforced(&self) -> &[&'static str] {
        &self.enforced
    }

    /// Returns the constraints that are reported but never enforced.
    #[must_use]
    pub fn reported_only(&self) -> &[&'static str] {
        &self.reported_only
    }

    /// Returns `true` when the final report has no error-severity findings.
    #[must_use]
    pub fn is_conforming(&self) -> bool {
        self.final_report().is_conforming()
    }

    /// Always `false`: this adapter does not certify conformance.
    #[must_use]
    pub const fn certification_claimed(&self) -> bool {
        false
    }

    /// Returns the stable scope statement.
    #[must_use]
    pub const fn scope_note(&self) -> &'static str {
        BYTE_PREFLIGHT_SCOPE_NOTE
    }

    /// Returns a stable, human-readable summary.
    #[must_use]
    pub fn summary(&self) -> String {
        let verdict = if self.is_conforming() {
            "conforming"
        } else {
            "non-conforming"
        };
        format!(
            "{}: {} (before={}, after={}, enforced={}, reported-only={}, certification=false)",
            self.standard,
            verdict,
            self.before.summary(),
            self.after
                .as_ref()
                .map_or_else(|| "none".to_owned(), PreflightReport::summary),
            self.enforced.len(),
            self.reported_only.len()
        )
    }
}

/// Resolve a value that may be an indirect reference to an owned object.
fn resolve(doc: &Document, value: &Object) -> Result<Object, BytePreflightError> {
    match value {
        Object::Reference(id) => doc
            .object(*id)
            .map_err(|error| BytePreflightError::Catalog {
                reason: error.to_string(),
            }),
        other => Ok(other.clone()),
    }
}

/// Read the catalog facts that document profiles inspect.
fn read_catalog(doc: &Document) -> Result<CatalogInfo, BytePreflightError> {
    let root = doc.root().map_err(|error| BytePreflightError::Catalog {
        reason: error.to_string(),
    })?;
    let object = doc
        .object(root)
        .map_err(|error| BytePreflightError::Catalog {
            reason: error.to_string(),
        })?;
    let dict = object
        .as_dictionary()
        .ok_or_else(|| BytePreflightError::Catalog {
            reason: "catalog object is not a dictionary".to_owned(),
        })?;

    let mut info = CatalogInfo {
        has_xmp_metadata: dict.contains_key("Metadata"),
        ..Default::default()
    };

    if let Some(value) = dict.get("OutputIntents") {
        let resolved = resolve(doc, value)?;
        if let Some(items) = resolved.as_array() {
            for item in items {
                let intent = resolve(doc, item)?;
                if let Some(intent_dict) = intent.as_dictionary() {
                    let subtype = intent_dict
                        .get("S")
                        .and_then(Object::as_name)
                        .unwrap_or_default()
                        .to_owned();
                    let has_profile = intent_dict.contains_key("DestOutputProfile");
                    info.output_intents
                        .push(OutputIntent::new(subtype, has_profile));
                }
            }
        }
    }

    if let Some(value) = dict.get("MarkInfo") {
        info.has_mark_info = true;
        let mark_info = resolve(doc, value)?;
        info.is_marked = mark_info
            .as_dictionary()
            .and_then(|dict| dict.get("Marked"))
            == Some(&Object::Boolean(true));
    }
    info.has_lang = dict.contains_key("Lang");
    info.has_struct_tree_root = dict.contains_key("StructTreeRoot");
    info.has_acro_form = dict.contains_key("AcroForm");
    info.has_dpart_root = dict.contains_key("DPartRoot");

    // These are the catalog/document entries forbidden by at least one of the
    // supported profiles. A profile only flags the entries it forbids, so
    // listing every present candidate is correct for every standard.
    for key in ["/EmbeddedFiles", "/JavaScript", "/AA"] {
        if dict.contains_key(&key[1..]) {
            info.forbidden_entries.push(key.to_owned());
        }
    }
    if let Some(names) = dict.get("Names").and_then(|value| resolve(doc, value).ok())
        && names
            .as_dictionary()
            .is_some_and(|dict| dict.contains_key("EmbeddedFiles"))
    {
        info.forbidden_entries.push("/EmbeddedFiles".to_owned());
    }

    Ok(info)
}

/// Best-effort encryption detection from raw bytes.
///
/// The core handle does not expose trailer keys, so this adapter checks for an
/// `/Encrypt` token in the source bytes. It is intentionally conservative: a
/// false positive yields an `ENCRYPTED` finding, never silent acceptance of an
/// encrypted file.
fn detect_encryption(doc: &Document) -> bool {
    doc.bytes()
        .windows(b"/Encrypt".len())
        .any(|window| window == &b"/Encrypt"[..])
}

/// Build a [`PreflightInput`] snapshot from a parsed document.
///
/// The snapshot carries the facts the profiles can decide from bytes. Fonts,
/// images, colour spaces, transparency and annotations are intentionally left
/// empty because deriving them would require a content-stream walker and a
/// rendering oracle; those checks remain reported-only (see the module docs).
///
/// # Errors
///
/// Returns a typed error when the catalog or page tree cannot be read.
pub fn build_preflight_input(doc: &Document) -> Result<PreflightInput, BytePreflightError> {
    let version = PdfVersion::parse(doc.version()).unwrap_or_default();
    let page_count = doc
        .page_count()
        .map_err(|error| BytePreflightError::Catalog {
            reason: error.to_string(),
        })?;
    let mut pages = Vec::with_capacity(page_count);
    for index in 0..page_count {
        let boxes = doc
            .page_boxes(index)
            .map_err(|error| BytePreflightError::PageBoxes {
                index,
                reason: error.to_string(),
            })?;
        let page = doc
            .page(index)
            .map_err(|error| BytePreflightError::PageBoxes {
                index,
                reason: error.to_string(),
            })?;
        let has_dpart = match doc.object(page.id) {
            Ok(object) => object
                .as_dictionary()
                .is_some_and(|dictionary| dictionary.contains_key("DPart")),
            Err(error) => {
                return Err(BytePreflightError::PageBoxes {
                    index,
                    reason: error.to_string(),
                });
            }
        };
        pages.push(PageInfo {
            number: index + 1,
            boxes: StandardsPageBoxes {
                has_media_box: boxes.is_explicit(BoxKind::Media),
                has_crop_box: boxes.is_explicit(BoxKind::Crop),
                has_bleed_box: boxes.is_explicit(BoxKind::Bleed),
                has_trim_box: boxes.is_explicit(BoxKind::Trim),
                has_art_box: boxes.is_explicit(BoxKind::Art),
            },
            has_dpart,
        });
    }
    Ok(PreflightInput {
        version,
        is_encrypted: detect_encryption(doc),
        catalog: read_catalog(doc)?,
        page_count,
        pages,
        ..Default::default()
    })
}

/// Run a preflight profile over `bytes` and return a deterministic typed report.
///
/// `profile_label` is resolved with [`pdfxi_standards::ConformanceProfile::from_label`],
/// so ASCII case is ignored. When [`PreflightOptions::fix`] is set, only the
/// byte-expressible suggestions (minimum version, XMP presence, output-intent
/// presence/subtype) are applied; the normalized bytes are reparsed to prove
/// they are a valid PDF and the profile is run again for the after-report.
///
/// # Errors
///
/// Returns a typed error for an unknown profile, malformed input, unreadable
/// page geometry, an unavailable or invalid fixup, or normalized bytes that
/// fail to reparse.
pub fn preflight_bytes(
    bytes: &[u8],
    profile_label: &str,
    options: &PreflightOptions,
) -> Result<BytePreflightReport, BytePreflightError> {
    let profile = ConformanceProfile::from_label(profile_label).map_err(|_| {
        BytePreflightError::UnsupportedProfile {
            label: profile_label.to_owned(),
        }
    })?;
    let standard = profile.standard();

    let doc =
        Document::from_bytes(bytes.to_vec()).map_err(|error| BytePreflightError::MalformedPdf {
            reason: error.to_string(),
        })?;
    let input = build_preflight_input(&doc)?;
    let before = profile.report(&input);

    let mut after = None;
    let mut fixups = FixupReport::new(standard, Vec::new());
    let mut output_bytes = None;
    let mut enforced = Vec::new();

    if options.fix {
        let expressible: Vec<Fixup> = profile
            .suggestions(&before)
            .into_iter()
            .filter(|fixup| {
                matches!(
                    fixup,
                    Fixup::NormalizeVersion { .. }
                        | Fixup::AddXmpMetadata
                        | Fixup::SetOutputIntent { .. }
                )
            })
            .collect();
        fixups = apply_fixups_snapshot(standard, &input, &expressible)?;

        // PDF/UA-1 has no conformance-export target, so no byte-level fixups
        // are available for it in this subset.
        if ConformanceTarget::from_label(standard.label()).is_none() {
            return Err(BytePreflightError::FixupsUnavailable {
                label: profile_label.to_owned(),
            });
        }
        let export = export_conformance_pdf(
            bytes,
            &ConformanceRequest::new(standard.label()),
            &NormalizationOptions::all(),
        )
        .map_err(|error| BytePreflightError::Fixup {
            reason: error.to_string(),
        })?;
        enforced = export.report.enforced().to_vec();
        let normalized = export.bytes;

        let reparsed = Document::from_bytes(normalized.clone()).map_err(|error| {
            BytePreflightError::Reparse {
                reason: error.to_string(),
            }
        })?;
        let reparsed_count =
            reparsed
                .page_count()
                .map_err(|error| BytePreflightError::Reparse {
                    reason: error.to_string(),
                })?;
        if reparsed_count != input.page_count {
            return Err(BytePreflightError::Reparse {
                reason: format!(
                    "page count changed from {} to {} during normalization",
                    input.page_count, reparsed_count
                ),
            });
        }
        let after_input = build_preflight_input(&reparsed)?;
        after = Some(profile.report(&after_input));
        output_bytes = Some(normalized);
    }

    Ok(BytePreflightReport {
        standard,
        before,
        after,
        fixups,
        output_bytes,
        enforced,
        reported_only: REPORTED_ONLY_CHECKS.to_vec(),
    })
}

/// Apply the validated fixup suggestions to a snapshot and return the audit.
fn apply_fixups_snapshot(
    standard: Standard,
    input: &PreflightInput,
    requested: &[Fixup],
) -> Result<FixupReport, BytePreflightError> {
    pdfxi_standards::apply_fixups(standard, input, &FixupPolicy::all(), requested)
        .map(|outcome| outcome.report)
        .map_err(|error| BytePreflightError::Fixup {
            reason: error.to_string(),
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scope_constants_are_stable() {
        assert!(!REPORTED_ONLY_CHECKS.is_empty());
        assert!(REPORTED_ONLY_CHECKS.contains(&"icc_profile_validity"));
        assert!(BYTE_PREFLIGHT_SCOPE_NOTE.contains("no conformance certification"));
    }

    #[test]
    fn options_default_to_check_only() {
        assert_eq!(PreflightOptions::default(), PreflightOptions { fix: false });
    }

    #[test]
    fn unknown_profile_and_malformed_input_are_typed_errors() {
        assert_eq!(
            preflight_bytes(b"%PDF-1.7", "PDF/A-9c", &PreflightOptions::default()),
            Err(BytePreflightError::UnsupportedProfile {
                label: "PDF/A-9c".to_owned()
            })
        );
        assert_eq!(
            preflight_bytes(b"not a pdf", "PDF/A-2b", &PreflightOptions::default()),
            Err(BytePreflightError::MalformedPdf {
                reason: "invalid PDF header; expected %PDF-".to_owned()
            })
        );
    }
}
