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
//! - [`encryption_writer`] — an encrypting save path that reuses the standard
//!   handler to write `/Encrypt` dictionaries and encrypted strings/streams,
//!   plus a password-aware reopen path ([`encrypt_document`], [`open_encrypted`],
//!   [`EncryptionReport`]).
//! - [`RedactionPlan`] / [`RedactionReport`] — page-scoped rectangle and text
//!   redaction with deterministic ordering and strict residual verification
//!   against caller-supplied post-redaction text.
//! - [`Sanitizer`] / [`SanitizationReport`] — category-based sanitization plans
//!   with deterministic actions and removal verification against a supplied
//!   inventory.
//! - [`sanitize_document()`] — applies those plans to a concrete
//!   [`pdfxi_core::Document`] with copy-on-write byte output and verification
//!   against the re-parsed result (SEC-008).
//! - [`ScrubDocument`] / [`ScrubReport`] — copy-on-write metadata and XMP
//!   scrubbing with scrub profiles, deterministic XMP replacement, and residual
//!   verification.
//! - [`der`] — a strict, dependency-free ASN.1 DER encoder/decoder subset.
//! - [`cms`] — CMS/PKCS#7 `SignedData` creation and validation behind the
//!   [`SignatureProvider`] trait, with a deterministic in-memory test provider.
//! - [`x509`] — X.509 certificate parsing, deterministic name display,
//!   extensions, caller-clocked chain validation, and digital IDs whose private
//!   keys stay behind an opaque [`KeyHandle`] trait.
//! - [`timestamp`] — RFC 3161 `TSTInfo` parsing, token signature and imprint
//!   checks, caller-clocked chain validation at `genTime`, a replay/skew guard,
//!   nonce verification, and a conservative mapping onto [`SignatureStatus`].
//! - [`certificate_encryption`] — certificate-based enveloped encryption with a
//!   recipient list, per-recipient wrapped keys behind a caller-supplied
//!   [`KeyWrapProvider`], a SHA-256 key commitment, and sender permissions.
//! - [`trust_manager`] — a local trust manager, CRL model, and deterministic
//!   trust/revocation reports with typed failure reasons. Callers supply every
//!   CRL; there is no network access and no host-clock read.
//! - [`security_envelope`] — local, tamper-evident file envelopes carrying a
//!   payload, recipient list, and permissions (SEC-010). No rights-management
//!   server or network access is involved.
//! - [`policy`] — named local protection policies with deterministic
//!   plans/reports and caller-supplied time (SEC-011). No rights-management
//!   server is involved.
//!
//! All operations are deterministic and allocate only as needed. No operation
//! here logs secrets or document contents.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Certificate-based enveloped encryption (SEC-003).
pub mod certificate_encryption;
pub mod cms;
pub mod crypto;
pub mod der;
mod encryption;
pub mod encryption_writer;
mod error;
mod permissions;
pub mod policy;
mod protector;
mod redaction;
mod sanitization;
pub mod sanitize_document;
mod scrub;
pub mod security_envelope;
mod signature;
pub mod standard_handler;
pub mod timestamp;
/// Local trust manager, CRLs, and deterministic trust/revocation reports (SEC-007).
pub mod trust_manager;
pub mod x509;

pub use certificate_encryption::{
    ContentCipher, EnvelopedData, InMemoryKeyWrapProvider, KeyWrapProvider, OpenedEnvelope,
    Recipient, RecipientEnvelopeKey, RecipientList, RecipientSelector, build_envelope,
    open_envelope,
};
pub use cms::{
    InMemorySignatureProvider, SignatureProvider, SignedDataParams, SignerIdentifier,
    build_signed_data, digest, modpow_u128, verify_signed_data, verify_signed_data_detached,
};
pub use encryption::EncryptionMethod;
pub use encryption_writer::{
    DecryptionReport, EncryptOptions, EncryptionReport, build_encrypt_dictionary, encrypt_document,
    open_encrypted,
};
pub use error::{Result, SecurityError};
pub use permissions::{PasswordRole, Permission, Permissions};
pub use policy::{
    PolicyAction, PolicyPlan, PolicyReport, PolicySet, PolicyStatus, ProtectionPolicy,
    build_policy_plan, check_permission, enforce_policy, validate_policy,
};
pub use protector::{MockProtector, Protector};
pub use redaction::{
    PageBounds, PageNumber, RedactionPlan, RedactionRect, RedactionRegion, RedactionReport,
    ResidualOccurrence, ResidualSource, TextPattern,
};
pub use sanitization::{
    SanitizationAction, SanitizationCategory, SanitizationInventory, SanitizationOperation,
    SanitizationReport, Sanitizer,
};
pub use sanitize_document::{
    AppliedSanitizationReport, SanitizedDocument, inventory_from_document, sanitize_document,
};
pub use scrub::{
    Allowlist, AnnotationRecord, Residual, ScrubAction, ScrubCategory, ScrubConfig, ScrubDocument,
    ScrubInventory, ScrubOperation, ScrubOutcome, ScrubProfile, ScrubReport, XmpPacket,
    XmpProperty, XmpPropertyKey, XmpStrategy,
};
pub use security_envelope::{
    EnvelopeRecipient, EnvelopeReport, OpenedSecurityEnvelope, SecurityEnvelope,
    build_security_envelope, open_security_envelope,
};
pub use signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};
pub use standard_handler::{
    Decryptor, PASSWORD_PADDING, SecurityParameters, StandardSecurityHandler, compute_file_key,
    compute_o_and_oe, compute_o_value, compute_perms, compute_u_and_ue, compute_u_value,
    pad_password,
};
pub use timestamp::{
    Accuracy, MessageImprint, TimestampPolicy, TimestampStatus, TimestampTokenParams,
    TimestampValidation, TimestampValidationContext, TstInfo, build_timestamp_token,
    encode_tst_info, parse_tst_info, validate_timestamp_token,
};
pub use trust_manager::{
    Crl, RevocationEvidence, RevocationPolicy, RevocationReason, RevocationStatus,
    RevokedCertificate, TrustDecision, TrustFailureReason, TrustManager, TrustReport,
};
pub use x509::{
    BasicConstraints, Certificate, CertificateVerifier, ChainReport, Clock, DigitalId,
    DigitalIdMetadata, DigitalIdStore, EkuPurpose, ExtendedKeyUsage, Extension, ExtensionValue,
    FixedClock, KeyHandle, KeyUsage, KeyUsageBit, Name, OpaqueKeyHandle, PublicKeyInfo,
    RejectingVerifier, RelativeDistinguishedName, SignatureAlgorithm, SubjectAltName, TrustStore,
    ValidationPolicy, Validity, ValidityStatus, verify_chain,
};

#[cfg(test)]
mod tests;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-security"
}
