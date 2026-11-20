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
//! - [`password_policy`] — end-to-end password planning: validates conflicting
//!   user/owner states, reports the handler and key length a document would
//!   use, and resolves open credentials without widening user permissions
//!   (SEC-001).
//! - [`authorization`] — explicit, deterministic `can_print`/`can_copy`/
//!   `can_modify`/`can_annotate` decisions derived from the user- or
//!   owner-password path, plus `/P` derivation and validation (SEC-001).
//! - [`SecurityError`] — actionable, specific error variants for wrong
//!   passwords, invalid keys, tampering, expiry, and revocation.
//! - [`SignatureInfo`] / [`SignatureStatus`] — a side-effect-free signature
//!   report, including signer, time, digest algorithm, coverage, and validity.
//! - [`Protector`] — a trait for encrypt/decrypt/permissions with a
//!   [`MockProtector`] test double.
//! - [`crypto`] — dependency-free MD5, SHA-2, RC4, and AES primitives with
//!   published known-answer tests, plus RFC 3394 AES-KW authenticated key
//!   wrapping ([`crypto::keywrap`]).
//! - [`standard_handler`] — real password authentication and per-object
//!   decryption for PDF encryption revisions 2–6
//!   ([`StandardSecurityHandler`], [`Decryptor`], [`SecurityParameters`]).
//! - [`encryption_writer`] — an encrypting save path that reuses the standard
//!   handler to write `/Encrypt` dictionaries and encrypted strings/streams,
//!   plus a password-aware reopen path ([`encrypt_document`], [`open_encrypted`],
//!   [`EncryptionReport`]).
//! - [`protect`] — document-facing protect and unprotect entry points
//!   ([`protect_document`], [`remove_protection`], [`ProtectOptions`],
//!   [`ProtectPermissions`]) over the same writer, with the eight `/P`
//!   permission bits and AES-256 revision 6 as the default.
//! - [`RedactionPlan`] / [`RedactionReport`] — page-scoped rectangle and text
//!   redaction with deterministic ordering and strict residual verification
//!   against caller-supplied post-redaction text.
//! - [`Sanitizer`] / [`SanitizationReport`] — category-based sanitization plans
//!   with deterministic actions and removal verification against a supplied
//!   inventory.
//! - [`sanitize_document()`] — applies those plans to a concrete
//!   [`pdfxi_core::Document`] with copy-on-write byte output and verification
//!   against the re-parsed result (SEC-008).
//! - [`sanitize_hidden_info()`] — extended structural removal of thumbnails,
//!   private application data, attachments, JavaScript locations, and other
//!   hidden-information classes with a deterministic selection model, a report
//!   of removed items, and post-sanitize verification (SEC-008).
//! - [`ScrubDocument`] / [`ScrubReport`] — copy-on-write metadata and XMP
//!   scrubbing with scrub profiles, deterministic XMP replacement, and residual
//!   verification.
//! - [`xmp_scrub`] — minimal XMP namespace/property removal that preserves
//!   unselected (including unknown) properties deterministically, rejects
//!   malformed XML with typed errors, and verifies the scrub (SEC-009).
//! - [`der`] — a strict, dependency-free ASN.1 DER encoder/decoder subset.
//! - [`cms`] — CMS/PKCS#7 `SignedData` creation and validation behind the
//!   [`SignatureProvider`] trait, with a deterministic in-memory test double
//!   ([`InMemorySignatureProvider`]) whose toy RSA is crate-private and is not
//!   a production signature backend.
//! - [`x509`] — X.509 certificate parsing, deterministic name display,
//!   extensions, caller-clocked chain validation, and digital IDs whose private
//!   keys stay behind an opaque [`KeyHandle`] trait.
//! - [`timestamp`] — RFC 3161 `TSTInfo` parsing, token signature and imprint
//!   checks, caller-clocked chain validation at `genTime`, a replay/skew guard,
//!   nonce verification, and a conservative mapping onto [`SignatureStatus`].
//! - [`certificate_encryption`] — certificate-based enveloped encryption with a
//!   recipient list, per-recipient CEK wrapping under RFC 3394 AES-KW, a caller-
//!   supplied [`KeyWrapProvider`] KEK boundary ([`AesKwKeyWrapProvider`]), a
//!   SHA-256 key commitment, and sender permissions. The XOR test double was
//!   replaced by real AES-KW.
//! - [`trust_manager`] — a local trust manager, CRL model with RFC 5280-style
//!   signature verification through the shared [`CertificateVerifier`] path,
//!   and deterministic trust/revocation reports with typed failure reasons.
//!   Callers supply every CRL and issuer certificate; there is no network
//!   access and no host-clock read.
//! - [`security_envelope`] — both local, tamper-evident file envelopes carrying
//!   a payload, recipient list, and permissions, and certificate envelopes that
//!   wrap a content key per recipient through the [`KeyWrapProvider`] boundary
//!   with an explicit attachment-encryption scope (SEC-010). File envelopes can
//!   carry a caller-supplied HMAC-SHA256 authentication key; unkeyed envelopes
//!   report [`crate::security_envelope::Integrity::Unkeyed`] and their
//!   permissions are untrusted. No rights-management server or network access
//!   is involved.
//! - [`policy`] — named local protection policies with a handler, key length,
//!   permissions and recipients, deterministic validation/apply plans/reports,
//!   and caller-supplied time (SEC-011). No rights-management server is
//!   involved.
//! - [`digital_id_lifecycle`] — validated caller-supplied PKCS#8-style key plus
//!   certificate pairs, a sorted and deterministic digital-ID store with
//!   caller-clocked validity and rotation, and a password-protected store
//!   envelope built from the existing AES/SHA primitives (SEC-004).
//! - [`timestamp_authority`] — a caller-supplied TSA token model with imprint,
//!   nonce, validity-window and policy checks, plus detached-signature
//!   validation wiring against a supplied certificate (SEC-006).
//!
//! All operations are deterministic and allocate only as needed. No operation
//! here logs secrets or document contents.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Explicit, deterministic authorization decisions for an unlocked session (SEC-001).
pub mod authorization;
/// Certificate-based enveloped encryption (SEC-003).
pub mod certificate_encryption;
pub mod cms;
pub mod crypto;
pub mod der;
/// Local digital-ID lifecycle: validated key/certificate pairs, a deterministic
/// store, and a password-protected store envelope (SEC-004).
pub mod digital_id_lifecycle;
mod encryption;
pub mod encryption_writer;
mod error;
/// End-to-end password and permission planning with conflict validation (SEC-001).
pub mod password_policy;
/// Real PDF signatures: a CMS `SignedData` blob over a PDF `/ByteRange`
/// (T-0241, acceptance item A-20).
pub mod pdf_signature;
mod permissions;
/// PKCS#12 (`.pfx`/`.p12`) identity import (T-0249).
pub mod pkcs12;
pub mod policy;
pub mod protect;
mod protector;
mod redaction;
mod sanitization;
pub mod sanitize_document;
mod scrub;
pub mod security_envelope;
mod signature;
pub mod standard_handler;
pub mod timestamp;
/// Timestamp-authority token model and detached-signature validation wiring (SEC-006).
pub mod timestamp_authority;
/// Local trust manager, CRLs, and deterministic trust/revocation reports (SEC-007).
pub mod trust_manager;
pub mod x509;
/// Minimal XMP namespace/property removal with post-scrub verification (SEC-009).
pub mod xmp_scrub;

pub use authorization::{Authorization, AuthorizationPath, Operation};
pub use certificate_encryption::{
    AesKwKeyWrapProvider, ContentCipher, EnvelopedData, InMemoryKeyWrapProvider, KeyWrapProvider,
    OpenedEnvelope, Recipient, RecipientEnvelopeKey, RecipientList, RecipientSelector,
    build_envelope, open_envelope,
};
pub use cms::{
    InMemorySignatureProvider, SignatureProvider, SignedDataParams, SignerIdentifier,
    build_signed_data, digest, verify_signed_data, verify_signed_data_detached,
};
pub use digital_id_lifecycle::{
    DigitalIdLifecycleStore, ExportedKeyPair, IdListing, KeyCertificatePair, PrivateKeyPkcs8,
    open_store, seal_store,
};
pub use encryption::EncryptionMethod;
pub use encryption_writer::{
    DecryptionReport, EncryptOptions, EncryptionReport, build_encrypt_dictionary, encrypt_document,
    open_encrypted,
};
pub use error::{Result, SecurityError};
pub use password_policy::{
    AppliedPasswordPlan, EncryptionPlan, PasswordCredentials, PasswordPlanInput,
    PasswordResolution, SecurityHandler, apply_password_plan, plan_encryption,
    resolve_open_credentials,
};
pub use pdf_signature::{
    DEFAULT_CONTENTS_RESERVATION, DEFAULT_FIELD_NAME, MIN_CONTENTS_RESERVATION, SignOptions,
    SignatureReport, SignatureVerification, SignedDocument, SigningIdentity, sign_pdf,
    verify_signature,
};
pub use permissions::{PasswordRole, Permission, Permissions};
pub use policy::{
    PolicyAction, PolicyPlan, PolicyReport, PolicySet, PolicyStatus, PolicyValidationReport,
    ProtectionAction, ProtectionApplyReport, ProtectionPlan, ProtectionPolicy,
    apply_protection_policy, build_policy_plan, build_protection_plan, check_permission,
    enforce_policy, validate_policy, validate_policy_spec,
};
pub use protect::{
    MAX_PASSWORD_BYTES, ProtectOptions, ProtectPermissions, protect_document, remove_protection,
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
    AppliedSanitizationReport, HiddenInfoAction, HiddenInfoClass, HiddenInfoReport,
    HiddenInfoSanitized, HiddenInfoSelection, RemovedItem, SanitizedDocument,
    hidden_info_inventory, inventory_from_document, sanitize_document, sanitize_hidden_info,
};
pub use scrub::{
    Allowlist, AnnotationRecord, Residual, ScrubAction, ScrubCategory, ScrubConfig, ScrubDocument,
    ScrubInventory, ScrubOperation, ScrubOutcome, ScrubProfile, ScrubReport, XmpPacket,
    XmpProperty, XmpPropertyKey, XmpStrategy,
};
pub use security_envelope::{
    CertificateEnvelope, CertificateEnvelopeInput, EncryptionScope, EnvelopeDeliveryReport,
    EnvelopeRecipient, EnvelopeReport, Integrity, OpenedSecurityEnvelope, RecipientDelivery,
    SecurityEnvelope, build_authenticated_security_envelope, build_certificate_envelope,
    build_security_envelope, open_security_envelope, open_security_envelope_with_key,
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
pub use timestamp_authority::{
    DetachedSignatureReport, TsaStatus, TsaTokenModel, TsaValidationReport,
    validate_detached_signature_against_certificate, validate_tsa_token,
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
pub use xmp_scrub::{
    XmpRemovalKind, XmpRemovalRecord, XmpRemovalSpec, XmpResidual, XmpScrubOutput, XmpScrubReport,
    XmpScrubVerification, scrub_and_verify_xmp, scrub_xmp, verify_xmp_scrub,
};

#[cfg(test)]
mod tests;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-security"
}
