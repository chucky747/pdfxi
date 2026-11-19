//! Digital signature metadata and validation status.
//!
//! This module provides a side-effect-free description of a digital signature:
//! who signed, when, with which digest algorithm, over what coverage, and the
//! current validity status. It does not perform cryptographic verification; it
//! models the *report* that a verifier produces so that callers can reason about
//! signature state uniformly.

use std::fmt;

/// Digest algorithm used to compute a signature's message digest.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum DigestAlgorithm {
    /// SHA-1 (legacy; retained for validating older documents).
    Sha1,
    /// SHA-256 (widely used, 256-bit output).
    Sha256,
    /// SHA-384 (384-bit output).
    Sha384,
    /// SHA-512 (512-bit output).
    Sha512,
}

impl DigestAlgorithm {
    /// Every digest algorithm in a stable order.
    pub const ALL: [Self; 4] = [Self::Sha1, Self::Sha256, Self::Sha384, Self::Sha512];

    /// Stable string identifier suitable for metadata serialization.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::Sha1 => "sha1",
            Self::Sha256 => "sha256",
            Self::Sha384 => "sha384",
            Self::Sha512 => "sha512",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "sha1" => Some(Self::Sha1),
            "sha256" => Some(Self::Sha256),
            "sha384" => Some(Self::Sha384),
            "sha512" => Some(Self::Sha512),
            _ => None,
        }
    }

    /// Length of the digest output in bytes.
    pub const fn output_len_bytes(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha384 => 48,
            Self::Sha512 => 64,
        }
    }
}

impl fmt::Display for DigestAlgorithm {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}

/// Validation status of a digital signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SignatureStatus {
    /// Signature is cryptographically valid and trusted.
    Valid,
    /// A signature is present but is invalid.
    Invalid,
    /// Signed bytes changed after the signature was created.
    Tampered,
    /// The signing certificate or signature had expired at validation time.
    Expired,
    /// The signing certificate was revoked by its issuer.
    Revoked,
    /// Trust could not be established (for example, an unknown root).
    Unknown,
}

impl SignatureStatus {
    /// Return `true` only for [`Self::Valid`].
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }

    /// Short human-readable label for this status.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Tampered => "tampered",
            Self::Expired => "expired",
            Self::Revoked => "revoked",
            Self::Unknown => "unknown",
        }
    }
}

impl fmt::Display for SignatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Portion of a document covered by a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureCoverage {
    /// The signature covers the entire document byte range.
    WholeDocument,
    /// The signature covers an inclusive range of 1-based page numbers.
    Pages {
        /// First covered page (1-based, inclusive).
        first: u32,
        /// Last covered page (1-based, inclusive).
        last: u32,
    },
}

impl SignatureCoverage {
    /// Build a validated page-range coverage.
    ///
    /// Returns `None` if `first` is less than 1 or `last` precedes `first`.
    pub fn pages(first: u32, last: u32) -> Option<Self> {
        if first >= 1 && last >= first {
            Some(Self::Pages { first, last })
        } else {
            None
        }
    }

    /// Return `true` if the given 1-based page is covered by this signature.
    pub const fn covers_page(self, page: u32) -> bool {
        match self {
            Self::WholeDocument => true,
            Self::Pages { first, last } => page >= first && page <= last,
        }
    }
}

/// A signed-document report produced by a verifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureInfo {
    signer: String,
    signed_at: u64,
    digest_algorithm: DigestAlgorithm,
    coverage: SignatureCoverage,
    validity: SignatureStatus,
}

impl SignatureInfo {
    /// Create a signature report.
    ///
    /// `signed_at` is a Unix timestamp in seconds. `signer` is a display name
    /// such as a certificate subject; no secrets are stored here.
    pub fn new(
        signer: impl Into<String>,
        signed_at: u64,
        digest_algorithm: DigestAlgorithm,
        coverage: SignatureCoverage,
        validity: SignatureStatus,
    ) -> Self {
        Self {
            signer: signer.into(),
            signed_at,
            digest_algorithm,
            coverage,
            validity,
        }
    }

    /// Display name of the signer.
    pub fn signer(&self) -> &str {
        &self.signer
    }

    /// Unix timestamp (seconds) at which the signature was created.
    pub const fn signed_at(&self) -> u64 {
        self.signed_at
    }

    /// Digest algorithm used for the signature.
    pub const fn digest_algorithm(&self) -> DigestAlgorithm {
        self.digest_algorithm
    }

    /// Portion of the document covered by the signature.
    pub const fn coverage(&self) -> SignatureCoverage {
        self.coverage
    }

    /// Current validity status of the signature.
    pub const fn validity(&self) -> SignatureStatus {
        self.validity
    }

    /// Convenience check: `true` when [`Self::validity`] is valid.
    pub const fn is_valid(&self) -> bool {
        self.validity.is_valid()
    }
}
