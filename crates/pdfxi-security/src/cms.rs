//! CMS/PKCS#7 `SignedData` creation and validation (RFC 5652 subset).
//!
//! This module builds and verifies detached-or-encapsulated CMS `SignedData`
//! blobs using the DER primitives in [`crate::der`]. The subset implemented is:
//!
//! - `ContentInfo` wrapping `signedData`;
//! - `SignedData` with version 1, one digest algorithm, `encapContentInfo`
//!   carrying an OCTET STRING, the signer certificate in `[0]`, and one
//!   `SignerInfo`;
//! - `SignerInfo` using `issuerAndSerialNumber`, the signed attributes
//!   `contentType`, `messageDigest`, and `signingTime`, and an RSA signature
//!   over the DER `SET OF` encoding of those attributes.
//!
//! All signing and verifying key material stays behind the [`SignatureProvider`]
//! trait, so this crate never stores or logs a private key. [`modpow_u128`] is a
//! small documented raw-modular-exponentiation primitive used by the test
//! provider; it is **not** a production RSA implementation and must not be used
//! with real keys.
//!
//! The builder is deterministic: attribute sets are DER-sorted and every field
//! has a canonical encoding, so identical inputs always produce identical bytes.
//!
//! # Verification scope and trust boundary
//!
//! Verification establishes *cryptographic integrity only*: the content digest
//! must match the `messageDigest` signed attribute, and the supplied
//! [`SignatureProvider`] must accept the signature over the DER-encoded
//! `signedAttrs` set. On success the returned [`SignatureInfo`] carries
//! [`SignatureStatus::Valid`] and [`SignatureCoverage::WholeDocument`]; in this
//! module those values mean precisely "the CMS signature is cryptographically
//! valid over the signed attributes and the whole encapsulated/detached
//! content". They do **not** assert trust in the signer: certificate-path
//! building, trust anchors, revocation/CRL checks, identity policy, and RFC 3161
//! trusted timestamps are explicitly out of scope and must be supplied by
//! higher layers. The `signingTime` attribute is decoded as untrusted
//! self-asserted metadata, not as a trusted timestamp token.

use crate::crypto::constant_time_eq;
use crate::crypto::sha::{sha256, sha384, sha512};
use crate::der::{
    DerReader, Tag, Tlv, encode_context, encode_integer, encode_null, encode_octet_string,
    encode_oid, encode_sequence, encode_set, encode_time, write_tlv,
};
use crate::error::{Result, SecurityError};
use crate::signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};

use std::fmt;

/// OID `1.2.840.113549.1.7.1` — `data`.
const OID_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 1];
/// OID `1.2.840.113549.1.7.2` — `signedData`.
const OID_SIGNED_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 2];
/// OID `1.2.840.113549.1.9.3` — `contentType` attribute.
const OID_CONTENT_TYPE: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 3];
/// OID `1.2.840.113549.1.9.4` — `messageDigest` attribute.
const OID_MESSAGE_DIGEST: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 4];
/// OID `1.2.840.113549.1.9.5` — `signingTime` attribute.
const OID_SIGNING_TIME: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 5];
/// OID `1.2.840.113549.1.1.11` — `sha256WithRSAEncryption`.
const OID_SHA256_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 11];
/// OID `1.2.840.113549.1.1.12` — `sha384WithRSAEncryption`.
const OID_SHA384_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 12];
/// OID `1.2.840.113549.1.1.13` — `sha512WithRSAEncryption`.
const OID_SHA512_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 13];
/// OID `2.16.840.1.101.3.4.2.1` — SHA-256.
const OID_SHA256: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 1];
/// OID `2.16.840.1.101.3.4.2.2` — SHA-384.
const OID_SHA384: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 2];
/// OID `2.16.840.1.101.3.4.2.3` — SHA-512.
const OID_SHA512: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 3];

fn digest_oid(algorithm: DigestAlgorithm) -> Result<&'static [u32]> {
    match algorithm {
        DigestAlgorithm::Sha256 => Ok(&OID_SHA256),
        DigestAlgorithm::Sha384 => Ok(&OID_SHA384),
        DigestAlgorithm::Sha512 => Ok(&OID_SHA512),
        DigestAlgorithm::Sha1 => Err(SecurityError::UnsupportedDigestAlgorithm(
            "1.3.14.3.2.26".to_string(),
        )),
    }
}

fn signature_oid(algorithm: DigestAlgorithm) -> Result<&'static [u32]> {
    match algorithm {
        DigestAlgorithm::Sha256 => Ok(&OID_SHA256_RSA),
        DigestAlgorithm::Sha384 => Ok(&OID_SHA384_RSA),
        DigestAlgorithm::Sha512 => Ok(&OID_SHA512_RSA),
        DigestAlgorithm::Sha1 => Err(SecurityError::UnsupportedDigestAlgorithm(
            "1.3.14.3.2.26".to_string(),
        )),
    }
}

fn digest_from_oid(oid: &[u32]) -> Option<DigestAlgorithm> {
    if oid == OID_SHA256 {
        Some(DigestAlgorithm::Sha256)
    } else if oid == OID_SHA384 {
        Some(DigestAlgorithm::Sha384)
    } else if oid == OID_SHA512 {
        Some(DigestAlgorithm::Sha512)
    } else {
        None
    }
}

fn signature_digest_from_oid(oid: &[u32]) -> Option<DigestAlgorithm> {
    if oid == OID_SHA256_RSA {
        Some(DigestAlgorithm::Sha256)
    } else if oid == OID_SHA384_RSA {
        Some(DigestAlgorithm::Sha384)
    } else if oid == OID_SHA512_RSA {
        Some(DigestAlgorithm::Sha512)
    } else {
        None
    }
}

fn oid_string(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// Compute a digest with one of the CMS-supported algorithms.
///
/// Returns [`SecurityError::UnsupportedDigestAlgorithm`] for SHA-1, which is
/// intentionally excluded from new CMS signatures.
pub fn digest(algorithm: DigestAlgorithm, data: &[u8]) -> Result<Vec<u8>> {
    match algorithm {
        DigestAlgorithm::Sha256 => Ok(sha256(data).to_vec()),
        DigestAlgorithm::Sha384 => Ok(sha384(data)),
        DigestAlgorithm::Sha512 => Ok(sha512(data)),
        DigestAlgorithm::Sha1 => Err(SecurityError::UnsupportedDigestAlgorithm(
            "1.3.14.3.2.26".to_string(),
        )),
    }
}

fn context0() -> Tag {
    Tag::context(0, true).unwrap_or(Tag::SEQUENCE)
}

fn set_to_implicit(set_der: &[u8]) -> Result<Vec<u8>> {
    if set_der.first() != Some(&Tag::SET.byte()) {
        return Err(SecurityError::DerInvalid);
    }
    let mut out = vec![context0().byte()];
    out.extend_from_slice(&set_der[1..]);
    Ok(out)
}

fn implicit_to_set(tlv: &Tlv<'_>) -> Result<Vec<u8>> {
    if tlv.tag() != context0() {
        return Err(SecurityError::DerUnexpectedTag {
            expected: context0().byte(),
            found: tlv.tag().byte(),
        });
    }
    let mut out = vec![Tag::SET.byte()];
    out.extend_from_slice(&tlv.encoded()[1..]);
    Ok(out)
}

fn validate_single_tlv(bytes: &[u8], expected: Tag) -> Result<()> {
    let mut reader = DerReader::new(bytes);
    let tlv = reader.read_tlv()?;
    if tlv.tag() != expected {
        return Err(SecurityError::DerUnexpectedTag {
            expected: expected.byte(),
            found: tlv.tag().byte(),
        });
    }
    reader.expect_end()
}

fn encode_algorithm_identifier(oid: &[u32]) -> Result<Vec<u8>> {
    let mut children = encode_oid(oid)?;
    children.extend_from_slice(&encode_null());
    Ok(encode_sequence(&children))
}

fn parse_algorithm_identifier(tlv: &Tlv<'_>) -> Result<Vec<u32>> {
    let mut sequence = tlv.as_sequence()?;
    let oid = sequence.read_tlv()?.as_oid()?;
    if !sequence.is_empty() {
        sequence.read_tlv()?.as_null()?;
    }
    sequence.expect_end()?;
    Ok(oid)
}

fn encode_attribute(oid: &[u32], value_der: &[u8]) -> Result<Vec<u8>> {
    let mut children = encode_oid(oid)?;
    write_tlv(&mut children, Tag::SET, value_der);
    Ok(encode_sequence(&children))
}

/// A CMS signer identifier (`IssuerAndSerialNumber`).
///
/// The issuer is carried as opaque, already-DER-encoded `Name` bytes (normally
/// an X.509 `RDNSequence` `SEQUENCE`). Certificate and key parsing are the
/// caller's responsibility; this crate never interprets X.509 structures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignerIdentifier {
    issuer_der: Vec<u8>,
    serial_number: u64,
}

impl SignerIdentifier {
    /// Build a signer identifier from an opaque DER issuer name and a serial.
    ///
    /// Returns [`SecurityError::DerInvalid`] if `issuer_der` is not exactly one
    /// well-formed DER `SEQUENCE`.
    pub fn new(issuer_der: Vec<u8>, serial_number: u64) -> Result<Self> {
        validate_single_tlv(&issuer_der, Tag::SEQUENCE)?;
        Ok(Self {
            issuer_der,
            serial_number,
        })
    }

    /// Return the opaque DER issuer name.
    pub fn issuer_der(&self) -> &[u8] {
        &self.issuer_der
    }

    /// Return the certificate serial number.
    pub const fn serial_number(&self) -> u64 {
        self.serial_number
    }
}

/// Inputs to [`build_signed_data`].
///
/// All byte fields are copied into the output; the caller retains ownership and
/// no content is ever logged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedDataParams {
    /// Content bytes to encapsulate and digest.
    pub content: Vec<u8>,
    /// Digest algorithm; SHA-256, SHA-384, and SHA-512 are supported.
    pub digest_algorithm: DigestAlgorithm,
    /// Signer identifier embedded in `SignerInfo`.
    pub signer: SignerIdentifier,
    /// Opaque DER-encoded certificate inserted into the `certificates` field.
    pub certificate_der: Vec<u8>,
    /// Signing time as Unix seconds; encoded as `UTCTime` or `GeneralizedTime`.
    pub signing_time: u64,
}

/// Boundary for signing key material and signer metadata.
///
/// Implementors own the private key (for [`SignatureProvider::sign`]) and any
/// public key used by [`SignatureProvider::verify`], as well as the certificate
/// and display name. Keeping this behind a trait means `pdfxi-security` never
/// stores, serializes, or logs a key. A deterministic in-memory implementation
/// is provided by [`InMemorySignatureProvider`] for contract testing.
pub trait SignatureProvider {
    /// Produce a signature over the DER `signedAttrs` bytes for `algorithm`.
    fn sign(&self, algorithm: DigestAlgorithm, tbs: &[u8]) -> Result<Vec<u8>>;

    /// Return `true` only when `signature` is valid over `tbs`.
    fn verify(&self, algorithm: DigestAlgorithm, tbs: &[u8], signature: &[u8]) -> Result<bool>;

    /// Human-readable signer name copied into verification metadata.
    fn signer_name(&self) -> &str;

    /// Opaque DER certificate embedded in and checked against `SignedData`.
    fn certificate_der(&self) -> &[u8];

    /// Signer identifier embedded in and checked against `SignedData`.
    fn signer_identifier(&self) -> &SignerIdentifier;
}

/// Raw modular exponentiation `base^exponent mod modulus` for small moduli.
///
/// This is a square-and-multiply implementation over `u128`. It returns `None`
/// when `modulus <= 1` or when an intermediate product overflows `u128`; callers
/// with large RSA keys must use a real big-integer backend instead. It exists
/// to give the in-memory test provider a deterministic raw-RSA path with
/// published small-key vectors; it is **not** safe for production keys.
pub fn modpow_u128(base: u128, exponent: u128, modulus: u128) -> Option<u128> {
    if modulus <= 1 {
        return None;
    }
    let mut result = 1u128 % modulus;
    let mut factor = base % modulus;
    let mut remaining = exponent;
    while remaining > 0 {
        if remaining & 1 == 1 {
            result = mulmod(result, factor, modulus)?;
        }
        remaining >>= 1;
        if remaining > 0 {
            factor = mulmod(factor, factor, modulus)?;
        }
    }
    Some(result)
}

fn mulmod(left: u128, right: u128, modulus: u128) -> Option<u128> {
    left.checked_mul(right).map(|product| product % modulus)
}

fn gcd_u128(mut left: u128, mut right: u128) -> u128 {
    while right != 0 {
        let remainder = left % right;
        left = right;
        right = remainder;
    }
    left
}

fn mod_inverse_u128(value: u128, modulus: u128) -> Option<u128> {
    if modulus <= 1 {
        return None;
    }
    let modulus_signed = modulus as i128;
    let mut old_t = 0i128;
    let mut new_t = 1i128;
    let mut old_r = modulus_signed;
    let mut new_r = (value % modulus) as i128;
    while new_r != 0 {
        let quotient = old_r / new_r;
        let next_t = old_t - quotient * new_t;
        old_t = new_t;
        new_t = next_t;
        let next_r = old_r - quotient * new_r;
        old_r = new_r;
        new_r = next_r;
    }
    if old_r != 1 {
        return None;
    }
    let mut inverse = old_t % modulus_signed;
    if inverse < 0 {
        inverse += modulus_signed;
    }
    Some(inverse as u128)
}

// Two fixed, publicly documented primes. The resulting ~40-bit modulus is used
// only by the test provider and offers no security whatsoever.
const TOY_P: u128 = 1_000_003;
const TOY_Q: u128 = 1_000_033;
const TOY_E: u128 = 65_537;

fn message_representative(digest_bytes: &[u8], modulus: u128) -> u128 {
    let mut value = 0u128;
    for byte in digest_bytes {
        value = ((value << 8) | u128::from(*byte)) % modulus;
    }
    value
}

/// Deterministic in-memory [`SignatureProvider`] used for contract tests.
///
/// It performs textbook (unpadded) RSA with the small public primes
/// `1_000_003` and `1_000_033` through [`modpow_u128`]. The message
/// representative is the digest reduced modulo the toy modulus, so signatures
/// are fixed-length, deterministic, and sensitive to every input byte. This is
/// a **test double**: it provides no authenticity against an adversary and must
/// never protect real documents.
#[derive(Clone)]
pub struct InMemorySignatureProvider {
    signer: SignerIdentifier,
    certificate_der: Vec<u8>,
    signer_name: String,
    modulus: u128,
    public_exponent: u128,
    private_exponent: u128,
}

impl InMemorySignatureProvider {
    /// Create a deterministic provider for the given signer and certificate.
    ///
    /// Returns an error if the certificate is not a single DER `SEQUENCE` or if
    /// the fixed toy key parameters cannot be inverted.
    pub fn new(
        signer: SignerIdentifier,
        certificate_der: Vec<u8>,
        signer_name: impl Into<String>,
    ) -> Result<Self> {
        validate_single_tlv(&certificate_der, Tag::SEQUENCE)?;
        let p_minus_one = TOY_P - 1;
        let q_minus_one = TOY_Q - 1;
        let lambda = p_minus_one / gcd_u128(p_minus_one, q_minus_one) * q_minus_one;
        let private_exponent = mod_inverse_u128(TOY_E, lambda).ok_or_else(|| {
            SecurityError::Configuration("in-memory toy RSA key parameters are invalid".to_string())
        })?;
        let modulus = TOY_P.checked_mul(TOY_Q).ok_or_else(|| {
            SecurityError::Configuration("in-memory toy RSA modulus overflowed".to_string())
        })?;
        Ok(Self {
            signer,
            certificate_der,
            signer_name: signer_name.into(),
            modulus,
            public_exponent: TOY_E,
            private_exponent,
        })
    }
}

impl SignatureProvider for InMemorySignatureProvider {
    fn sign(&self, algorithm: DigestAlgorithm, tbs: &[u8]) -> Result<Vec<u8>> {
        let digest_bytes = digest(algorithm, tbs)?;
        let representative = message_representative(&digest_bytes, self.modulus);
        let signature = modpow_u128(representative, self.private_exponent, self.modulus)
            .ok_or_else(|| {
                SecurityError::Configuration("in-memory toy RSA signing overflowed".to_string())
            })?;
        Ok(signature.to_be_bytes().to_vec())
    }

    fn verify(&self, algorithm: DigestAlgorithm, tbs: &[u8], signature: &[u8]) -> Result<bool> {
        if signature.len() != 16 {
            return Ok(false);
        }
        let mut value = 0u128;
        for byte in signature {
            value = value
                .checked_mul(256)
                .and_then(|number| number.checked_add(u128::from(*byte)))
                .ok_or(SecurityError::SignatureInvalid)?;
        }
        if value >= self.modulus {
            return Ok(false);
        }
        let recovered =
            modpow_u128(value, self.public_exponent, self.modulus).ok_or_else(|| {
                SecurityError::Configuration(
                    "in-memory toy RSA verification overflowed".to_string(),
                )
            })?;
        let digest_bytes = digest(algorithm, tbs)?;
        let expected = message_representative(&digest_bytes, self.modulus);
        Ok(constant_time_eq(
            &recovered.to_be_bytes(),
            &expected.to_be_bytes(),
        ))
    }

    fn signer_name(&self) -> &str {
        &self.signer_name
    }

    fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    fn signer_identifier(&self) -> &SignerIdentifier {
        &self.signer
    }
}

impl fmt::Debug for InMemorySignatureProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("InMemorySignatureProvider")
            .field("signer", &self.signer)
            .field("signer_name", &self.signer_name)
            .field(
                "certificate_der",
                &format_args!("<{} bytes>", self.certificate_der.len()),
            )
            .field("key", &"<redacted>")
            .finish()
    }
}

/// Build a deterministic DER `ContentInfo` / `SignedData` blob.
///
/// The content is encapsulated in `encapContentInfo`, the signer certificate is
/// placed in the `[0] certificates` field, and the three signed attributes are
/// DER-sorted before being signed. Signing itself is delegated to `provider`.
///
/// # Errors
///
/// - [`SecurityError::UnsupportedDigestAlgorithm`] for SHA-1.
/// - DER errors for a malformed signer identifier or certificate.
/// - Whatever error the [`SignatureProvider`] returns while signing.
pub fn build_signed_data(
    params: &SignedDataParams,
    provider: &dyn SignatureProvider,
) -> Result<Vec<u8>> {
    let digest_algorithm = params.digest_algorithm;
    let digest_oid = digest_oid(digest_algorithm)?;
    let signature_algorithm_oid = signature_oid(digest_algorithm)?;
    validate_single_tlv(&params.certificate_der, Tag::SEQUENCE)?;

    let content_digest = digest(digest_algorithm, &params.content)?;
    let content_type_attribute = encode_attribute(&OID_CONTENT_TYPE, &encode_oid(&OID_DATA)?)?;
    let message_digest_attribute =
        encode_attribute(&OID_MESSAGE_DIGEST, &encode_octet_string(&content_digest))?;
    let signing_time_attribute =
        encode_attribute(&OID_SIGNING_TIME, &encode_time(params.signing_time)?)?;

    let mut attributes = vec![
        content_type_attribute,
        message_digest_attribute,
        signing_time_attribute,
    ];
    attributes.sort();
    let mut signed_attributes_content = Vec::new();
    for attribute in &attributes {
        signed_attributes_content.extend_from_slice(attribute);
    }
    let signed_attributes_set = encode_set(&signed_attributes_content);
    let signature = provider.sign(digest_algorithm, &signed_attributes_set)?;

    let mut signer_children = Vec::new();
    signer_children.extend_from_slice(&encode_integer(1));
    let mut sid_children = Vec::with_capacity(16);
    sid_children.extend_from_slice(params.signer.issuer_der());
    sid_children.extend_from_slice(&encode_integer(params.signer.serial_number()));
    signer_children.extend_from_slice(&encode_sequence(&sid_children));
    signer_children.extend_from_slice(&encode_algorithm_identifier(digest_oid)?);
    signer_children.extend_from_slice(&set_to_implicit(&signed_attributes_set)?);
    signer_children.extend_from_slice(&encode_algorithm_identifier(signature_algorithm_oid)?);
    signer_children.extend_from_slice(&encode_octet_string(&signature));
    let signer_info = encode_sequence(&signer_children);

    let digest_algorithms = encode_set(&encode_algorithm_identifier(digest_oid)?);
    let explicit_content = encode_context(0, true, &encode_octet_string(&params.content))
        .ok_or(SecurityError::DerInvalid)?;
    let mut encap_children = encode_oid(&OID_DATA)?;
    encap_children.extend_from_slice(&explicit_content);
    let encap_content_info = encode_sequence(&encap_children);

    // X.509 Certificate is a SEQUENCE; [0] IMPLICIT replaces only its tag octet.
    let mut certificate_field = vec![context0().byte()];
    certificate_field.extend_from_slice(&params.certificate_der[1..]);

    let signer_infos = encode_set(&signer_info);
    let mut signed_data_children = Vec::new();
    signed_data_children.extend_from_slice(&encode_integer(1));
    signed_data_children.extend_from_slice(&digest_algorithms);
    signed_data_children.extend_from_slice(&encap_content_info);
    signed_data_children.extend_from_slice(&certificate_field);
    signed_data_children.extend_from_slice(&signer_infos);
    let signed_data = encode_sequence(&signed_data_children);

    let explicit_signed_data =
        encode_context(0, true, &signed_data).ok_or(SecurityError::DerInvalid)?;
    let mut content_info_children = encode_oid(&OID_SIGNED_DATA)?;
    content_info_children.extend_from_slice(&explicit_signed_data);
    Ok(encode_sequence(&content_info_children))
}

/// Verify a CMS blob whose content is encapsulated in `encapContentInfo`.
///
/// On success returns a [`SignatureInfo`] whose validity is
/// [`SignatureStatus::Valid`] and whose coverage is
/// [`SignatureCoverage::WholeDocument`]. These mean that the content digest
/// matches the `messageDigest` signed attribute and that `provider` accepted the
/// signature over the DER-encoded `signedAttrs` set — i.e. cryptographic
/// validity of the signature over the signed attributes and the whole
/// encapsulated content.
///
/// # Trust boundary
///
/// This function does not establish trust in the signer. No certificate chain is
/// built, no trust anchor or revocation status is consulted, and `signingTime`
/// is untrusted self-asserted metadata rather than an RFC 3161 timestamp. Callers
/// requiring authenticity or non-repudiation must perform those checks in a
/// higher layer before treating the result as trusted. See the [module docs][crate::cms]
/// for the full scope.
pub fn verify_signed_data(der: &[u8], provider: &dyn SignatureProvider) -> Result<SignatureInfo> {
    verify_inner(der, None, provider)
}

/// Verify a CMS blob against externally supplied content (detached mode).
///
/// The embedded `eContent`, if present, is ignored; `content` is the byte string
/// whose digest must match the `messageDigest` signed attribute. The same
/// cryptographic-only guarantee and trust boundary described on
/// [`verify_signed_data`] apply here.
pub fn verify_signed_data_detached(
    der: &[u8],
    content: &[u8],
    provider: &dyn SignatureProvider,
) -> Result<SignatureInfo> {
    verify_inner(der, Some(content), provider)
}

#[allow(clippy::too_many_lines)]
fn verify_inner(
    der: &[u8],
    external_content: Option<&[u8]>,
    provider: &dyn SignatureProvider,
) -> Result<SignatureInfo> {
    let mut top = DerReader::new(der);
    let mut content_info = top.read_sequence()?;
    top.expect_end()?;

    let content_type_oid = content_info.read_tlv()?.as_oid()?;
    if content_type_oid != OID_SIGNED_DATA {
        return Err(SecurityError::DerInvalid);
    }
    let mut signed_data_wrapper = content_info.read_tagged(context0())?;
    content_info.expect_end()?;
    let mut signed_data = signed_data_wrapper.read_sequence()?;
    signed_data_wrapper.expect_end()?;

    let version = signed_data.read_tlv()?.as_integer_u64()?;
    if version != 1 {
        return Err(SecurityError::DerInvalid);
    }

    let mut digest_algorithms = signed_data.read_set()?;
    let digest_algorithm_tlv = digest_algorithms.read_tlv()?;
    digest_algorithms.expect_end()?;
    let digest_oid = parse_algorithm_identifier(&digest_algorithm_tlv)?;
    let digest_algorithm = digest_from_oid(&digest_oid)
        .ok_or_else(|| SecurityError::UnsupportedDigestAlgorithm(oid_string(&digest_oid)))?;

    let mut encap_content_info = signed_data.read_sequence()?;
    let encap_content_type = encap_content_info.read_tlv()?.as_oid()?;
    if encap_content_type != OID_DATA {
        return Err(SecurityError::DerInvalid);
    }
    let embedded_content = if encap_content_info.peek_tag() == Some(context0()) {
        let mut wrapper = encap_content_info.read_tagged(context0())?;
        let bytes = wrapper.read_tlv()?.as_octets()?.to_vec();
        wrapper.expect_end()?;
        Some(bytes)
    } else {
        None
    };
    encap_content_info.expect_end()?;
    let content: &[u8] = match external_content {
        Some(bytes) => bytes,
        None => embedded_content
            .as_deref()
            .ok_or(SecurityError::DerInvalid)?,
    };

    let certificate_field = signed_data.read_tlv()?;
    if certificate_field.tag() != context0() {
        return Err(SecurityError::CertificateMissing);
    }
    let mut certificate_der = vec![Tag::SEQUENCE.byte()];
    certificate_der.extend_from_slice(&certificate_field.encoded()[1..]);
    validate_single_tlv(&certificate_der, Tag::SEQUENCE)?;
    if certificate_der.as_slice() != provider.certificate_der() {
        return Err(SecurityError::CertificateMismatch);
    }

    let mut signer_infos = signed_data.read_set()?;
    let signer_info_tlv = signer_infos.read_tlv()?;
    signer_infos.expect_end()?;
    let mut signer_info = signer_info_tlv.as_sequence()?;

    let signer_version = signer_info.read_tlv()?.as_integer_u64()?;
    if signer_version != 1 {
        return Err(SecurityError::DerInvalid);
    }

    let signer_id_tlv = signer_info.read_tlv()?;
    let mut signer_id = signer_id_tlv.as_sequence()?;
    let issuer = signer_id.read_tlv()?;
    let serial_number = signer_id.read_tlv()?.as_integer_u64()?;
    signer_id.expect_end()?;
    let expected_signer = provider.signer_identifier();
    if issuer.encoded() != expected_signer.issuer_der()
        || serial_number != expected_signer.serial_number()
    {
        return Err(SecurityError::SignerMismatch);
    }

    let signer_digest_oid = parse_algorithm_identifier(&signer_info.read_tlv()?)?;
    if signer_digest_oid != digest_oid {
        return Err(SecurityError::DerInvalid);
    }

    let signed_attributes_tlv = signer_info.read_tlv()?;
    let signed_attributes_set = implicit_to_set(&signed_attributes_tlv)?;
    let mut set_wrapper = DerReader::new(&signed_attributes_set);
    let mut attributes = set_wrapper.read_set()?;
    set_wrapper.expect_end()?;

    let mut content_type: Option<Vec<u32>> = None;
    let mut message_digest: Option<Vec<u8>> = None;
    let mut signing_time: Option<u64> = None;
    while !attributes.is_empty() {
        let attribute_tlv = attributes.read_tlv()?;
        let mut attribute = attribute_tlv.as_sequence()?;
        let attribute_oid = attribute.read_tlv()?.as_oid()?;
        let mut values = attribute.read_set()?;
        let value = values.read_tlv()?;
        values.expect_end()?;
        attribute.expect_end()?;
        if attribute_oid == OID_CONTENT_TYPE {
            content_type = Some(value.as_oid()?);
        } else if attribute_oid == OID_MESSAGE_DIGEST {
            message_digest = Some(value.as_octets()?.to_vec());
        } else if attribute_oid == OID_SIGNING_TIME {
            signing_time = Some(value.as_time()?);
        }
    }
    let content_type = content_type.ok_or(SecurityError::MissingSignedAttribute("contentType"))?;
    let message_digest =
        message_digest.ok_or(SecurityError::MissingSignedAttribute("messageDigest"))?;
    let signing_time = signing_time.ok_or(SecurityError::MissingSignedAttribute("signingTime"))?;
    if content_type != encap_content_type {
        return Err(SecurityError::SignatureInvalid);
    }

    let signature_algorithm_oid = parse_algorithm_identifier(&signer_info.read_tlv()?)?;
    let signature_digest =
        signature_digest_from_oid(&signature_algorithm_oid).ok_or_else(|| {
            SecurityError::UnsupportedSignatureAlgorithm(oid_string(&signature_algorithm_oid))
        })?;
    if signature_digest != digest_algorithm {
        return Err(SecurityError::DerInvalid);
    }

    let signature = signer_info.read_tlv()?.as_octets()?;
    if !signer_info.is_empty() {
        let unsigned_attributes = signer_info.read_tlv()?;
        if Some(unsigned_attributes.tag()) != Tag::context(1, true) {
            return Err(SecurityError::DerInvalid);
        }
    }
    signer_info.expect_end()?;
    signed_data.expect_end()?;

    let computed_digest = digest(digest_algorithm, content)?;
    if !constant_time_eq(&computed_digest, &message_digest) {
        return Err(SecurityError::TamperedContent);
    }
    if !provider.verify(digest_algorithm, &signed_attributes_set, signature)? {
        return Err(SecurityError::SignatureInvalid);
    }

    // `Valid`/`WholeDocument` here certify only the checks above: the content
    // digest matches `messageDigest` and the provider accepted the signature
    // over the signed attributes. Trust anchoring, chain/revocation validation,
    // and trusted timestamps are deliberately not established by this crate.
    Ok(SignatureInfo::new(
        provider.signer_name(),
        signing_time,
        digest_algorithm,
        SignatureCoverage::WholeDocument,
        SignatureStatus::Valid,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::encode_octet_string as der_octets;
    use crate::der::encode_sequence as der_sequence;
    use crate::der::encode_tlv;

    fn certificate() -> Vec<u8> {
        der_sequence(&encode_oid(&[1, 2, 3, 4]).expect("oid"))
    }

    fn issuer(name: &str) -> Vec<u8> {
        der_sequence(&der_octets(name.as_bytes()))
    }

    fn signer(serial: u64) -> SignerIdentifier {
        SignerIdentifier::new(issuer("CN=PDFXI Test"), serial).expect("signer")
    }

    fn provider(serial: u64, name: &str) -> InMemorySignatureProvider {
        InMemorySignatureProvider::new(signer(serial), certificate(), name).expect("provider")
    }

    fn params() -> SignedDataParams {
        SignedDataParams {
            content: b"PDFXI-CONTENT-1234".to_vec(),
            digest_algorithm: DigestAlgorithm::Sha256,
            signer: signer(7),
            certificate_der: certificate(),
            signing_time: 1_600_000_000,
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len())
            .find(|&start| &haystack[start..start + needle.len()] == needle)
    }

    struct RejectingProvider {
        inner: InMemorySignatureProvider,
    }

    impl SignatureProvider for RejectingProvider {
        fn sign(&self, algorithm: DigestAlgorithm, tbs: &[u8]) -> Result<Vec<u8>> {
            self.inner.sign(algorithm, tbs)
        }
        fn verify(
            &self,
            _algorithm: DigestAlgorithm,
            _tbs: &[u8],
            _signature: &[u8],
        ) -> Result<bool> {
            Ok(false)
        }
        fn signer_name(&self) -> &str {
            self.inner.signer_name()
        }
        fn certificate_der(&self) -> &[u8] {
            self.inner.certificate_der()
        }
        fn signer_identifier(&self) -> &SignerIdentifier {
            self.inner.signer_identifier()
        }
    }

    #[test]
    fn build_then_verify_returns_valid_metadata() {
        let provider = provider(7, "PDFXI Test Signer");
        let der = build_signed_data(&params(), &provider).expect("build");
        let info = verify_signed_data(&der, &provider).expect("verify");
        assert_eq!(info.validity(), SignatureStatus::Valid);
        assert!(info.is_valid());
        assert_eq!(info.signer(), "PDFXI Test Signer");
        assert_eq!(info.signed_at(), 1_600_000_000);
        assert_eq!(info.digest_algorithm(), DigestAlgorithm::Sha256);
        assert_eq!(info.coverage(), SignatureCoverage::WholeDocument);
    }

    #[test]
    fn output_is_deterministic_for_fixed_inputs() {
        let provider = provider(7, "PDFXI Test Signer");
        let first = build_signed_data(&params(), &provider).expect("build");
        let second = build_signed_data(&params(), &provider).expect("build");
        assert_eq!(first, second);

        let mut changed = params();
        changed.signing_time += 1;
        assert_ne!(
            first,
            build_signed_data(&changed, &provider).expect("build")
        );
    }

    #[test]
    fn tampered_content_is_rejected() {
        let provider = provider(7, "PDFXI Test Signer");
        let mut der = build_signed_data(&params(), &provider).expect("build");
        let needle = b"PDFXI-CONTENT-1234";
        let offset = find_subsequence(&der, needle).expect("content present");
        der[offset] ^= 0x01;
        assert_eq!(
            verify_signed_data(&der, &provider),
            Err(SecurityError::TamperedContent)
        );
    }

    #[test]
    fn tampered_signed_attribute_is_rejected() {
        let provider = provider(7, "PDFXI Test Signer");
        let mut der = build_signed_data(&params(), &provider).expect("build");
        let time_tlv = crate::der::encode_time(1_600_000_000).expect("time");
        let offset = find_subsequence(&der, &time_tlv).expect("time attribute present");
        let digit_index = offset + 2;
        der[digit_index] = if der[digit_index] == b'2' { b'3' } else { b'2' };
        assert_eq!(
            verify_signed_data(&der, &provider),
            Err(SecurityError::SignatureInvalid)
        );
    }

    #[test]
    fn truncated_and_trailing_der_are_rejected_distinctly() {
        let provider = provider(7, "PDFXI Test Signer");
        let der = build_signed_data(&params(), &provider).expect("build");
        let truncated = &der[..der.len() - 4];
        assert_eq!(
            verify_signed_data(truncated, &provider),
            Err(SecurityError::DerTruncated)
        );
        let mut trailing = der;
        trailing.push(0);
        assert_eq!(
            verify_signed_data(&trailing, &provider),
            Err(SecurityError::DerTrailingBytes)
        );
    }

    #[test]
    fn unsupported_algorithms_are_rejected_distinctly() {
        let provider = provider(7, "PDFXI Test Signer");
        let mut unsupported_digest = params();
        unsupported_digest.digest_algorithm = DigestAlgorithm::Sha1;
        assert_eq!(
            build_signed_data(&unsupported_digest, &provider),
            Err(SecurityError::UnsupportedDigestAlgorithm(
                "1.3.14.3.2.26".to_string()
            ))
        );

        let mut der = build_signed_data(&params(), &provider).expect("build");
        let signature_oid = encode_tlv(
            Tag::OBJECT_IDENTIFIER,
            &[0x2a, 0x86, 0x48, 0x86, 0xf7, 0x0d, 0x01, 0x01, 0x0b],
        );
        let offset = find_subsequence(&der, &signature_oid).expect("signature oid present");
        der[offset + signature_oid.len() - 1] = 0x05;
        assert_eq!(
            verify_signed_data(&der, &provider),
            Err(SecurityError::UnsupportedSignatureAlgorithm(
                "1.2.840.113549.1.1.5".to_string()
            ))
        );
    }

    #[test]
    fn wrong_provider_credentials_are_rejected() {
        let signer_provider = provider(7, "PDFXI Test Signer");
        let der = build_signed_data(&params(), &signer_provider).expect("build");

        let different_signer = provider(8, "PDFXI Test Signer");
        assert_eq!(
            verify_signed_data(&der, &different_signer),
            Err(SecurityError::SignerMismatch)
        );

        let different_certificate =
            InMemorySignatureProvider::new(signer(7), der_sequence(&[]), "Other")
                .expect("provider");
        assert_eq!(
            verify_signed_data(&der, &different_certificate),
            Err(SecurityError::CertificateMismatch)
        );
    }

    #[test]
    fn bad_signature_is_rejected() {
        let provider = provider(7, "PDFXI Test Signer");
        let der = build_signed_data(&params(), &provider).expect("build");
        let rejecting = RejectingProvider { inner: provider };
        assert_eq!(
            verify_signed_data(&der, &rejecting),
            Err(SecurityError::SignatureInvalid)
        );
    }

    #[test]
    fn detached_verification_uses_supplied_content() {
        let provider = provider(7, "PDFXI Test Signer");
        let der = build_signed_data(&params(), &provider).expect("build");
        assert!(verify_signed_data_detached(&der, b"PDFXI-CONTENT-1234", &provider).is_ok());
        assert_eq!(
            verify_signed_data_detached(&der, b"PDFXI-CONTENT-1235", &provider),
            Err(SecurityError::TamperedContent)
        );
    }

    #[test]
    fn metadata_maps_all_digest_algorithms() {
        let provider = provider(7, "Metadata Signer");
        for (algorithm, expected_len) in [
            (DigestAlgorithm::Sha256, 32usize),
            (DigestAlgorithm::Sha384, 48),
            (DigestAlgorithm::Sha512, 64),
        ] {
            let mut input = params();
            input.digest_algorithm = algorithm;
            let der = build_signed_data(&input, &provider).expect("build");
            let info = verify_signed_data(&der, &provider).expect("verify");
            assert_eq!(info.digest_algorithm(), algorithm);
            assert_eq!(
                digest(algorithm, b"abc").expect("digest").len(),
                expected_len
            );
        }
        assert_eq!(
            digest(DigestAlgorithm::Sha1, b"abc"),
            Err(SecurityError::UnsupportedDigestAlgorithm(
                "1.3.14.3.2.26".to_string()
            ))
        );
    }

    #[test]
    fn modpow_matches_published_small_key_vectors() {
        assert_eq!(modpow_u128(3, 7, 13), Some(3));
        assert_eq!(modpow_u128(2, 10, 17), Some(4));
        assert_eq!(modpow_u128(7, 0, 11), Some(1));
        assert_eq!(modpow_u128(2, 3, 1), None);

        let provider = provider(7, "PDFXI Test Signer");
        let representative = 123_456u128;
        let signature = modpow_u128(representative, provider.private_exponent, provider.modulus)
            .expect("toy exponentiation");
        assert_eq!(
            modpow_u128(signature, provider.public_exponent, provider.modulus),
            Some(representative)
        );
    }

    #[test]
    fn provider_signatures_are_deterministic_and_content_sensitive() {
        let provider = provider(7, "PDFXI Test Signer");
        let first = provider
            .sign(DigestAlgorithm::Sha256, b"tbs")
            .expect("sign");
        let second = provider
            .sign(DigestAlgorithm::Sha256, b"tbs")
            .expect("sign");
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        let changed = provider
            .sign(DigestAlgorithm::Sha256, b"tbt")
            .expect("sign");
        assert_ne!(first, changed);
        assert!(
            provider
                .verify(DigestAlgorithm::Sha256, b"tbs", &first)
                .expect("verify")
        );
        assert!(
            !provider
                .verify(DigestAlgorithm::Sha256, b"tbt", &first)
                .expect("verify")
        );
    }

    #[test]
    fn malformed_inputs_produce_specific_errors() {
        assert_eq!(
            SignerIdentifier::new(vec![0x04, 0x01, 0x00], 1),
            Err(SecurityError::DerUnexpectedTag {
                expected: Tag::SEQUENCE.byte(),
                found: Tag::OCTET_STRING.byte(),
            })
        );
        assert_eq!(
            InMemorySignatureProvider::new(signer(1), vec![0x30], "Bad").map(|_| ()),
            Err(SecurityError::DerTruncated)
        );
    }
}
