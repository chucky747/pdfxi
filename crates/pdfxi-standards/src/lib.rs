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
//! - [`PdfA1bProfile`], [`PdfA2bProfile`], [`PdfX1aProfile`], [`PdfX4Profile`],
//!   and [`PdfUa1Profile`] run real, deterministic preflight checks over that
//!   snapshot with stable codes such as `PDFA1B.NO_OUTPUT_INTENT`.
//! - [`PreflightReport`], [`StaticProfile`], and [`HeaderProfile`] are the
//!   report type, a test double, and a small real header check respectively.
//!
//! The crate is dependency-free and never executes external tools. Checks are
//! limited to facts decidable from the snapshot; full byte-level conformance
//! (including ICC profile validation) belongs in future adapters, with veraPDF
//! used as a CI oracle rather than a runtime dependency.
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
mod input;
mod profile;
mod standard;

pub use document_profile::{
    PdfA1bProfile, PdfA2bProfile, PdfUa1Profile, PdfX1aProfile, PdfX4Profile,
};
pub use finding::{PreflightFinding, Severity, filter_by_severity, sort_findings};
pub use input::{
    CatalogInfo, ColourSpace, FontInfo, FontType, ImageFilter, ImageInfo, OutputIntent, PageBoxes,
    PageInfo, PdfVersion, PreflightInput, TransparencyUsage,
};
pub use profile::{HeaderProfile, PreflightProfile, PreflightReport, StaticProfile};
pub use standard::{Standard, StandardFamily};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-standards"
}
