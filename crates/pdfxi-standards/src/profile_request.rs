//! Typed dispatch of conformance profile requests.
//!
//! The individual profiles in [`crate::document_profile`] are separate types.
//! Callers that accept a profile name from configuration or a command line need
//! one place that (a) resolves a label to a concrete profile, and (b) rejects
//! labels and requirement combinations that cannot be honoured. This module
//! provides that dispatch with two typed error cases:
//!
//! - [`ProfileRequestError::UnsupportedProfile`] for an unknown label;
//! - [`ProfileRequestError::ContradictoryRequirements`] for a request that asks
//!   a profile to allow something its rules forbid.
//!
//! Dispatch is dependency-free and never inspects PDF bytes. It is deliberately
//! separate from [`crate::PreflightInput`] checking: resolving a profile only
//! decides *which* deterministic checker to run.

use crate::finding::PreflightFinding;
use crate::fixup::{Fixup, suggested_fixups};
use crate::{
    PdfA1bProfile, PdfA2bProfile, PdfA3bProfile, PdfE1Profile, PdfUa1Profile, PdfVt1Profile,
    PdfX1aProfile, PdfX4Profile, PreflightInput, PreflightProfile, PreflightReport, Standard,
};
use std::fmt;

/// A typed reason a conformance profile request could not be resolved.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProfileRequestError {
    /// The label does not name a standard this crate can check.
    UnsupportedProfile {
        /// The label as supplied by the caller.
        label: String,
    },
    /// The request combines requirements that the target profile forbids.
    ContradictoryRequirements {
        /// The standard that was requested.
        standard: Standard,
        /// A human-readable explanation of the conflict.
        reason: String,
    },
}

impl fmt::Display for ProfileRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnsupportedProfile { label } => {
                write!(f, "unsupported conformance profile '{label}'")
            }
            Self::ContradictoryRequirements { standard, reason } => {
                write!(f, "contradictory requirements for {standard}: {reason}")
            }
        }
    }
}

impl std::error::Error for ProfileRequestError {}

/// A caller-supplied request for one conformance profile.
///
/// The `label` is matched with [`Standard::from_label`], so ASCII case is
/// ignored. `allow_transparency` is an optional extra requirement: when it is
/// `Some(true)` for a profile that forbids transparency, resolution returns
/// [`ProfileRequestError::ContradictoryRequirements`] instead of silently
/// producing a checker that will always fail that requirement.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProfileRequest {
    label: String,
    allow_transparency: Option<bool>,
}

impl ProfileRequest {
    /// Creates a request for the profile named by `label`.
    #[must_use]
    pub fn new(label: impl Into<String>) -> Self {
        Self {
            label: label.into(),
            allow_transparency: None,
        }
    }

    /// Adds an explicit transparency requirement to the request.
    #[must_use]
    pub const fn with_allow_transparency(mut self, allow: bool) -> Self {
        self.allow_transparency = Some(allow);
        self
    }

    /// Returns the requested label.
    #[must_use]
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Returns the explicit transparency requirement, if one was supplied.
    #[must_use]
    pub const fn allow_transparency(&self) -> Option<bool> {
        self.allow_transparency
    }

    /// Resolves the request to a concrete [`ConformanceProfile`].
    pub fn resolve(&self) -> Result<ConformanceProfile, ProfileRequestError> {
        let standard = Standard::from_label(&self.label).ok_or_else(|| {
            ProfileRequestError::UnsupportedProfile {
                label: self.label.clone(),
            }
        })?;
        if self.allow_transparency == Some(true)
            && crate::document_profile::forbids_transparency(standard)
        {
            return Err(ProfileRequestError::ContradictoryRequirements {
                standard,
                reason: "transparency is forbidden by this profile".to_owned(),
            });
        }
        ConformanceProfile::from_standard(standard)
    }
}

/// A uniform handle for one of the concrete preflight profiles.
///
/// This enum lets callers dispatch a resolved request without trait objects.
/// Each variant delegates to the same profile implementation used by direct
/// callers, so findings and reports are identical.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConformanceProfile {
    /// ISO 19005-1 Level B.
    PdfA1b,
    /// ISO 19005-2 Level B.
    PdfA2b,
    /// ISO 19005-3 Level B.
    PdfA3b,
    /// ISO 14739-1.
    PdfE1,
    /// ISO 15930-1 Level A.
    PdfX1a,
    /// ISO 15930-7.
    PdfX4,
    /// ISO 14289-1.
    PdfUa1,
    /// ISO 16612-2 (PDF/VT-1).
    PdfVt1,
}

impl ConformanceProfile {
    /// Maps a [`Standard`] to its concrete profile.
    pub fn from_standard(standard: Standard) -> Result<Self, ProfileRequestError> {
        match standard {
            Standard::PdfA1b => Ok(Self::PdfA1b),
            Standard::PdfA2b => Ok(Self::PdfA2b),
            Standard::PdfA3b => Ok(Self::PdfA3b),
            Standard::PdfE1 => Ok(Self::PdfE1),
            Standard::PdfX1a => Ok(Self::PdfX1a),
            Standard::PdfX4 => Ok(Self::PdfX4),
            Standard::PdfUa1 => Ok(Self::PdfUa1),
            Standard::PdfVt1 => Ok(Self::PdfVt1),
        }
    }

    /// Resolves a label through [`ProfileRequest::resolve`].
    pub fn from_label(label: &str) -> Result<Self, ProfileRequestError> {
        ProfileRequest::new(label).resolve()
    }

    /// Returns the standard this profile checks.
    #[must_use]
    pub const fn standard(self) -> Standard {
        match self {
            Self::PdfA1b => Standard::PdfA1b,
            Self::PdfA2b => Standard::PdfA2b,
            Self::PdfA3b => Standard::PdfA3b,
            Self::PdfE1 => Standard::PdfE1,
            Self::PdfX1a => Standard::PdfX1a,
            Self::PdfX4 => Standard::PdfX4,
            Self::PdfUa1 => Standard::PdfUa1,
            Self::PdfVt1 => Standard::PdfVt1,
        }
    }

    /// Runs the concrete profile and returns findings in discovery order.
    #[must_use]
    pub fn check(self, input: &PreflightInput) -> Vec<PreflightFinding> {
        match self {
            Self::PdfA1b => PdfA1bProfile::new().check(input),
            Self::PdfA2b => PdfA2bProfile::new().check(input),
            Self::PdfA3b => PdfA3bProfile::new().check(input),
            Self::PdfE1 => PdfE1Profile::new().check(input),
            Self::PdfX1a => PdfX1aProfile::new().check(input),
            Self::PdfX4 => PdfX4Profile::new().check(input),
            Self::PdfUa1 => PdfUa1Profile::new().check(input),
            Self::PdfVt1 => PdfVt1Profile::new().check(input),
        }
    }

    /// Runs the concrete profile and returns a canonical report.
    #[must_use]
    pub fn report(self, input: &PreflightInput) -> PreflightReport {
        match self {
            Self::PdfA1b => PdfA1bProfile::new().report(input),
            Self::PdfA2b => PdfA2bProfile::new().report(input),
            Self::PdfA3b => PdfA3bProfile::new().report(input),
            Self::PdfE1 => PdfE1Profile::new().report(input),
            Self::PdfX1a => PdfX1aProfile::new().report(input),
            Self::PdfX4 => PdfX4Profile::new().report(input),
            Self::PdfUa1 => PdfUa1Profile::new().report(input),
            Self::PdfVt1 => PdfVt1Profile::new().report(input),
        }
    }

    /// Returns conservative, deterministic fixup suggestions for a report.
    ///
    /// Suggestions are values only; applying them is always opt-in and never
    /// mutates the caller's snapshot (see [`crate::apply_fixups`]).
    #[must_use]
    pub fn suggestions(self, report: &PreflightReport) -> Vec<Fixup> {
        suggested_fixups(self.standard(), report)
    }
}

#[test]
fn pdf_vt_1_resolves_to_its_profile() {
    let profile = ConformanceProfile::from_label("PDF/VT-1").expect("known label");
    assert_eq!(profile, ConformanceProfile::PdfVt1);
    assert_eq!(profile.standard(), Standard::PdfVt1);
    assert!(ConformanceProfile::from_label("PDF/VT-2").is_err());
}
#[cfg(test)]
mod tests {
    use super::*;
    use crate::input::{CatalogInfo, PdfVersion};

    #[test]
    fn request_resolves_every_canonical_label() {
        for standard in Standard::all() {
            let resolved = ProfileRequest::new(standard.label()).resolve();
            assert_eq!(
                resolved.ok().map(ConformanceProfile::standard),
                Some(*standard)
            );
        }
        let lower = ProfileRequest::new("pdf/a-2b").resolve();
        assert_eq!(
            lower.ok().map(ConformanceProfile::standard),
            Some(Standard::PdfA2b)
        );
    }

    #[test]
    fn unknown_label_is_a_typed_unsupported_error() {
        let error = ProfileRequest::new("PDF/A-9c").resolve();
        assert_eq!(
            error,
            Err(ProfileRequestError::UnsupportedProfile {
                label: "PDF/A-9c".to_owned()
            })
        );
        let message = error.err().map_or_else(String::new, |err| err.to_string());
        assert!(message.contains("unsupported"));
        assert_eq!(
            ConformanceProfile::from_label(""),
            Err(ProfileRequestError::UnsupportedProfile {
                label: String::new()
            })
        );
    }

    #[test]
    fn contradictory_transparency_requirement_is_rejected() {
        let a1b = ProfileRequest::new(Standard::PdfA1b.label())
            .with_allow_transparency(true)
            .resolve();
        assert_eq!(
            a1b,
            Err(ProfileRequestError::ContradictoryRequirements {
                standard: Standard::PdfA1b,
                reason: "transparency is forbidden by this profile".to_owned(),
            })
        );
        let x1a = ProfileRequest::new(Standard::PdfX1a.label())
            .with_allow_transparency(true)
            .resolve();
        assert!(matches!(
            x1a,
            Err(ProfileRequestError::ContradictoryRequirements { .. })
        ));

        for standard in [Standard::PdfA2b, Standard::PdfX4, Standard::PdfE1] {
            let ok = ProfileRequest::new(standard.label())
                .with_allow_transparency(true)
                .resolve();
            assert_eq!(ok.ok().map(ConformanceProfile::standard), Some(standard));
        }
        assert!(
            ProfileRequest::new(Standard::PdfA1b.label())
                .with_allow_transparency(false)
                .resolve()
                .is_ok()
        );
    }

    #[test]
    fn dispatch_matches_the_concrete_profile_and_suggests_fixups() {
        let input = PreflightInput {
            version: PdfVersion::new(1, 4),
            catalog: CatalogInfo {
                has_xmp_metadata: false,
                ..Default::default()
            },
            ..Default::default()
        };
        let a1b = ConformanceProfile::from_label("PDF/A-1b")
            .unwrap_or_else(|_| panic!("test: canonical label must resolve"));
        assert_eq!(a1b.report(&input), PdfA1bProfile::new().report(&input));
        assert_eq!(a1b.standard(), Standard::PdfA1b);
        assert!(
            a1b.suggestions(&a1b.report(&input))
                .contains(&Fixup::AddXmpMetadata)
        );
        let error = ProfileRequestError::UnsupportedProfile {
            label: "x".to_owned(),
        };
        let _: &dyn std::error::Error = &error;
    }
}
