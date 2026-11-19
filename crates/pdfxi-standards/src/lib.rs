//! PDF/A, PDF/X, PDF/E, PDF/VT, preflight, color management, and optimization.
//!
//! This crate provides the standards contracts for the convert lane:
//!
//! - [`Standard`] identifies PDF/A-1b, PDF/A-2b, PDF/X-1a, PDF/X-4, PDF/E-1,
//!   and PDF/UA-1 with canonical labels and minimum PDF versions.
//! - [`Severity`] and [`PreflightFinding`] describe actionable check results.
//! - [`PreflightProfile`] defines a deterministic checker; its provided methods
//!   sort findings into a canonical order and filter them by severity.
//! - [`PreflightReport`], [`StaticProfile`], and [`HeaderProfile`] are the
//!   report type, a test double, and a small real header check respectively.
//!
//! The crate is dependency-free and never executes external tools. Full
//! conformance (including ICC colour, transparency, and tagging rules) belongs
//! in future adapters; veraPDF is used as a CI oracle, not a runtime
//! dependency.
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

mod finding;
mod profile;
mod standard;

pub use finding::{PreflightFinding, Severity, filter_by_severity, sort_findings};
pub use profile::{HeaderProfile, PreflightProfile, PreflightReport, StaticProfile};
pub use standard::{Standard, StandardFamily};

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-standards"
}
