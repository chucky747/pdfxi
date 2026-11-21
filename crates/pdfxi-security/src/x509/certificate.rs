//! X.509 `Certificate` parsing: TBS fields, validity, key info, and extensions.
//!
//! Parsing is strict and total: every field is bounds-checked through
//! [`crate::der::DerReader`] and malformed input produces a typed error. The
//! complete encoded certificate and its `tbsCertificate` bytes are retained so
//! callers can perform or verify signatures without re-encoding.

use std::fmt;

use crate::crypto::sha::sha256;
use crate::der::{DerReader, Tag, Tlv};
use crate::error::{Result, SecurityError};
use crate::x509::extension::{
    BasicConstraints, ExtendedKeyUsage, Extension, KeyUsage, KeyUsageBit, SubjectAltName,
    parse_extensions,
};
use crate::x509::name::Name;
use crate::x509::{Clock, SignatureAlgorithm, parse_algorithm_identifier};

/// Low-order 64-bit view of a canonical positive `INTEGER` magnitude.
///
/// Serial numbers that fit in 64 bits round-trip exactly; wider values keep
/// their full bytes through [`Certificate::serial_number_bytes`] and only use
/// this view for the legacy [`Certificate::serial_number`] accessor.
fn low_u64(bytes: &[u8]) -> u64 {
    let significant = if bytes.first() == Some(&0) {
        &bytes[1..]
    } else {
        bytes
    };
    let mut value = 0_u64;
    for byte in significant.iter().skip(significant.len().saturating_sub(8)) {
        value = (value << 8) | u64::from(*byte);
    }
    value
}

/// Where a certificate's validity window stands at a given instant.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ValidityStatus {
    /// The instant is within `[notBefore, notAfter]` (both inclusive).
    Valid,
    /// `notBefore` is later than the instant.
    NotYetValid,
    /// `notAfter` is earlier than the instant.
    Expired,
}

impl ValidityStatus {
    /// Return `true` only for [`Self::Valid`].
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

/// The `Validity` sequence of an X.509 certificate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Validity {
    not_before: u64,
    not_after: u64,
}

impl Validity {
    fn parse(tlv: &Tlv<'_>) -> Result<Self> {
        let mut sequence = tlv.as_sequence()?;
        let not_before = sequence.read_tlv()?.as_time()?;
        let not_after = sequence.read_tlv()?.as_time()?;
        sequence.expect_end()?;
        if not_after < not_before {
            return Err(SecurityError::DerInvalid);
        }
        Ok(Self {
            not_before,
            not_after,
        })
    }

    /// Unix seconds at which the certificate becomes valid.
    pub const fn not_before(&self) -> u64 {
        self.not_before
    }

    /// Unix seconds at which the certificate stops being valid.
    pub const fn not_after(&self) -> u64 {
        self.not_after
    }

    /// Classify the validity window at `now` Unix seconds.
    pub const fn status_at(&self, now: u64) -> ValidityStatus {
        if now < self.not_before {
            ValidityStatus::NotYetValid
        } else if now > self.not_after {
            ValidityStatus::Expired
        } else {
            ValidityStatus::Valid
        }
    }
}

/// A parsed `SubjectPublicKeyInfo` — public material only.
#[derive(Clone, PartialEq, Eq)]
pub struct PublicKeyInfo {
    algorithm: Vec<u32>,
    parameters: Option<Vec<u8>>,
    key_bits: Vec<u8>,
    unused_bits: u8,
}

impl PublicKeyInfo {
    fn parse(tlv: &Tlv<'_>) -> Result<Self> {
        let mut sequence = tlv.as_sequence()?;
        let (algorithm, parameters) = parse_algorithm_identifier(&sequence.read_tlv()?)?;
        let key_tlv = sequence.read_tlv()?;
        sequence.expect_end()?;
        let (unused_bits, key_bits) = key_tlv.as_bit_string()?;
        if key_bits.is_empty() {
            return Err(SecurityError::DerInvalid);
        }
        Ok(Self {
            algorithm,
            parameters,
            key_bits: key_bits.to_vec(),
            unused_bits,
        })
    }

    /// Return the public-key algorithm OID arcs.
    pub fn algorithm_oid(&self) -> &[u32] {
        &self.algorithm
    }

    /// Return the raw algorithm parameters, when present.
    pub fn parameters(&self) -> Option<&[u8]> {
        self.parameters.as_deref()
    }

    /// Return the raw `subjectPublicKey` bits (the unused-bit count removed).
    pub fn key_bits(&self) -> &[u8] {
        &self.key_bits
    }

    /// Return the number of unused bits in the final octet of [`Self::key_bits`].
    pub const fn unused_bits(&self) -> u8 {
        self.unused_bits
    }

    /// Return the number of meaningful key bits.
    pub fn bit_len(&self) -> usize {
        self.key_bits.len() * 8 - usize::from(self.unused_bits)
    }
}

impl fmt::Debug for PublicKeyInfo {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PublicKeyInfo")
            .field("algorithm", &crate::x509::oid_string(&self.algorithm))
            .field("bit_len", &self.bit_len())
            .field("key_bits", &format_args!("<{} bytes>", self.key_bits.len()))
            .finish()
    }
}

/// A parsed X.509 `Certificate`.
///
/// The type is immutable and `Clone`, so trust stores and digital-ID stores can
/// share certificates without re-parsing untrusted input. Its `Debug` output
/// contains only metadata (never full DER, signature bytes, or key material).
#[derive(Clone, PartialEq, Eq)]
pub struct Certificate {
    der: Vec<u8>,
    tbs_der: Vec<u8>,
    version: u8,
    serial_number: u64,
    serial_number_bytes: Vec<u8>,
    issuer: Name,
    subject: Name,
    validity: Validity,
    subject_public_key_info: PublicKeyInfo,
    signature_algorithm: SignatureAlgorithm,
    outer_signature_algorithm: SignatureAlgorithm,
    signature_bits: Vec<u8>,
    extensions: Vec<Extension>,
    unknown_critical_extensions: Vec<Vec<u32>>,
}

impl Certificate {
    /// Parse a single DER-encoded `Certificate`.
    ///
    /// Returns [`SecurityError::DerTrailingBytes`] if bytes follow the
    /// top-level `SEQUENCE`, and [`SecurityError::CertificateVersionUnsupported`]
    /// for X.509 versions other than v1, v2, or v3.
    pub fn parse(der: &[u8]) -> Result<Self> {
        let mut top = DerReader::new(der);
        let certificate_tlv = top.read_tlv()?;
        top.expect_end()?;
        let mut certificate = certificate_tlv.as_sequence()?;

        let tbs_tlv = certificate.read_tlv()?;
        let tbs_der = tbs_tlv.encoded().to_vec();
        let mut tbs = tbs_tlv.as_sequence()?;

        let version = Self::parse_version(&mut tbs)?;
        let serial_bytes = tbs.read_tlv()?.as_positive_integer_bytes()?.to_vec();
        // RFC 5280 section 4.1.2.2 caps the serial at 20 octets. Reject wider
        // values instead of allocating an unbounded magnitude for hostile input.
        if serial_bytes.len() > 20 {
            return Err(SecurityError::DerInvalid);
        }
        let serial_number = low_u64(&serial_bytes);
        let (tbs_signature_oid, _) = parse_algorithm_identifier(&tbs.read_tlv()?)?;
        let signature_algorithm = SignatureAlgorithm::from_oid(&tbs_signature_oid);
        let issuer = Name::parse(&tbs.read_tlv()?)?;
        let validity = Validity::parse(&tbs.read_tlv()?)?;
        let subject = Name::parse(&tbs.read_tlv()?)?;
        let subject_public_key_info = PublicKeyInfo::parse(&tbs.read_tlv()?)?;

        let issuer_unique_tag = Tag::context(1, false)
            .ok_or(SecurityError::DerInvalid)?
            .byte();
        let subject_unique_tag = Tag::context(2, false)
            .ok_or(SecurityError::DerInvalid)?
            .byte();
        let extensions_tag = Tag::context(3, true)
            .ok_or(SecurityError::DerInvalid)?
            .byte();
        let mut extensions = Vec::new();
        let mut unknown_critical_extensions = Vec::new();
        let mut seen_issuer_unique_id = false;
        let mut seen_subject_unique_id = false;
        let mut seen_extensions = false;
        while let Some(tag) = tbs.peek_tag() {
            let byte = tag.byte();
            if byte == issuer_unique_tag {
                if seen_issuer_unique_id {
                    return Err(SecurityError::CertificateDuplicateField("issuerUniqueID"));
                }
                seen_issuer_unique_id = true;
                // issuerUniqueID is opaque for this subset.
                tbs.read_tlv()?;
            } else if byte == subject_unique_tag {
                if seen_subject_unique_id {
                    return Err(SecurityError::CertificateDuplicateField("subjectUniqueID"));
                }
                seen_subject_unique_id = true;
                // subjectUniqueID is opaque for this subset.
                tbs.read_tlv()?;
            } else if byte == extensions_tag {
                if seen_extensions {
                    return Err(SecurityError::CertificateDuplicateField("extensions"));
                }
                seen_extensions = true;
                if version < 2 {
                    return Err(SecurityError::DerInvalid);
                }
                let wrapper = tbs.read_tlv()?;
                let (parsed, unknown) = parse_extensions(&wrapper)?;
                extensions = parsed;
                unknown_critical_extensions = unknown;
            } else {
                break;
            }
        }
        tbs.expect_end()?;

        let (outer_signature_oid, _) = parse_algorithm_identifier(&certificate.read_tlv()?)?;
        let outer_signature_algorithm = SignatureAlgorithm::from_oid(&outer_signature_oid);
        if outer_signature_algorithm != signature_algorithm {
            return Err(SecurityError::DerInvalid);
        }
        let signature_tlv = certificate.read_tlv()?;
        let (_, signature_bits) = signature_tlv.as_bit_string()?;
        certificate.expect_end()?;

        Ok(Self {
            der: der.to_vec(),
            tbs_der,
            version,
            serial_number,
            serial_number_bytes: serial_bytes,
            issuer,
            subject,
            validity,
            subject_public_key_info,
            signature_algorithm,
            outer_signature_algorithm,
            signature_bits: signature_bits.to_vec(),
            extensions,
            unknown_critical_extensions,
        })
    }

    fn parse_version(tbs: &mut DerReader<'_>) -> Result<u8> {
        let expected = Tag::context(0, true).ok_or(SecurityError::DerInvalid)?;
        if tbs.peek_tag() != Some(expected) {
            return Ok(0);
        }
        let mut wrapper = tbs.read_tagged(expected)?;
        let raw = wrapper.read_tlv()?.as_integer_u64()?;
        wrapper.expect_end()?;
        match raw {
            0..=2 => Ok(raw as u8),
            other => Err(SecurityError::CertificateVersionUnsupported(other)),
        }
    }

    /// Return the complete encoded certificate.
    pub fn der(&self) -> &[u8] {
        &self.der
    }

    /// Return the encoded `tbsCertificate` covered by the signature.
    pub fn tbs_der(&self) -> &[u8] {
        &self.tbs_der
    }

    /// Return the X.509 version (`0` = v1, `1` = v2, `2` = v3).
    pub const fn version(&self) -> u8 {
        self.version
    }

    /// Return the certificate serial number as a `u64`.
    ///
    /// RFC 5280 permits serial numbers wider than 64 bits and Windows-issued
    /// certificates commonly use them. This accessor is a lossless view for any
    /// serial that fits; for a wider serial it returns the low-order 64 bits.
    /// Use [`Self::serial_number_bytes`] for identity, display, or any decision
    /// that must distinguish the full value.
    pub const fn serial_number(&self) -> u64 {
        self.serial_number
    }

    /// Return the full canonical `INTEGER` content of the serial number.
    ///
    /// The bytes are the DER content octets: non-empty, positive, minimally
    /// encoded, and at most the 20 octets RFC 5280 allows. A leading `0x00` is
    /// present only when DER requires it for a high-bit magnitude.
    pub fn serial_number_bytes(&self) -> &[u8] {
        &self.serial_number_bytes
    }

    /// Return the issuer name.
    pub fn issuer(&self) -> &Name {
        &self.issuer
    }

    /// Return the subject name.
    pub fn subject(&self) -> &Name {
        &self.subject
    }

    /// Return the validity window.
    pub const fn validity(&self) -> &Validity {
        &self.validity
    }

    /// Return the subject public key information.
    pub const fn subject_public_key_info(&self) -> &PublicKeyInfo {
        &self.subject_public_key_info
    }

    /// Return the `tbsCertificate.signature` algorithm.
    pub const fn signature_algorithm(&self) -> &SignatureAlgorithm {
        &self.signature_algorithm
    }

    /// Return the outer `signatureAlgorithm` (equal to [`Self::signature_algorithm`]).
    pub const fn outer_signature_algorithm(&self) -> &SignatureAlgorithm {
        &self.outer_signature_algorithm
    }

    /// Return the raw signature bits, excluding the BIT STRING unused-bit octet.
    pub fn signature_bits(&self) -> &[u8] {
        &self.signature_bits
    }

    /// Return all parsed extensions in encoded order.
    pub fn extensions(&self) -> &[Extension] {
        &self.extensions
    }

    /// Return OIDs of critical extensions this subset cannot interpret.
    pub fn unknown_critical_extensions(&self) -> &[Vec<u32>] {
        &self.unknown_critical_extensions
    }

    /// Return the basic constraints extension, when present.
    pub fn basic_constraints(&self) -> Option<BasicConstraints> {
        self.extensions
            .iter()
            .find_map(|extension| match extension.value() {
                crate::x509::extension::ExtensionValue::BasicConstraints(constraints) => {
                    Some(*constraints)
                }
                _ => None,
            })
    }

    /// Return the key usage extension, when present.
    pub fn key_usage(&self) -> Option<KeyUsage> {
        self.extensions
            .iter()
            .find_map(|extension| match extension.value() {
                crate::x509::extension::ExtensionValue::KeyUsage(usage) => Some(*usage),
                _ => None,
            })
    }

    /// Return the subject alternative name extension, when present.
    pub fn subject_alt_name(&self) -> Option<&SubjectAltName> {
        self.extensions
            .iter()
            .find_map(|extension| match extension.value() {
                crate::x509::extension::ExtensionValue::SubjectAltName(names) => Some(names),
                _ => None,
            })
    }

    /// Return the extended key usage extension, when present.
    pub fn extended_key_usage(&self) -> Option<&ExtendedKeyUsage> {
        self.extensions
            .iter()
            .find_map(|extension| match extension.value() {
                crate::x509::extension::ExtensionValue::ExtendedKeyUsage(eku) => Some(eku),
                _ => None,
            })
    }

    /// Return `true` when the certificate permits `usage`.
    ///
    /// RFC 5280 treats an absent `keyUsage` extension as unconstrained, so this
    /// method returns `true` in that case.
    pub fn allows_usage(&self, usage: KeyUsageBit) -> bool {
        self.key_usage()
            .is_none_or(|key_usage| key_usage.permits(usage))
    }

    /// Return `true` when the certificate is marked as a CA.
    ///
    /// An absent `basicConstraints` extension means it is not a CA.
    pub fn is_ca(&self) -> bool {
        match self.basic_constraints() {
            Some(constraints) => constraints.is_ca(),
            None => false,
        }
    }

    /// Classify validity at `now` Unix seconds.
    pub const fn validity_status_at(&self, now: u64) -> ValidityStatus {
        self.validity.status_at(now)
    }

    /// Return `true` when the certificate is valid at the clock's instant.
    pub fn is_valid_at(&self, clock: &dyn Clock) -> bool {
        self.validity_status_at(clock.now_unix_seconds()).is_valid()
    }

    /// Compute the SHA-256 fingerprint of the complete encoded certificate.
    pub fn fingerprint_sha256(&self) -> [u8; 32] {
        sha256(&self.der)
    }
}

impl fmt::Debug for Certificate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Certificate")
            .field("version", &self.version)
            .field("serial_number", &self.serial_number)
            .field("issuer", &self.issuer.display_rfc4514())
            .field("subject", &self.subject.display_rfc4514())
            .field("validity", &self.validity)
            .field("signature_algorithm", &self.signature_algorithm.as_str())
            .field("extensions", &self.extensions.len())
            .field("der", &format_args!("<{} bytes>", self.der.len()))
            .field(
                "signature_bits",
                &format_args!("<{} bytes>", self.signature_bits.len()),
            )
            .finish()
    }
}
