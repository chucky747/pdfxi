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
//!
//! All operations are deterministic and allocate only as needed. Concrete
//! cipher handlers, CMS/PKCS#7 verification, timestamps, redaction, and
//! sanitization will be layered onto these contracts in later milestones
//! without changing the public surface. No operation logs secrets or document
//! contents.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod encryption;
mod error;
mod permissions;
mod protector;
mod signature;

pub use encryption::EncryptionMethod;
pub use error::{Result, SecurityError};
pub use permissions::{PasswordRole, Permission, Permissions};
pub use protector::{MockProtector, Protector};
pub use signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};

#[cfg(test)]
mod tests;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-security"
}
