//! Encryption, permissions, digital signatures, certificates, sanitization, and policy.
//!
//! This crate provides a dependency-free, in-memory model for PDF security.
//! The core abstractions are:
//!
//! - [`EncryptionMethod`] — the PDF-standard handlers (none, RC4-40/128,
//!   AES-128/256), with key lengths and round-trippable string identifiers.
//! - [`Permission`] / [`Permissions`] — permission bit flags and the effective
//!   rights of a session, taking the [`PasswordRole`] into account. The owner
//!   role bypasses restrictions, as with the PDF owner password.
//! - [`SecurityError`] — actionable, specific error variants for wrong
//!   passwords, invalid keys, tampering, expiry, and revocation.
//! - [`SignatureInfo`] / [`SignatureStatus`] — a side-effect-free signature
//!   report, including signer, time, digest algorithm, coverage, and validity.
//! - [`Protector`] — a trait for encrypt/decrypt/permissions with a
//!   [`MockProtector`] test double.
//! - [`crypto`] — dependency-free MD5, SHA-2, RC4, and AES primitives with
//!   published known-answer tests.
//! - [`standard_handler`] — real password authentication and per-object
//!   decryption for PDF encryption revisions 2–6
//!   ([`StandardSecurityHandler`], [`Decryptor`], [`SecurityParameters`]).
//! - [`RedactionPlan`] / [`RedactionReport`] — page-scoped rectangle and text
//!   redaction with deterministic ordering and strict residual verification
//!   against caller-supplied post-redaction text.
//! - [`Sanitizer`] / [`SanitizationReport`] — category-based sanitization plans
//!   with deterministic actions and removal verification against a supplied
//!   inventory.
//!
//! All operations are deterministic and allocate only as needed. CMS/PKCS#7
//! verification and timestamps will be layered onto these contracts in later
//! milestones without changing the public surface. No operation logs secrets
//! or document contents.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod crypto;
mod encryption;
mod error;
mod permissions;
mod protector;
mod redaction;
mod sanitization;
mod signature;
pub mod standard_handler;

pub use encryption::EncryptionMethod;
pub use error::{Result, SecurityError};
pub use permissions::{PasswordRole, Permission, Permissions};
pub use protector::{MockProtector, Protector};
pub use redaction::{
    PageBounds, PageNumber, RedactionPlan, RedactionRect, RedactionRegion, RedactionReport,
    ResidualOccurrence, ResidualSource, TextPattern,
};
pub use sanitization::{
    SanitizationAction, SanitizationCategory, SanitizationInventory, SanitizationOperation,
    SanitizationReport, Sanitizer,
};
pub use signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};
pub use standard_handler::{
    Decryptor, PASSWORD_PADDING, SecurityParameters, StandardSecurityHandler, compute_file_key,
    compute_o_and_oe, compute_o_value, compute_perms, compute_u_and_ue, compute_u_value,
    pad_password,
};

#[cfg(test)]
mod tests;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-security"
}
