//! Dependency-free X.509 certificate parsing, validation, and digital IDs.
//!
//! This module implements the public X.690 DER subset needed for RFC 5280
//! certificates without pulling in a cryptography crate. It deliberately stops
//! at the trust boundary: no private key, no symmetric secret, and no hidden
//! root store lives here.
//!
//! # Layout
//!
//! - [`certificate`] — `Certificate`, `TbsCertificate` fields, validity, and
//!   `SubjectPublicKeyInfo` parsing.
//! - [`name`] — X.501 `Name` / RDN parsing with deterministic RFC 4514 display.
//! - [`extension`] — basic constraints, key usage, subject alternative names,
//!   and extended key usage.
//! - [`verify`] — [`TrustStore`], [`CertificateVerifier`], and chain validation.
//! - [`digital_id`] — [`KeyHandle`], [`DigitalId`], and [`DigitalIdStore`].
//!
//! All parsers are bounds-checked and hostile-input aware: every read goes
//! through [`crate::der::DerReader`], and malformed input yields a typed
//! [`crate::SecurityError`] rather than a panic. Values are never logged, and
//! `Debug` implementations redact key material.
//!
//! # Trust boundary
//!
//! Signature checking is delegated to the caller through the
//! [`CertificateVerifier`] trait. This crate never generates or holds keys and
//! never implies that a cryptographically valid signature is *trusted*: trust
//! anchors and intermediates are supplied explicitly by the caller in a
//! [`TrustStore`], and every trust decision is reported by [`verify_chain`].
//!
//! [`CertificateVerifier`]: verify::CertificateVerifier
//! [`TrustStore`]: verify::TrustStore
//! [`verify_chain`]: verify::verify_chain
//! [`KeyHandle`]: digital_id::KeyHandle
//! [`DigitalId`]: digital_id::DigitalId
//! [`DigitalIdStore`]: digital_id::DigitalIdStore

pub mod certificate;
pub mod digital_id;
pub mod extension;
pub mod name;
pub mod verify;

#[cfg(test)]
mod tests;

pub use certificate::{Certificate, PublicKeyInfo, Validity, ValidityStatus};
pub use digital_id::{DigitalId, DigitalIdMetadata, DigitalIdStore, KeyHandle, OpaqueKeyHandle};
pub use extension::{
    BasicConstraints, EkuPurpose, ExtendedKeyUsage, Extension, ExtensionValue, KeyUsage,
    KeyUsageBit, SubjectAltName,
};
pub use name::{AttributeTypeAndValue, Name, RelativeDistinguishedName};
pub use verify::{
    CertificateVerifier, ChainReport, RejectingVerifier, TrustStore, ValidationPolicy, verify_chain,
};

use crate::der::Tlv;
use crate::error::{Result, SecurityError};
use crate::signature::DigestAlgorithm;

/// OID `1.2.840.113549.1.1.5` — `sha1WithRSAEncryption`.
pub const OID_SHA1_WITH_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 5];
/// OID `1.2.840.113549.1.1.11` — `sha256WithRSAEncryption`.
pub const OID_SHA256_WITH_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 11];
/// OID `1.2.840.113549.1.1.12` — `sha384WithRSAEncryption`.
pub const OID_SHA384_WITH_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 12];
/// OID `1.2.840.113549.1.1.13` — `sha512WithRSAEncryption`.
pub const OID_SHA512_WITH_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 13];
/// OID `1.2.840.10045.4.3.2` — `ecdsa-with-SHA256`.
pub const OID_ECDSA_SHA256: [u32; 7] = [1, 2, 840, 10045, 4, 3, 2];
/// OID `1.2.840.10045.4.3.3` — `ecdsa-with-SHA384`.
pub const OID_ECDSA_SHA384: [u32; 7] = [1, 2, 840, 10045, 4, 3, 3];
/// OID `1.2.840.10045.4.3.4` — `ecdsa-with-SHA512`.
pub const OID_ECDSA_SHA512: [u32; 7] = [1, 2, 840, 10045, 4, 3, 4];

/// A signature algorithm identified by an X.509 `AlgorithmIdentifier` OID.
///
/// Unknown OIDs are preserved as [`Self::Other`] so that a certificate can be
/// parsed and displayed even when this subset cannot verify its signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SignatureAlgorithm {
    /// `sha1WithRSAEncryption` (legacy; recognition only).
    Sha1WithRsa,
    /// `sha256WithRSAEncryption`.
    Sha256WithRsa,
    /// `sha384WithRSAEncryption`.
    Sha384WithRsa,
    /// `sha512WithRSAEncryption`.
    Sha512WithRsa,
    /// `ecdsa-with-SHA256`.
    EcdsaWithSha256,
    /// `ecdsa-with-SHA384`.
    EcdsaWithSha384,
    /// `ecdsa-with-SHA512`.
    EcdsaWithSha512,
    /// An unrecognised signature OID, retained verbatim.
    Other(Vec<u32>),
}

impl SignatureAlgorithm {
    /// Map an OID to a signature algorithm.
    pub fn from_oid(oid: &[u32]) -> Self {
        match oid {
            o if o == OID_SHA1_WITH_RSA => Self::Sha1WithRsa,
            o if o == OID_SHA256_WITH_RSA => Self::Sha256WithRsa,
            o if o == OID_SHA384_WITH_RSA => Self::Sha384WithRsa,
            o if o == OID_SHA512_WITH_RSA => Self::Sha512WithRsa,
            o if o == OID_ECDSA_SHA256 => Self::EcdsaWithSha256,
            o if o == OID_ECDSA_SHA384 => Self::EcdsaWithSha384,
            o if o == OID_ECDSA_SHA512 => Self::EcdsaWithSha512,
            other => Self::Other(other.to_vec()),
        }
    }

    /// Return the OID arcs for this algorithm.
    pub fn oid(&self) -> Vec<u32> {
        match self {
            Self::Sha1WithRsa => OID_SHA1_WITH_RSA.to_vec(),
            Self::Sha256WithRsa => OID_SHA256_WITH_RSA.to_vec(),
            Self::Sha384WithRsa => OID_SHA384_WITH_RSA.to_vec(),
            Self::Sha512WithRsa => OID_SHA512_WITH_RSA.to_vec(),
            Self::EcdsaWithSha256 => OID_ECDSA_SHA256.to_vec(),
            Self::EcdsaWithSha384 => OID_ECDSA_SHA384.to_vec(),
            Self::EcdsaWithSha512 => OID_ECDSA_SHA512.to_vec(),
            Self::Other(oid) => oid.clone(),
        }
    }

    /// Return the digest algorithm, or `None` when the OID is unrecognised.
    pub const fn digest_algorithm(&self) -> Option<DigestAlgorithm> {
        match self {
            Self::Sha1WithRsa => Some(DigestAlgorithm::Sha1),
            Self::Sha256WithRsa | Self::EcdsaWithSha256 => Some(DigestAlgorithm::Sha256),
            Self::Sha384WithRsa | Self::EcdsaWithSha384 => Some(DigestAlgorithm::Sha384),
            Self::Sha512WithRsa | Self::EcdsaWithSha512 => Some(DigestAlgorithm::Sha512),
            Self::Other(_) => None,
        }
    }

    /// Stable string identifier (dotted OID for unrecognised algorithms).
    pub fn as_str(&self) -> String {
        match self.digest_algorithm() {
            Some(digest) => format!("{}|{}", digest.as_id(), self.family()),
            None => oid_string(&self.oid()),
        }
    }

    const fn family(&self) -> &'static str {
        match self {
            Self::EcdsaWithSha256 | Self::EcdsaWithSha384 | Self::EcdsaWithSha512 => "ecdsa",
            _ => "rsa",
        }
    }
}

/// Render an OID as a dotted decimal string.
pub fn oid_string(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// Parse an X.509 `AlgorithmIdentifier` into its OID and optional raw parameters.
///
/// The parameters are retained as opaque DER (typically `NULL` or an EC curve
/// OID) so callers can inspect them without this crate interpreting them.
pub(crate) fn parse_algorithm_identifier(tlv: &Tlv<'_>) -> Result<(Vec<u32>, Option<Vec<u8>>)> {
    let mut sequence = tlv.as_sequence()?;
    let oid = sequence.read_tlv()?.as_oid()?;
    let params = if sequence.is_empty() {
        None
    } else {
        Some(sequence.read_tlv()?.encoded().to_vec())
    };
    sequence.expect_end()?;
    Ok((oid, params))
}

/// A caller-supplied clock used for certificate time validation.
///
/// Implementations are deterministic so tests can pin time to a fixed instant;
/// the crate never reads the system clock itself.
pub trait Clock {
    /// Return the current time as Unix seconds (seconds since 1970-01-01 UTC).
    fn now_unix_seconds(&self) -> u64;
}

/// A [`Clock`] pinned to a caller-supplied instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FixedClock {
    now: u64,
}

impl FixedClock {
    /// Create a clock that always reports `now` Unix seconds.
    pub const fn new(now: u64) -> Self {
        Self { now }
    }
}

impl Clock for FixedClock {
    fn now_unix_seconds(&self) -> u64 {
        self.now
    }
}

/// Reject a certificate when an extension marked critical is not understood.
///
/// Returns [`SecurityError::CertificateUnknownCriticalExtension`] when any of
/// `unknown_critical_oids` is non-empty; otherwise returns `Ok(())`.
pub(crate) fn reject_unknown_critical(unknown_critical_oids: &[Vec<u32>]) -> Result<()> {
    if unknown_critical_oids.is_empty() {
        Ok(())
    } else {
        Err(SecurityError::CertificateUnknownCriticalExtension)
    }
}
