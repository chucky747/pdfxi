//! Error types for the security crate.
//!
//! All security operations return [`Result`], whose error type is
//! [`SecurityError`]. Variants are intentionally specific so that callers can
//! distinguish a wrong password from a tampered document or a revoked
//! certificate without string matching. No documents contents or secrets are
//! ever embedded in an error value.

use std::fmt;

use crate::sanitization::SanitizationCategory;

/// A failure produced by a security operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityError {
    /// The supplied password matched neither the user nor the owner password.
    WrongPassword,
    /// A password was required but none was supplied.
    MissingPassword,
    /// The requested encryption method is not supported by the active backend.
    UnsupportedMethod(String),
    /// A permission mask contained reserved or unknown bits.
    InvalidPermissionBits(u32),
    /// A requested key length did not match the encryption method.
    InvalidKeyLength(u16),
    /// Encryption could not be completed.
    EncryptionFailed,
    /// Decryption failed (for example, invalid key or padding).
    DecryptionFailed,
    /// A signature was structurally invalid or could not be parsed.
    SignatureInvalid,
    /// Signed content was modified after the signature was created.
    TamperedContent,
    /// A certificate or signature had expired at validation time.
    SignatureExpired,
    /// A signing certificate was revoked by its issuer.
    CertificateRevoked,
    /// A CMS/DER input ended before a complete encoded value was available.
    DerTruncated,
    /// A DER input violated a canonical encoding rule (for example an indefinite
    /// length, a non-minimal integer, or an out-of-range date).
    DerInvalid,
    /// A declared DER length could not be represented or exceeded the input.
    DerLengthOverflow,
    /// A DER value had a different tag than the parser expected.
    ///
    /// Both identifier octets are reported, never the value's contents.
    DerUnexpectedTag {
        /// Identifier octet the parser expected.
        expected: u8,
        /// Identifier octet that was actually present.
        found: u8,
    },
    /// Bytes remained after a complete top-level DER value was parsed.
    DerTrailingBytes,
    /// The CMS digest algorithm is not supported for signing or verification.
    UnsupportedDigestAlgorithm(String),
    /// The CMS signature algorithm is not supported for verification.
    UnsupportedSignatureAlgorithm(String),
    /// A required signed attribute was absent from a `SignedData` signer info.
    MissingSignedAttribute(&'static str),
    /// The signer identifier did not match the supplied signing provider.
    SignerMismatch,
    /// The embedded certificate did not match the supplied signing provider.
    CertificateMismatch,
    /// A `SignedData` did not contain the expected signer certificate.
    CertificateMissing,
    /// An X.509 certificate used a version other than v1 or v3.
    CertificateVersionUnsupported(u64),
    /// The certificate's `notBefore` was later than the validation time.
    CertificateNotYetValid,
    /// The certificate's `notAfter` was earlier than the validation time.
    CertificateExpired,
    /// No issuer certificate could be found for a certificate in the chain.
    CertificateIssuerUnknown,
    /// The chain did not terminate at a caller-supplied trust anchor.
    CertificateUntrusted,
    /// The certificate chain exceeded the caller's maximum depth.
    CertificateChainTooLong,
    /// A certificate signature check was rejected by the caller's verifier.
    CertificateSignatureInvalid,
    /// A certificate's key usage did not permit the requested operation.
    CertificateKeyUsageViolation(&'static str),
    /// An issuing certificate was not marked as a CA in basic constraints.
    CertificateNotCa,
    /// A required extended key usage purpose was absent from the certificate.
    CertificateEkuMissing,
    /// A critical X.509 extension that this subset does not understand was present.
    CertificateUnknownCriticalExtension,
    /// A known extension carried a malformed payload.
    CertificateExtensionInvalid(&'static str),
    /// An optional `TBSCertificate` field appeared more than once.
    CertificateDuplicateField(&'static str),
    /// Two extensions in a certificate declared the same OID.
    CertificateDuplicateExtension,
    /// A digital ID with the same certificate or key handle already exists.
    DigitalIdDuplicate,
    /// No digital ID matched the requested lookup.
    DigitalIdNotFound,
    /// Exported metadata did not match the supplied certificate material.
    DigitalIdMetadataMismatch,
    /// A protector or test double was misconfigured.
    Configuration(String),
    /// A page number was zero; PDF page numbers are 1-based.
    InvalidPageNumber,
    /// A redaction rectangle had zero or negative area.
    EmptyRedactionRegion,
    /// A rectangle or page-bound coordinate was NaN or infinite.
    NonFiniteCoordinate,
    /// Page bounds had a non-finite or non-positive dimension.
    InvalidPageBounds,
    /// A redaction rectangle referenced a page absent from the supplied bounds.
    UnknownPage {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A redaction rectangle extended outside its declared page bounds.
    RedactionRegionOutOfBounds {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A redaction text pattern was empty or whitespace-only.
    EmptyRedactionPattern,
    /// Strict redaction verification found residual content.
    RedactionVerificationFailed {
        /// Page whose text contained the residual.
        page: u32,
        /// Byte offset of the residual within that page's text.
        offset: usize,
    },
    /// Strict sanitization verification found a category still present.
    SanitizationVerificationFailed {
        /// Category that was still present after sanitization.
        category: SanitizationCategory,
    },
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "the supplied password is incorrect"),
            Self::MissingPassword => write!(f, "a password is required but none was supplied"),
            Self::UnsupportedMethod(method) => {
                write!(f, "unsupported encryption method: {method}")
            }
            Self::InvalidPermissionBits(bits) => {
                write!(f, "invalid permission bit mask: {bits:#010x}")
            }
            Self::InvalidKeyLength(bits) => {
                write!(f, "invalid key length: {bits} bits")
            }
            Self::EncryptionFailed => write!(f, "encryption failed"),
            Self::DecryptionFailed => write!(f, "decryption failed"),
            Self::SignatureInvalid => write!(f, "signature is invalid"),
            Self::TamperedContent => write!(f, "signed content was tampered with"),
            Self::SignatureExpired => write!(f, "signature or certificate has expired"),
            Self::CertificateRevoked => write!(f, "signing certificate was revoked"),
            Self::DerTruncated => write!(f, "DER input was truncated"),
            Self::DerInvalid => write!(f, "DER input was not canonically encoded"),
            Self::DerLengthOverflow => write!(f, "DER length exceeded representable bounds"),
            Self::DerUnexpectedTag { expected, found } => {
                write!(
                    f,
                    "unexpected DER tag: expected {expected:#04x}, found {found:#04x}"
                )
            }
            Self::DerTrailingBytes => write!(f, "DER input had trailing bytes"),
            Self::UnsupportedDigestAlgorithm(algorithm) => {
                write!(f, "unsupported digest algorithm: {algorithm}")
            }
            Self::UnsupportedSignatureAlgorithm(algorithm) => {
                write!(f, "unsupported signature algorithm: {algorithm}")
            }
            Self::MissingSignedAttribute(name) => {
                write!(f, "missing required signed attribute: {name}")
            }
            Self::SignerMismatch => write!(f, "signer identifier did not match the provider"),
            Self::CertificateMismatch => {
                write!(f, "embedded certificate did not match the provider")
            }
            Self::CertificateMissing => write!(f, "signed data contained no signer certificate"),
            Self::CertificateVersionUnsupported(version) => {
                write!(f, "unsupported X.509 certificate version: {version}")
            }
            Self::CertificateNotYetValid => write!(f, "certificate is not yet valid"),
            Self::CertificateExpired => write!(f, "certificate has expired"),
            Self::CertificateIssuerUnknown => write!(f, "certificate issuer is unknown"),
            Self::CertificateUntrusted => write!(f, "certificate chain has no trusted anchor"),
            Self::CertificateChainTooLong => write!(f, "certificate chain is too long"),
            Self::CertificateSignatureInvalid => write!(f, "certificate signature is invalid"),
            Self::CertificateKeyUsageViolation(usage) => {
                write!(f, "certificate key usage forbids {usage}")
            }
            Self::CertificateNotCa => {
                write!(f, "issuing certificate is not marked as a CA")
            }
            Self::CertificateEkuMissing => {
                write!(f, "certificate lacks the required extended key usage")
            }
            Self::CertificateUnknownCriticalExtension => {
                write!(f, "certificate contains an unknown critical extension")
            }
            Self::CertificateExtensionInvalid(name) => {
                write!(f, "malformed X.509 extension: {name}")
            }
            Self::CertificateDuplicateField(name) => {
                write!(f, "duplicate X.509 TBS field: {name}")
            }
            Self::CertificateDuplicateExtension => {
                write!(f, "certificate contains duplicate extension OIDs")
            }
            Self::DigitalIdDuplicate => write!(f, "a digital ID with this identity already exists"),
            Self::DigitalIdNotFound => write!(f, "no matching digital ID was found"),
            Self::DigitalIdMetadataMismatch => {
                write!(f, "digital ID metadata does not match the certificate")
            }
            Self::Configuration(message) => write!(f, "security configuration error: {message}"),
            Self::InvalidPageNumber => write!(f, "page number must be at least 1"),
            Self::EmptyRedactionRegion => {
                write!(f, "redaction rectangle has zero or negative area")
            }
            Self::NonFiniteCoordinate => write!(f, "redaction coordinate was not finite"),
            Self::InvalidPageBounds => write!(f, "page bounds must be finite and positive"),
            Self::UnknownPage { page } => {
                write!(f, "page {page} is not present in the supplied page bounds")
            }
            Self::RedactionRegionOutOfBounds { page } => {
                write!(f, "redaction rectangle lies outside page {page}")
            }
            Self::EmptyRedactionPattern => write!(f, "redaction text pattern was empty"),
            Self::RedactionVerificationFailed { page, offset } => write!(
                f,
                "redacted content remains on page {page} at byte offset {offset}"
            ),
            Self::SanitizationVerificationFailed { category } => {
                write!(f, "sanitization category still present: {category}")
            }
        }
    }
}

impl std::error::Error for SecurityError {}

/// Convenience result alias for operations that may fail with [`SecurityError`].
pub type Result<T> = core::result::Result<T, SecurityError>;
