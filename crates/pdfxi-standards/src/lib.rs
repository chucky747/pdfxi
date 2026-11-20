//! PDF/A, PDF/X, PDF/E, PDF/VT, preflight, color management, and optimization.
//!
//! This crate provides the standards contracts for the convert lane:
//!
//! - [`Standard`] identifies PDF/A-1b, PDF/A-2b, PDF/X-1a, PDF/X-4, PDF/E-1,
//!   and PDF/UA-1 with canonical labels and minimum PDF versions.
//! - [`Severity`] and [`PreflightFinding`] describe actionable check results.
//! - [`PreflightProfile`] defines a deterministic checker over an associated
//!   input type; its provided methods sort findings into a canonical order and
//!   filter them by severity.
//! - [`PreflightInput`] is a dependency-free snapshot of the document facts the
//!   real profiles need. A caller adapts `pdfxi-core` data into it.
//! - [`PdfA1bProfile`], [`PdfA2bProfile`], [`PdfA3bProfile`], [`PdfX1aProfile`],
//!   [`PdfX4Profile`], [`PdfE1Profile`], and [`PdfUa1Profile`] run real,
//!   deterministic preflight checks over that snapshot with stable codes such
//!   as `PDFA1B.NO_OUTPUT_INTENT`. Rules cover version and catalog facts, font
//!   embedding, colour spaces, image filters, transparency, XMP, output
//!   intents, annotations, encryption, and forbidden entries.
//! - [`apply_fixups`], [`FixupPolicy`], and [`FixupReport`] apply only explicitly
//!   enabled, safe mechanical fixes to a cloned snapshot and audit every change.
//!   [`suggested_fixups`] derives a conservative fixup list from a report.
//! - [`ProfileRequest`], [`ConformanceProfile`], and [`ProfileRequestError`]
//!   resolve a caller-supplied label to a concrete profile and reject unknown
//!   or contradictory requests with typed errors.
//! - [`PreflightReport`], [`StaticProfile`], and [`HeaderProfile`] are the
//!   report type, a test double, and a small real header check respectively.
//! - [`icc`] validates bounded output-intent ICC bytes (header, `acsp`
//!   signature, declared size, colour space, PCS, and tag-table bounds) with
//!   typed [`IccError`]s and stable codes. [`validate_icc`] and
//!   [`validated_output_intent_icc`] expose it; invalid bytes produce
//!   `*.ICC_PROFILE_INVALID` findings in the PDF/A and PDF/X profiles.
//! - [`structure`] runs deterministic PDF/UA structure checks (role-map
//!   coverage, alt text, tagged text, `/StructParents`, unique MCIDs, and
//!   parent links) and returns typed [`StructureViolation`]s.
//!
//! The crate is dependency-free and never executes external tools. [`CERTIFICATION`]
//! is `false`: checks are limited to facts decidable from the snapshot. Full
//! byte-level conformance, semantic reading order, and colourimetric validation
//! need human review or an independent validator; veraPDF is a CI oracle, not a
//! runtime dependency. Fixups edit a cloned snapshot only: a producer applies
//! the result to PDF bytes and must still supply real fonts, ICC profiles, and
//! metadata where a placeholder is used.
//!
//! # Examples
//!
//! This example is rendered as text rather than executed as a doctest because
//! the build host denies execution of generated doctest binaries (os error 5).
//! The equivalent behaviour is verified by the unit tests in `profile::tests`.
//!
//! ```text
//! use pdfxi_standards::{HeaderProfile, PreflightProfile, Severity, Standard};
//!
//! let profile = HeaderProfile::new(Standard::PdfA2b);
//! assert!(profile.check_sorted(b"%PDF-1.7 content").is_empty());
//! let findings = profile.check_filtered(b"%PDF-1.4 content", Severity::Warning);
//! assert_eq!(findings[0].code(), "STD-004");
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod document_profile;
mod finding;
mod fixup;
pub mod icc;
mod input;
mod profile;
mod profile_request;
mod standard;
pub mod structure;

pub use document_profile::{
    PdfA1bProfile, PdfA2bProfile, PdfA3bProfile, PdfE1Profile, PdfUa1Profile, PdfX1aProfile,
    PdfX4Profile,
};
pub use finding::{PreflightFinding, Severity, filter_by_severity, sort_findings};
pub use fixup::{
    Fixup, FixupError, FixupOutcome, FixupPolicy, FixupRecord, FixupReport, apply_fixups,
    profile_uses_output_intents, prohibited_entries, required_output_subtype, suggested_fixups,
};
pub use icc::{
    ICC_HEADER_LEN, ICC_MIN_PROFILE_LEN, ICC_TAG_COUNT_OFFSET, ICC_TAG_ENTRY_LEN,
    ICC_TAG_TABLE_OFFSET, IccColourSpace, IccError, IccProfileInfo, MAX_ICC_BYTES, MAX_ICC_TAGS,
    validate_icc, validate_icc_with_limit, validated_output_intent_icc,
};
pub use input::{
    AnnotationInfo, CatalogInfo, ColourSpace, FontInfo, FontType, ImageFilter, ImageInfo,
    OutputIntent, PageBoxes, PageInfo, PageStructureInfo, PdfVersion, PreflightInput,
    StructElementInfo, TransparencyUsage,
};
pub use profile::{HeaderProfile, PreflightProfile, PreflightReport, StaticProfile};
pub use profile_request::{ConformanceProfile, ProfileRequest, ProfileRequestError};
pub use standard::{Standard, StandardFamily};
pub use structure::{STANDARD_ROLES, StructureViolation, check_structure, is_standard_role};

/// Whether this crate performs conformance certification. Always `false`.
///
/// A green [`PreflightReport`] means no decidable subset rule fired. It is not
/// a certification: an independent validator (for example veraPDF in CI) and
/// human review remain necessary for any PDF/A, PDF/X, or PDF/UA claim.
pub const CERTIFICATION: bool = false;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-standards"
}
