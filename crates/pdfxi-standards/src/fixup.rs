//! Opt-in, reported fixups for safe and mechanical preflight problems.
//!
//! A fixup never touches PDF bytes. It edits a cloned [`PreflightInput`]
//! snapshot: a producer adapts the resulting snapshot back into its own object
//! model and is responsible for the byte-level rewrite. Nothing is applied
//! unless the caller explicitly enables it in [`FixupPolicy`], and every applied
//! change is recorded in a [`FixupReport`]. Fixups that change conformance in a
//! way that still needs human judgement (embedding a real font or ICC profile,
//! removing encryption, repairing page geometry) are deliberately not offered.
//!
//! The entry point is [`apply_fixups`]. It validates each request against the
//! policy and the target [`Standard`], applies it to a clone, and returns the
//! corrected snapshot together with an audit report. Invalid or disabled
//! requests return a typed [`FixupError`] rather than being silently skipped.
//! [`suggested_fixups`] derives a conservative, deterministic fixup list from a
//! [`PreflightReport`] so callers can offer "fix what is safe" in one step.

use crate::input::{OutputIntent, PdfVersion, PreflightInput};
use crate::{PreflightReport, Standard};
use std::fmt;

/// A safe, mechanical change that a caller may opt in to.
///
/// Each variant names one narrowly scoped edit. No variant rewrites page
/// content, font programs, or ICC profiles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fixup {
    /// Add an XMP `/Metadata` skeleton to the catalog when none is present.
    ///
    /// The skeleton is a placeholder; a producer must fill in real document
    /// metadata before claiming archival conformance.
    AddXmpMetadata,
    /// Add an output-intent placeholder with the given `/S` subtype.
    ///
    /// Only offered for profiles that use output intents. The placeholder has
    /// no ICC stream, so a real `/DestOutputProfile` must still be supplied.
    SetOutputIntent {
        /// The `/S` subtype name, for example `GTS_PDFA1`.
        subtype: String,
    },
    /// Remove a prohibited catalog or document-level entry by name.
    RemoveProhibitedEntry {
        /// The entry name, for example `/JavaScript`.
        entry: String,
    },
    /// Normalize the `%PDF-x.y` header version to `version`.
    NormalizeVersion {
        /// The replacement version, which must not be below the profile minimum.
        version: PdfVersion,
    },
}

/// Explicit opt-in switches for each fixup family.
///
/// Every field defaults to `false`: a fixup runs only when its switch is set.
/// Use [`FixupPolicy::all`] to enable every family, or
/// [`FixupPolicy::for_output_intent`] for a narrow, single-purpose policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixupPolicy {
    /// Permit adding an XMP metadata skeleton.
    pub add_xmp_metadata: bool,
    /// Permit adding an output-intent placeholder.
    pub set_output_intent: bool,
    /// Permit removing entries the target profile prohibits.
    pub remove_prohibited_entries: bool,
    /// Permit normalizing the header version.
    pub normalize_version: bool,
}

impl FixupPolicy {
    /// Returns a policy with every fixup disabled (the default).
    #[must_use]
    pub const fn none() -> Self {
        Self {
            add_xmp_metadata: false,
            set_output_intent: false,
            remove_prohibited_entries: false,
            normalize_version: false,
        }
    }

    /// Returns a policy with every fixup family enabled.
    #[must_use]
    pub const fn all() -> Self {
        Self {
            add_xmp_metadata: true,
            set_output_intent: true,
            remove_prohibited_entries: true,
            normalize_version: true,
        }
    }

    /// Returns a policy that only permits output-intent placeholders.
    #[must_use]
    pub const fn for_output_intent() -> Self {
        Self {
            add_xmp_metadata: false,
            set_output_intent: true,
            remove_prohibited_entries: false,
            normalize_version: false,
        }
    }

    /// Returns `true` when `policy` permits `fixup` at the family level.
    #[must_use]
    pub fn permits(&self, fixup: &Fixup) -> bool {
        match fixup {
            Fixup::AddXmpMetadata => self.add_xmp_metadata,
            Fixup::SetOutputIntent { .. } => self.set_output_intent,
            Fixup::RemoveProhibitedEntry { .. } => self.remove_prohibited_entries,
            Fixup::NormalizeVersion { .. } => self.normalize_version,
        }
    }
}

impl Default for FixupPolicy {
    fn default() -> Self {
        Self::none()
    }
}

/// A typed reason a requested fixup could not be applied.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FixupError {
    /// The request was valid but its family was not enabled in the policy.
    NotEnabled(Fixup),
    /// The target profile does not use output intents, so a placeholder is not
    /// a meaningful fix.
    OutputIntentNotAllowed(Standard),
    /// The named entry is not prohibited by the target profile; removing it
    /// would change document semantics without a conformance benefit.
    EntryNotProhibited(String),
    /// The named entry is prohibited but is not present in the input.
    EntryNotPresent(String),
    /// Normalizing would set a version below the profile's minimum.
    VersionBelowMinimum {
        /// The requested version.
        version: PdfVersion,
        /// The lowest version the profile accepts.
        minimum: PdfVersion,
    },
    /// The fixup has nothing to change in the supplied input.
    NoApplicableTarget(Fixup),
}

impl fmt::Display for FixupError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::NotEnabled(fixup) => {
                write!(f, "fixup {fixup:?} is not enabled in the active policy")
            }
            Self::OutputIntentNotAllowed(standard) => write!(
                f,
                "{standard} does not use output intents; a placeholder is not applicable"
            ),
            Self::EntryNotProhibited(entry) => {
                write!(f, "entry {entry} is not prohibited by the target profile")
            }
            Self::EntryNotPresent(entry) => {
                write!(f, "prohibited entry {entry} is not present in the input")
            }
            Self::VersionBelowMinimum { version, minimum } => write!(
                f,
                "version {version} is below the profile minimum {minimum}"
            ),
            Self::NoApplicableTarget(fixup) => {
                write!(f, "fixup {fixup:?} has no applicable target")
            }
        }
    }
}

impl std::error::Error for FixupError {}

/// An audit record for one applied fixup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixupRecord {
    fixup: Fixup,
}

impl FixupRecord {
    /// Creates a record for an applied `fixup`.
    #[must_use]
    pub const fn new(fixup: Fixup) -> Self {
        Self { fixup }
    }

    /// Returns the applied fixup.
    #[must_use]
    pub fn fixup(&self) -> &Fixup {
        &self.fixup
    }
}

/// The audit report produced by [`apply_fixups`].
///
/// Records appear in the same order the fixups were requested, which makes the
/// report deterministic and easy to diff.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixupReport {
    standard: Standard,
    records: Vec<FixupRecord>,
}

impl FixupReport {
    /// Builds a report for `standard` with the given applied records.
    #[must_use]
    pub fn new(standard: Standard, records: Vec<FixupRecord>) -> Self {
        Self { standard, records }
    }

    /// Returns the standard the fixups targeted.
    #[must_use]
    pub fn standard(&self) -> Standard {
        self.standard
    }

    /// Returns the applied fixups in request order.
    #[must_use]
    pub fn applied(&self) -> &[FixupRecord] {
        &self.records
    }

    /// Returns the number of applied fixups.
    #[must_use]
    pub fn applied_count(&self) -> usize {
        self.records.len()
    }

    /// Returns `true` when no fixups were applied.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.records.is_empty()
    }

    /// Returns a stable, human-readable tally of applied fixups.
    #[must_use]
    pub fn summary(&self) -> String {
        format!(
            "{}: {} fixup(s) applied",
            self.standard,
            self.applied_count()
        )
    }
}

/// The corrected snapshot and its audit report.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixupOutcome {
    /// The corrected snapshot; the caller's input is never mutated.
    pub input: PreflightInput,
    /// The audit report describing every applied fixup.
    pub report: FixupReport,
}

/// Returns `true` when `standard` is a profile that uses output intents.
///
/// Only those profiles can meaningfully receive an output-intent placeholder.
#[must_use]
pub fn profile_uses_output_intents(standard: Standard) -> bool {
    matches!(
        standard,
        Standard::PdfA1b | Standard::PdfA2b | Standard::PdfA3b | Standard::PdfX1a | Standard::PdfX4
    )
}

/// Returns the output-intent subtype required by `standard`, if any.
#[must_use]
pub fn required_output_subtype(standard: Standard) -> Option<&'static str> {
    match standard {
        Standard::PdfA1b | Standard::PdfA2b | Standard::PdfA3b => Some("GTS_PDFA1"),
        Standard::PdfX1a | Standard::PdfX4 => Some("GTS_PDFX"),
        Standard::PdfE1 | Standard::PdfUa1 => None,
    }
}

/// Returns the catalog entries `standard` explicitly prohibits.
#[must_use]
pub fn prohibited_entries(standard: Standard) -> &'static [&'static str] {
    match standard {
        Standard::PdfA1b | Standard::PdfA2b => &["/EmbeddedFiles", "/JavaScript", "/AA"],
        Standard::PdfA3b => &["/JavaScript", "/AA"],
        Standard::PdfX1a | Standard::PdfX4 => &["/EmbeddedFiles", "/JavaScript", "/AA"],
        Standard::PdfE1 | Standard::PdfUa1 => &[],
    }
}

/// Parses the bundled minimum-version constant into a comparable version.
fn minimum_version(standard: Standard) -> PdfVersion {
    // The bundled constants are known-valid; a future typo degrades to the
    // oldest version rather than panicking.
    PdfVersion::parse(standard.minimum_pdf_version()).unwrap_or_default()
}

/// Applies the requested, policy-enabled fixups to a clone of `input`.
///
/// Each request is checked in order against the policy, the target profile, and
/// the input. The first invalid request returns a typed [`FixupError`]; no
/// partial result is produced. On success the corrected snapshot and a report
/// listing every applied fixup (in request order) are returned.
pub fn apply_fixups(
    standard: Standard,
    input: &PreflightInput,
    policy: &FixupPolicy,
    requested: &[Fixup],
) -> Result<FixupOutcome, FixupError> {
    let mut corrected = input.clone();
    let mut records = Vec::with_capacity(requested.len());

    for fixup in requested {
        if !policy.permits(fixup) {
            return Err(FixupError::NotEnabled(fixup.clone()));
        }
        match fixup {
            Fixup::AddXmpMetadata => {
                if corrected.catalog.has_xmp_metadata {
                    return Err(FixupError::NoApplicableTarget(fixup.clone()));
                }
                corrected.catalog.has_xmp_metadata = true;
            }
            Fixup::SetOutputIntent { subtype } => {
                if !profile_uses_output_intents(standard) {
                    return Err(FixupError::OutputIntentNotAllowed(standard));
                }
                if corrected.output_intent_of(subtype.as_str()).is_some() {
                    return Err(FixupError::NoApplicableTarget(fixup.clone()));
                }
                // A placeholder carries no ICC stream; a producer must supply
                // /DestOutputProfile before the result is conforming.
                corrected
                    .catalog
                    .output_intents
                    .push(OutputIntent::new(subtype.clone(), false));
            }
            Fixup::RemoveProhibitedEntry { entry } => {
                let allowed = prohibited_entries(standard)
                    .iter()
                    .any(|name| name.eq_ignore_ascii_case(entry.as_str()));
                if !allowed {
                    return Err(FixupError::EntryNotProhibited(entry.clone()));
                }
                let before = corrected.catalog.forbidden_entries.len();
                corrected
                    .catalog
                    .forbidden_entries
                    .retain(|name| !name.eq_ignore_ascii_case(entry.as_str()));
                if corrected.catalog.forbidden_entries.len() == before {
                    return Err(FixupError::EntryNotPresent(entry.clone()));
                }
            }
            Fixup::NormalizeVersion { version } => {
                let minimum = minimum_version(standard);
                if *version < minimum {
                    return Err(FixupError::VersionBelowMinimum {
                        version: *version,
                        minimum,
                    });
                }
                if corrected.version == *version {
                    return Err(FixupError::NoApplicableTarget(fixup.clone()));
                }
                corrected.version = *version;
            }
        }
        records.push(FixupRecord::new(fixup.clone()));
    }

    Ok(FixupOutcome {
        input: corrected,
        report: FixupReport::new(standard, records),
    })
}

/// Derives a conservative, deterministic fixup list from a preflight report.
///
/// Only findings with an unambiguous mechanical remedy are mapped:
/// `NO_XMP_METADATA`, `NO_OUTPUT_INTENT`/`OUTPUT_INTENT_SUBTYPE`, and
/// `VERSION_TOO_LOW`. Findings such as encryption, unembedded fonts, and
/// forbidden entries intentionally have no suggestion because repairing them
/// requires judgement or content changes. The result is deduplicated and
/// follows the report's canonical finding order.
#[must_use]
pub fn suggested_fixups(standard: Standard, report: &PreflightReport) -> Vec<Fixup> {
    let mut suggestions = Vec::new();
    let mut add = |fixup: Fixup| {
        if !suggestions.contains(&fixup) {
            suggestions.push(fixup);
        }
    };

    for finding in report.findings() {
        let code = finding.code();
        let suffix = code.rsplit('.').next().unwrap_or(code);
        match suffix {
            "NO_XMP_METADATA" => add(Fixup::AddXmpMetadata),
            "NO_OUTPUT_INTENT" | "OUTPUT_INTENT_SUBTYPE" => {
                if let Some(subtype) = required_output_subtype(standard) {
                    add(Fixup::SetOutputIntent {
                        subtype: subtype.to_owned(),
                    });
                }
            }
            "VERSION_TOO_LOW" => add(Fixup::NormalizeVersion {
                version: minimum_version(standard),
            }),
            _ => {}
        }
    }
    suggestions
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{CatalogInfo, FontInfo, FontType};
    use crate::{PdfA2bProfile, PreflightFinding, PreflightProfile, Severity};

    fn input_with_problems(standard: Standard) -> PreflightInput {
        let version = if standard == Standard::PdfA2b {
            PdfVersion::new(1, 4)
        } else {
            PdfVersion::new(1, 7)
        };
        PreflightInput {
            version,
            catalog: CatalogInfo {
                forbidden_entries: vec!["/JavaScript".to_owned()],
                ..Default::default()
            },
            fonts: vec![FontInfo {
                name: "/F1".to_owned(),
                font_type: FontType::TrueType,
                embedded: true,
            }],
            ..Default::default()
        }
    }

    #[test]
    fn add_xmp_metadata_fixup_is_opt_in_and_reported() {
        let input = input_with_problems(Standard::PdfA2b);
        let fixup = Fixup::AddXmpMetadata;

        let denied = apply_fixups(
            Standard::PdfA2b,
            &input,
            &FixupPolicy::default(),
            std::slice::from_ref(&fixup),
        );
        assert_eq!(denied, Err(FixupError::NotEnabled(fixup.clone())));

        let policy = FixupPolicy {
            add_xmp_metadata: true,
            ..FixupPolicy::default()
        };
        let outcome = apply_fixups(
            Standard::PdfA2b,
            &input,
            &policy,
            std::slice::from_ref(&fixup),
        )
        .expect("test: enabled metadata fixup must succeed");
        assert!(outcome.input.catalog.has_xmp_metadata);
        assert_eq!(outcome.report.applied_count(), 1);
        assert_eq!(outcome.report.applied()[0].fixup(), &fixup);
        assert!(
            !input.catalog.has_xmp_metadata,
            "the source is never mutated"
        );

        let after = PdfA2bProfile::new().report(&outcome.input);
        assert!(
            !after
                .findings()
                .iter()
                .any(|f| f.code() == "PDFA2B.NO_XMP_METADATA")
        );
    }

    #[test]
    fn remove_prohibited_entry_fixup_is_opt_in() {
        let input = input_with_problems(Standard::PdfA2b);
        let fixup = Fixup::RemoveProhibitedEntry {
            entry: "/JavaScript".to_owned(),
        };

        let denied = apply_fixups(
            Standard::PdfA2b,
            &input,
            &FixupPolicy::default(),
            std::slice::from_ref(&fixup),
        );
        assert_eq!(denied, Err(FixupError::NotEnabled(fixup.clone())));

        let policy = FixupPolicy {
            remove_prohibited_entries: true,
            ..FixupPolicy::default()
        };
        let outcome = apply_fixups(
            Standard::PdfA2b,
            &input,
            &policy,
            std::slice::from_ref(&fixup),
        )
        .expect("test: enabled prohibited-entry fixup must succeed");
        assert!(outcome.input.catalog.forbidden_entries.is_empty());
        assert_eq!(outcome.report.applied(), &[FixupRecord::new(fixup)]);
        assert_eq!(
            input.catalog.forbidden_entries,
            vec!["/JavaScript".to_owned()],
            "the source is never mutated"
        );

        let not_prohibited = Fixup::RemoveProhibitedEntry {
            entry: "/MediaBox".to_owned(),
        };
        assert_eq!(
            apply_fixups(Standard::PdfA2b, &input, &policy, &[not_prohibited]),
            Err(FixupError::EntryNotProhibited("/MediaBox".to_owned()))
        );

        let mut absent = input.clone();
        absent.catalog.forbidden_entries.clear();
        assert_eq!(
            apply_fixups(
                Standard::PdfA2b,
                &absent,
                &policy,
                &[Fixup::RemoveProhibitedEntry {
                    entry: "/JavaScript".to_owned(),
                }]
            ),
            Err(FixupError::EntryNotPresent("/JavaScript".to_owned()))
        );
    }

    #[test]
    fn output_intent_placeholder_respects_profile_and_policy() {
        let input = input_with_problems(Standard::PdfA1b);
        let fixup = Fixup::SetOutputIntent {
            subtype: "GTS_PDFA1".to_owned(),
        };

        assert_eq!(
            apply_fixups(
                Standard::PdfA1b,
                &input,
                &FixupPolicy::default(),
                std::slice::from_ref(&fixup)
            ),
            Err(FixupError::NotEnabled(fixup.clone()))
        );

        // PDF/E-1 does not use output intents, even when the switch is on.
        let e_input = input_with_problems(Standard::PdfE1);
        assert_eq!(
            apply_fixups(
                Standard::PdfE1,
                &e_input,
                &FixupPolicy::for_output_intent(),
                std::slice::from_ref(&fixup)
            ),
            Err(FixupError::OutputIntentNotAllowed(Standard::PdfE1))
        );

        let outcome = apply_fixups(
            Standard::PdfA1b,
            &input,
            &FixupPolicy::for_output_intent(),
            std::slice::from_ref(&fixup),
        )
        .expect("test: PDF/A-1b allows an output-intent placeholder");
        let intent = outcome
            .input
            .output_intent_of("GTS_PDFA1")
            .expect("test: the placeholder is present");
        assert!(!intent.has_dest_output_profile);
        assert_eq!(outcome.report.applied()[0].fixup(), &fixup);
    }

    #[test]
    fn normalize_version_fixup_enforces_minimum_and_policy() {
        let input = input_with_problems(Standard::PdfA2b); // header 1.4
        let raise = Fixup::NormalizeVersion {
            version: PdfVersion::new(1, 7),
        };
        let policy = FixupPolicy {
            normalize_version: true,
            ..FixupPolicy::default()
        };
        let outcome = apply_fixups(
            Standard::PdfA2b,
            &input,
            &policy,
            std::slice::from_ref(&raise),
        )
        .expect("test: raising the version is allowed");
        assert_eq!(outcome.input.version, PdfVersion::new(1, 7));

        assert_eq!(
            apply_fixups(
                Standard::PdfA2b,
                &input,
                &FixupPolicy::default(),
                std::slice::from_ref(&raise)
            ),
            Err(FixupError::NotEnabled(raise.clone()))
        );
        assert_eq!(
            apply_fixups(Standard::PdfA2b, &outcome.input, &policy, &[raise]),
            Err(FixupError::NoApplicableTarget(Fixup::NormalizeVersion {
                version: PdfVersion::new(1, 7),
            }))
        );
        let downgrade = Fixup::NormalizeVersion {
            version: PdfVersion::new(1, 0),
        };
        assert_eq!(
            apply_fixups(Standard::PdfA2b, &input, &policy, &[downgrade]),
            Err(FixupError::VersionBelowMinimum {
                version: PdfVersion::new(1, 0),
                minimum: PdfVersion::new(1, 7),
            })
        );
    }

    #[test]
    fn fixup_report_is_accurate() {
        let input = input_with_problems(Standard::PdfA2b);
        let requested = vec![
            Fixup::AddXmpMetadata,
            Fixup::RemoveProhibitedEntry {
                entry: "/JavaScript".to_owned(),
            },
            Fixup::NormalizeVersion {
                version: PdfVersion::new(1, 7),
            },
        ];
        let outcome = apply_fixups(Standard::PdfA2b, &input, &FixupPolicy::all(), &requested)
            .expect("test: all requested fixups are valid");
        assert_eq!(
            outcome.report.applied(),
            &[
                FixupRecord::new(requested[0].clone()),
                FixupRecord::new(requested[1].clone()),
                FixupRecord::new(requested[2].clone()),
            ]
        );
        assert_eq!(outcome.report.applied_count(), 3);
        assert!(!outcome.report.is_empty());
        assert_eq!(outcome.report.standard(), Standard::PdfA2b);
        assert_eq!(outcome.report.summary(), "PDF/A-2b: 3 fixup(s) applied");

        // The report matches the actual snapshot differences exactly.
        assert!(outcome.input.catalog.has_xmp_metadata);
        assert!(outcome.input.catalog.forbidden_entries.is_empty());
        assert_eq!(outcome.input.version, PdfVersion::new(1, 7));
        assert_eq!(outcome.input.fonts, input.fonts);
        assert_eq!(
            outcome.input.catalog.output_intents,
            input.catalog.output_intents
        );
    }

    #[test]
    fn suggested_fixups_resolve_only_mechanical_findings() {
        let input = input_with_problems(Standard::PdfA2b);
        let before = PdfA2bProfile::new().report(&input);
        assert!(!before.is_conforming());

        let suggestions = suggested_fixups(Standard::PdfA2b, &before);
        // Canonical finding order is by ascending code, so suggestions follow
        // NO_OUTPUT_INTENT -> NO_XMP_METADATA -> VERSION_TOO_LOW.
        let expected = vec![
            Fixup::SetOutputIntent {
                subtype: "GTS_PDFA1".to_owned(),
            },
            Fixup::AddXmpMetadata,
            Fixup::NormalizeVersion {
                version: PdfVersion::new(1, 7),
            },
        ];
        assert_eq!(suggestions, expected);
        assert_eq!(suggestions, suggested_fixups(Standard::PdfA2b, &before));

        let outcome = apply_fixups(Standard::PdfA2b, &input, &FixupPolicy::all(), &suggestions)
            .expect("test: suggested fixups are valid");
        let after = PdfA2bProfile::new().report(&outcome.input);
        assert!(
            after.error_count() < before.error_count(),
            "fixups must reduce errors: before={}, after={}",
            before.error_count(),
            after.error_count()
        );
        assert!(
            after.error_count() > 0,
            "a real ICC profile is still required"
        );
        assert!(
            after
                .findings()
                .iter()
                .any(|f| f.code() == "PDFA2B.OUTPUT_INTENT_NO_PROFILE")
        );
        // Encryption, fonts and forbidden entries are never auto-suggested.
        assert!(
            !suggestions
                .iter()
                .any(|fixup| matches!(fixup, Fixup::RemoveProhibitedEntry { .. }))
        );
    }

    #[test]
    fn typed_errors_are_actionable_and_distinct() {
        let not_enabled = FixupError::NotEnabled(Fixup::AddXmpMetadata);
        assert!(not_enabled.to_string().contains("not enabled"));
        let not_allowed = FixupError::OutputIntentNotAllowed(Standard::PdfE1);
        assert!(not_allowed.to_string().contains("PDF/E-1"));
        let below = FixupError::VersionBelowMinimum {
            version: PdfVersion::new(1, 0),
            minimum: PdfVersion::new(1, 7),
        };
        assert_eq!(
            below.to_string(),
            "version 1.0 is below the profile minimum 1.7"
        );
        assert_ne!(not_enabled, not_allowed);
        assert_ne!(not_allowed, below);
        // std::error::Error is implemented for callers using `?` at boundaries.
        let _: &dyn std::error::Error = &below;
    }

    #[test]
    fn empty_request_produces_an_empty_report() {
        let input = input_with_problems(Standard::PdfA1b);
        let outcome = apply_fixups(Standard::PdfA1b, &input, &FixupPolicy::all(), &[])
            .expect("test: an empty request succeeds");
        assert!(outcome.report.is_empty());
        assert_eq!(outcome.report.summary(), "PDF/A-1b: 0 fixup(s) applied");
        assert_eq!(outcome.input, input);
    }

    #[test]
    fn prohibited_entry_helper_matches_profile_rules() {
        assert!(prohibited_entries(Standard::PdfA2b).contains(&"/EmbeddedFiles"));
        assert!(!prohibited_entries(Standard::PdfA3b).contains(&"/EmbeddedFiles"));
        assert!(profile_uses_output_intents(Standard::PdfX4));
        assert!(!profile_uses_output_intents(Standard::PdfUa1));
        assert_eq!(required_output_subtype(Standard::PdfX1a), Some("GTS_PDFX"));
        assert_eq!(required_output_subtype(Standard::PdfE1), None);
    }

    #[test]
    fn already_present_xmp_is_not_silently_readded() {
        let mut input = input_with_problems(Standard::PdfA1b);
        input.catalog.has_xmp_metadata = true;
        assert_eq!(
            apply_fixups(
                Standard::PdfA1b,
                &input,
                &FixupPolicy::all(),
                &[Fixup::AddXmpMetadata]
            ),
            Err(FixupError::NoApplicableTarget(Fixup::AddXmpMetadata))
        );
    }

    #[test]
    fn suggested_fixups_ignore_warning_and_info_findings() {
        let finding = PreflightFinding::new(Severity::Info, "PDFA1B.NOTE", "advisory only");
        let report = crate::PreflightReport::new(Standard::PdfA1b, vec![finding]);
        assert!(suggested_fixups(Standard::PdfA1b, &report).is_empty());
    }
}
