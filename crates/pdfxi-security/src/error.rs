//! Error types for the security crate.
//!
//! All security operations return [`Result`], whose error type is
//! [`SecurityError`]. Variants are intentionally specific so that callers can
//! distinguish a wrong password from a tampered document or a revoked
//! certificate without string matching. No documents contents or secrets are
//! ever embedded in an error value.

use std::fmt;

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
    /// A protector or test double was misconfigured.
    Configuration(String),
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
            Self::Configuration(message) => write!(f, "security configuration error: {message}"),
        }
    }
}

impl std::error::Error for SecurityError {}

/// Convenience result alias for operations that may fail with [`SecurityError`].
pub type Result<T> = core::result::Result<T, SecurityError>;
