//! RFC 3161 timestamp token parsing and validation.
//!
//! This module models an RFC 3161 `TimeStampToken`: a CMS `SignedData` whose
//! encapsulated content type is `id-ct-TSTInfo` and whose `eContent` is the DER
//! encoding of a `TSTInfo`. It parses every `TSTInfo` field defined by RFC 3161
//! (version, policy, message imprint, serial, genTime, accuracy, ordering,
//! nonce, and the optional TSA name), checks the token signature and the
//! `TSTInfo` digest through the existing [`SignatureProvider`] CMS boundary,
//! and delegates all trust decisions to a caller-supplied store, verifier and
//! clock via [`verify_chain`].
//!
//! # Trust boundary
//!
//! No key material, root store, or wall clock lives here. The token signature
//! is accepted only when the caller's [`SignatureProvider`] says so; the TSA
//! certificate chain is trusted only when it terminates at a caller-supplied
//! anchor. A [`TimestampStatus::Valid`] result — and the corresponding
//! [`SignatureStatus::Valid`] — means precisely that every check this crate can
//! perform succeeded: cryptographic integrity of the CMS token, a matching
//! message imprint, a genTime that is not implausibly in the future, a TSA
//! chain valid at `genTime`, a TSA certificate bearing the RFC 3161
//! `id-kp-timeStamping` EKU, and a matching nonce when one was supplied. It does
//! **not** assert identity policy, revocation/CRL status, or any legal notion of
//! non-repudiation; those remain higher-layer responsibilities (see SEC-007).
//!
//! Because the token carries its own `genTime`, this module also enforces a
//! replay/clock policy: tokens dated beyond a caller-controlled skew from the
//! supplied "now" clock are rejected, and the TSA certificate is validated at
//! `genTime` rather than at "now" so an expired-today TSA can still validate a
//! historically issued token.
//!
//! # TSA extended key usage
//!
//! RFC 3161 section 2.3 requires a TSA certificate to carry the
//! `id-kp-timeStamping` extended key usage. [`TimestampPolicy::required_tsa_eku`]
//! enforces this by default (unlike a generic [`ValidationPolicy`], whose EKU is
//! unconstrained by default). A chain-trusted certificate that lacks the purpose
//! yields [`TimestampStatus::TimestampEkuMissing`], never `Valid`. Callers with
//! non-standard TSA certificates can set that field to another purpose or to
//! `None` to disable the check; doing so is an explicit trust decision.
//!
//! # GeneralizedTime limits
//!
//! `genTime` must be a DER `GeneralizedTime`. The shared DER time decoder accepts
//! canonical `YYYYMMDDHHMMSSZ` and an optional fractional-seconds suffix
//! (`.sss...Z`, one to nine digits, as used by RFC 3161). The fraction is
//! validated but discarded because the public time type is whole Unix seconds;
//! sub-second precision is therefore not represented in [`TstInfo::gen_time`]
//! and is not used by the skew or nonce checks. `UTCTime` and malformed
//! fractional forms are rejected.
//!
//! [`verify_chain`]: crate::x509::verify_chain

use crate::cms::{SignatureProvider, digest};
use crate::crypto::constant_time_eq;
use crate::der::{
    DerReader, Tag, Tlv, encode_context, encode_integer, encode_null, encode_octet_string,
    encode_oid, encode_sequence, encode_set, encode_tlv,
};
use crate::error::{Result, SecurityError};
use crate::signature::{DigestAlgorithm, SignatureCoverage, SignatureInfo, SignatureStatus};
use crate::x509::certificate::Certificate;
use crate::x509::extension::EkuPurpose;
use crate::x509::verify::{TrustStore, ValidationPolicy, verify_chain};
use crate::x509::{CertificateVerifier, Clock, FixedClock, parse_algorithm_identifier};

/// OID `1.2.840.113549.1.7.2` — `signedData` (CMS content type).
const OID_SIGNED_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 2];
/// OID `1.2.840.113549.1.9.16.1.4` — `id-ct-TSTInfo`.
const OID_ID_CT_TST_INFO: [u32; 9] = [1, 2, 840, 113_549, 1, 9, 16, 1, 4];
/// OID `1.2.840.113549.1.9.3` — `contentType` signed attribute.
const OID_CONTENT_TYPE: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 3];
/// OID `1.2.840.113549.1.9.4` — `messageDigest` signed attribute.
const OID_MESSAGE_DIGEST: [u32; 7] = [1, 2, 840, 113_549, 1, 9, 4];
/// OID `2.16.840.1.101.3.4.2.1` — SHA-256.
const OID_SHA256: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 1];
/// OID `2.16.840.1.101.3.4.2.2` — SHA-384.
const OID_SHA384: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 2];
/// OID `2.16.840.1.101.3.4.2.3` — SHA-512.
const OID_SHA512: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 2, 3];
/// OID `1.2.840.113549.1.1.11` — `sha256WithRSAEncryption`.
const OID_SHA256_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 11];
/// OID `1.2.840.113549.1.1.12` — `sha384WithRSAEncryption`.
const OID_SHA384_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 12];
/// OID `1.2.840.113549.1.1.13` — `sha512WithRSAEncryption`.
const OID_SHA512_RSA: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 13];

fn oid_string(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

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

fn context0() -> Result<Tag> {
    Tag::context(0, true).ok_or(SecurityError::DerInvalid)
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

fn encode_attribute(oid: &[u32], value_der: &[u8]) -> Result<Vec<u8>> {
    let mut children = encode_oid(oid)?;
    children.push(Tag::SET.byte());
    children.extend_from_slice(&crate::der::encode_length(value_der.len()));
    children.extend_from_slice(value_der);
    Ok(encode_sequence(&children))
}

fn set_to_implicit(set_der: &[u8]) -> Result<Vec<u8>> {
    if set_der.first() != Some(&Tag::SET.byte()) {
        return Err(SecurityError::DerInvalid);
    }
    let mut out = vec![context0()?.byte()];
    out.extend_from_slice(&set_der[1..]);
    Ok(out)
}

fn implicit_to_set(tlv: &Tlv<'_>) -> Result<Vec<u8>> {
    if tlv.tag() != context0()? {
        return Err(SecurityError::DerUnexpectedTag {
            expected: context0()?.byte(),
            found: tlv.tag().byte(),
        });
    }
    let mut out = vec![Tag::SET.byte()];
    out.extend_from_slice(&tlv.encoded()[1..]);
    Ok(out)
}

fn next_required<'a>(reader: &mut DerReader<'a>, field: &'static str) -> Result<Tlv<'a>> {
    reader.read_tlv().map_err(|error| match error {
        SecurityError::DerTruncated => SecurityError::TimestampFieldMissing(field),
        other => other,
    })
}

/// Minimal content octets of a non-negative DER `INTEGER` (tag/length omitted).
fn integer_content(value: u64) -> Vec<u8> {
    let bytes = value.to_be_bytes();
    let mut first = 0usize;
    while first + 1 < bytes.len() && bytes[first] == 0 {
        first += 1;
    }
    let mut content = Vec::with_capacity(9);
    if bytes[first] & 0x80 != 0 {
        content.push(0u8);
    }
    content.extend_from_slice(&bytes[first..]);
    content
}

/// Decode the content octets of a context-tagged IMPLICIT `INTEGER`.
fn implicit_integer_content(content: &[u8]) -> Result<u64> {
    if content.is_empty() || content[0] & 0x80 != 0 {
        return Err(SecurityError::DerInvalid);
    }
    if content.len() > 1 && content[0] == 0 && content[1] & 0x80 == 0 {
        return Err(SecurityError::DerInvalid);
    }
    if content.len() > 9 {
        return Err(SecurityError::DerInvalid);
    }
    let significant = if content.len() == 9 {
        if content[0] != 0 {
            return Err(SecurityError::DerInvalid);
        }
        &content[1..]
    } else {
        content
    };
    let mut value = 0u64;
    for byte in significant {
        value = value
            .checked_mul(256)
            .and_then(|number| number.checked_add(u64::from(*byte)))
            .ok_or(SecurityError::DerInvalid)?;
    }
    Ok(value)
}

/// Encode a time as a DER `GeneralizedTime` (`YYYYMMDDHHMMSSZ`).
///
/// RFC 3161 requires `genTime` to be a `GeneralizedTime`, whereas the general
/// DER helper emits a `UTCTime` for years 1950–2049. This converts that form to
/// the canonical four-digit-year representation with the correct century.
fn encode_generalized_time(unix_seconds: u64) -> Result<Vec<u8>> {
    let encoded = crate::der::encode_time(unix_seconds)?;
    if encoded.first() == Some(&Tag::UTC_TIME.byte()) {
        let content = encoded.get(2..).ok_or(SecurityError::DerInvalid)?;
        if content.len() != 13 || content.last() != Some(&b'Z') {
            return Err(SecurityError::DerInvalid);
        }
        let century: &[u8; 2] = if content[0] >= b'5' { b"19" } else { b"20" };
        let mut generalized = Vec::with_capacity(15);
        generalized.extend_from_slice(century);
        generalized.extend_from_slice(content);
        Ok(encode_tlv(Tag::GENERALIZED_TIME, &generalized))
    } else {
        Ok(encoded)
    }
}

fn is_context(byte: u8, number: u8) -> bool {
    byte & 0x1f == number && byte & 0xe0 == 0x80
}

/// The `Accuracy` structure of a `TSTInfo` (RFC 3161 section 2.4.2).
///
/// All values are non-negative integers; the RFC constrains `millis` and
/// `micros` to `1..=999`, which the parser enforces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Accuracy {
    /// Accuracy in whole seconds, when present.
    pub seconds: Option<u64>,
    /// Additional sub-second accuracy in milliseconds (`1..=999`).
    pub millis: Option<u64>,
    /// Additional sub-second accuracy in microseconds (`1..=999`).
    pub micros: Option<u64>,
}

/// A `MessageImprint`: the digest algorithm and the hash of the timestamped data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MessageImprint {
    /// Digest algorithm used to compute the imprint.
    pub digest_algorithm: DigestAlgorithm,
    /// The hashed message octets.
    pub hashed_message: Vec<u8>,
}

/// A parsed RFC 3161 `TSTInfo`.
///
/// The `tsa` field is stored as the opaque DER of the `[0] GeneralName` field;
/// this subset does not interpret GeneralName alternatives. The `extensions`
/// field (if present) is parsed as opaque DER and not retained.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TstInfo {
    /// `version`; this subset accepts only v1.
    pub version: u64,
    /// The TSA policy OID arcs.
    pub policy: Vec<u32>,
    /// The imprint of the data being timestamped.
    pub message_imprint: MessageImprint,
    /// Serial number assigned by the TSA.
    pub serial_number: u64,
    /// Time at which the stamp was produced, as Unix seconds (`genTime`).
    pub gen_time: u64,
    /// Optional accuracy of `genTime`.
    pub accuracy: Option<Accuracy>,
    /// Ordering flag (DEFAULT FALSE).
    pub ordering: bool,
    /// Optional nonce echoing a request value.
    pub nonce: Option<u64>,
    /// Optional TSA name as the raw DER of the `[0] GeneralName` field.
    pub tsa: Option<Vec<u8>>,
}

fn parse_accuracy(tlv: &Tlv<'_>) -> Result<Accuracy> {
    let mut reader = tlv.as_sequence()?;
    let mut accuracy = Accuracy {
        seconds: None,
        millis: None,
        micros: None,
    };
    if reader.peek_tag() == Some(Tag::INTEGER) {
        accuracy.seconds = Some(reader.read_tlv()?.as_integer_u64()?);
    }
    if let Some(tag) = reader.peek_tag()
        && is_context(tag.byte(), 0)
    {
        let value = reader.read_tlv()?;
        let millis = implicit_integer_content(value.content())?;
        if millis == 0 || millis > 999 {
            return Err(SecurityError::DerInvalid);
        }
        accuracy.millis = Some(millis);
    }
    if let Some(tag) = reader.peek_tag()
        && is_context(tag.byte(), 1)
    {
        let value = reader.read_tlv()?;
        let micros = implicit_integer_content(value.content())?;
        if micros == 0 || micros > 999 {
            return Err(SecurityError::DerInvalid);
        }
        accuracy.micros = Some(micros);
    }
    reader.expect_end()?;
    Ok(accuracy)
}

/// Parse a DER-encoded RFC 3161 `TSTInfo`.
///
/// Every read is bounds-checked; malformed or missing fields produce typed
/// errors rather than panics. A `genTime` that is not a `GeneralizedTime`, or a
/// version other than v1, is rejected.
pub fn parse_tst_info(der: &[u8]) -> Result<TstInfo> {
    let mut top = DerReader::new(der);
    let tlv = top.read_tlv()?;
    top.expect_end()?;
    let mut sequence = tlv.as_sequence()?;

    let version = next_required(&mut sequence, "version")?.as_integer_u64()?;
    if version != 1 {
        return Err(SecurityError::TimestampVersionUnsupported(version));
    }
    let policy = next_required(&mut sequence, "policy")?.as_oid()?;

    let mut imprint = next_required(&mut sequence, "messageImprint")?.as_sequence()?;
    let (digest_alg_oid, _) = parse_algorithm_identifier(&next_required(
        &mut imprint,
        "messageImprint.hashAlgorithm",
    )?)?;
    let digest_algorithm = digest_from_oid(&digest_alg_oid)
        .ok_or_else(|| SecurityError::UnsupportedDigestAlgorithm(oid_string(&digest_alg_oid)))?;
    let hashed_message = next_required(&mut imprint, "messageImprint.hashedMessage")?
        .as_octets()?
        .to_vec();
    imprint.expect_end()?;

    let serial_number = next_required(&mut sequence, "serialNumber")?.as_integer_u64()?;
    let gen_time_tlv = next_required(&mut sequence, "genTime")?;
    if gen_time_tlv.tag() != Tag::GENERALIZED_TIME {
        return Err(SecurityError::DerUnexpectedTag {
            expected: Tag::GENERALIZED_TIME.byte(),
            found: gen_time_tlv.tag().byte(),
        });
    }
    let gen_time = gen_time_tlv.as_time()?;

    let mut accuracy = None;
    let mut ordering = false;
    let mut ordering_seen = false;
    let mut nonce = None;
    let mut tsa = None;
    while let Some(tag) = sequence.peek_tag() {
        let byte = tag.byte();
        if byte == Tag::SEQUENCE.byte() {
            if accuracy.is_some() {
                return Err(SecurityError::DerInvalid);
            }
            accuracy = Some(parse_accuracy(&sequence.read_tlv()?)?);
        } else if byte == Tag::BOOLEAN.byte() {
            if ordering_seen {
                return Err(SecurityError::DerInvalid);
            }
            ordering_seen = true;
            ordering = sequence.read_tlv()?.as_bool()?;
        } else if byte == Tag::INTEGER.byte() {
            if nonce.is_some() {
                return Err(SecurityError::DerInvalid);
            }
            nonce = Some(sequence.read_tlv()?.as_integer_u64()?);
        } else if is_context(byte, 0) {
            if tsa.is_some() {
                return Err(SecurityError::DerInvalid);
            }
            tsa = Some(sequence.read_tlv()?.encoded().to_vec());
        } else if is_context(byte, 1) {
            // `extensions [1] IMPLICIT` is opaque to this subset.
            sequence.read_tlv()?;
        } else {
            return Err(SecurityError::DerInvalid);
        }
    }

    Ok(TstInfo {
        version,
        policy,
        message_imprint: MessageImprint {
            digest_algorithm,
            hashed_message,
        },
        serial_number,
        gen_time,
        accuracy,
        ordering,
        nonce,
        tsa,
    })
}

/// Encode a [`TstInfo`] as DER.
///
/// This is the inverse of [`parse_tst_info`] and is used to build test vectors
/// and by [`build_timestamp_token`]. Optional fields are emitted in RFC 3161
/// order; `ordering` is omitted when false because it defaults to false.
pub fn encode_tst_info(info: &TstInfo) -> Result<Vec<u8>> {
    let mut children = Vec::new();
    children.extend_from_slice(&encode_integer(info.version));
    children.extend_from_slice(&encode_oid(&info.policy)?);

    let mut imprint = Vec::new();
    imprint.extend_from_slice(&encode_algorithm_identifier(digest_oid(
        info.message_imprint.digest_algorithm,
    )?)?);
    imprint.extend_from_slice(&encode_octet_string(&info.message_imprint.hashed_message));
    children.extend_from_slice(&encode_sequence(&imprint));

    children.extend_from_slice(&encode_integer(info.serial_number));
    children.extend_from_slice(&encode_generalized_time(info.gen_time)?);

    if let Some(accuracy) = &info.accuracy {
        let mut fields = Vec::new();
        if let Some(seconds) = accuracy.seconds {
            fields.extend_from_slice(&encode_integer(seconds));
        }
        if let Some(millis) = accuracy.millis {
            let tag = Tag::context(0, false).ok_or(SecurityError::DerInvalid)?;
            fields.extend_from_slice(&encode_tlv(tag, &integer_content(millis)));
        }
        if let Some(micros) = accuracy.micros {
            let tag = Tag::context(1, false).ok_or(SecurityError::DerInvalid)?;
            fields.extend_from_slice(&encode_tlv(tag, &integer_content(micros)));
        }
        children.extend_from_slice(&encode_sequence(&fields));
    }
    if info.ordering {
        children.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
    }
    if let Some(nonce) = info.nonce {
        children.extend_from_slice(&encode_integer(nonce));
    }
    if let Some(tsa) = &info.tsa {
        children.extend_from_slice(tsa);
    }
    Ok(encode_sequence(&children))
}

/// Inputs to [`build_timestamp_token`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampTokenParams<'a> {
    /// The data whose digest is placed in the `messageImprint`.
    pub data: &'a [u8],
    /// Digest algorithm for the imprint and for the CMS signature.
    pub digest_algorithm: DigestAlgorithm,
    /// TSA policy OID arcs.
    pub policy: Vec<u32>,
    /// TSA-assigned serial number.
    pub serial_number: u64,
    /// `genTime` as whole Unix seconds.
    ///
    /// A DER `GeneralizedTime` fractional-seconds suffix is accepted and
    /// validated by the parser but discarded here, since this representation
    /// carries no sub-second precision.
    pub gen_time: u64,
    /// Optional accuracy of `genTime`.
    pub accuracy: Option<Accuracy>,
    /// Ordering flag.
    pub ordering: bool,
    /// Optional nonce echoing a request value.
    pub nonce: Option<u64>,
    /// Optional TSA name as the raw DER of a `[0] GeneralName` field.
    pub tsa: Option<Vec<u8>>,
}

/// Build a deterministic RFC 3161 `TimeStampToken` (CMS `SignedData`) for tests
/// and for callers that need to mint tokens with a provider they control.
///
/// The token wraps a freshly built `TSTInfo` whose imprint is the digest of
/// `params.data`. Signing is delegated to `provider`, so no private key is held
/// by this crate. Identical inputs with the same provider yield identical bytes.
pub fn build_timestamp_token(
    params: &TimestampTokenParams<'_>,
    provider: &dyn SignatureProvider,
) -> Result<Vec<u8>> {
    let imprint = digest(params.digest_algorithm, params.data)?;
    let info = TstInfo {
        version: 1,
        policy: params.policy.clone(),
        message_imprint: MessageImprint {
            digest_algorithm: params.digest_algorithm,
            hashed_message: imprint,
        },
        serial_number: params.serial_number,
        gen_time: params.gen_time,
        accuracy: params.accuracy.clone(),
        ordering: params.ordering,
        nonce: params.nonce,
        tsa: params.tsa.clone(),
    };
    let tst_info_der = encode_tst_info(&info)?;
    build_signed_data_around(&tst_info_der, params.digest_algorithm, provider)
}

fn build_signed_data_around(
    e_content: &[u8],
    algorithm: DigestAlgorithm,
    provider: &dyn SignatureProvider,
) -> Result<Vec<u8>> {
    let digest_alg_oid = digest_oid(algorithm)?;
    let signature_alg_oid = signature_oid(algorithm)?;
    validate_single_tlv(provider.certificate_der(), Tag::SEQUENCE)?;

    let content_digest = digest(algorithm, e_content)?;
    let content_type_attribute =
        encode_attribute(&OID_CONTENT_TYPE, &encode_oid(&OID_ID_CT_TST_INFO)?)?;
    let message_digest_attribute =
        encode_attribute(&OID_MESSAGE_DIGEST, &encode_octet_string(&content_digest))?;
    let mut attributes = vec![content_type_attribute, message_digest_attribute];
    attributes.sort();
    let mut attributes_content = Vec::new();
    for attribute in &attributes {
        attributes_content.extend_from_slice(attribute);
    }
    let signed_attributes_set = encode_set(&attributes_content);
    let signature = provider.sign(algorithm, &signed_attributes_set)?;

    let mut signer_children = Vec::new();
    signer_children.extend_from_slice(&encode_integer(1));
    let mut signer_id_children = Vec::new();
    signer_id_children.extend_from_slice(provider.signer_identifier().issuer_der());
    signer_id_children.extend_from_slice(&encode_integer(
        provider.signer_identifier().serial_number(),
    ));
    signer_children.extend_from_slice(&encode_sequence(&signer_id_children));
    signer_children.extend_from_slice(&encode_algorithm_identifier(digest_alg_oid)?);
    signer_children.extend_from_slice(&set_to_implicit(&signed_attributes_set)?);
    signer_children.extend_from_slice(&encode_algorithm_identifier(signature_alg_oid)?);
    signer_children.extend_from_slice(&encode_octet_string(&signature));
    let signer_info = encode_sequence(&signer_children);

    let digest_algorithms = encode_set(&encode_algorithm_identifier(digest_alg_oid)?);
    let explicit_content = encode_context(0, true, &encode_octet_string(e_content))
        .ok_or(SecurityError::DerInvalid)?;
    let mut encap_children = encode_oid(&OID_ID_CT_TST_INFO)?;
    encap_children.extend_from_slice(&explicit_content);
    let encap_content_info = encode_sequence(&encap_children);

    let mut certificate_field = vec![context0()?.byte()];
    certificate_field.extend_from_slice(&provider.certificate_der()[1..]);

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

/// The outcome of the cryptographic checks on a timestamp token, before trust
/// and policy decisions are applied.
struct ParsedToken {
    info: TstInfo,
    tsa_certificate_der: Vec<u8>,
    cryptographic_valid: bool,
    signer_display: String,
}

/// Parse a token and verify its CMS signature and `TSTInfo` digest.
///
/// `provider` is the existing CMS boundary: it supplies the embedded
/// certificate and signer, and performs the signature verification. This
/// function never builds a chain or reads a clock; it only establishes whether
/// the token is cryptographically intact.
fn verify_timestamp_token(
    token_der: &[u8],
    provider: &dyn SignatureProvider,
) -> Result<ParsedToken> {
    let mut top = DerReader::new(token_der);
    let mut content_info = top.read_sequence()?;
    top.expect_end()?;

    let content_type = content_info.read_tlv()?.as_oid()?;
    if content_type != OID_SIGNED_DATA {
        return Err(SecurityError::TimestampTokenTypeInvalid);
    }
    let mut signed_data_wrapper = content_info.read_tagged(context0()?)?;
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
    let (digest_alg_oid, _) = parse_algorithm_identifier(&digest_algorithm_tlv)?;
    let digest_algorithm = digest_from_oid(&digest_alg_oid)
        .ok_or_else(|| SecurityError::UnsupportedDigestAlgorithm(oid_string(&digest_alg_oid)))?;

    let mut encap_content_info = signed_data.read_sequence()?;
    let encap_content_type = encap_content_info.read_tlv()?.as_oid()?;
    if encap_content_type != OID_ID_CT_TST_INFO {
        return Err(SecurityError::TimestampTokenTypeInvalid);
    }
    let mut econtent_wrapper = encap_content_info.read_tagged(context0()?)?;
    let tst_info_der = econtent_wrapper.read_tlv()?.as_octets()?.to_vec();
    econtent_wrapper.expect_end()?;
    encap_content_info.expect_end()?;

    let certificate_field = signed_data.read_tlv()?;
    if certificate_field.tag() != context0()? {
        return Err(SecurityError::CertificateMissing);
    }
    let mut tsa_certificate_der = vec![Tag::SEQUENCE.byte()];
    tsa_certificate_der.extend_from_slice(&certificate_field.encoded()[1..]);
    validate_single_tlv(&tsa_certificate_der, Tag::SEQUENCE)?;
    if tsa_certificate_der.as_slice() != provider.certificate_der() {
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
    let _issuer = signer_id.read_tlv()?;
    let _serial = signer_id.read_tlv()?.as_integer_u64()?;
    signer_id.expect_end()?;
    let expected_signer = provider.signer_identifier();
    if _issuer.encoded() != expected_signer.issuer_der()
        || _serial != expected_signer.serial_number()
    {
        return Err(SecurityError::SignerMismatch);
    }

    let signer_digest_oid = parse_algorithm_identifier(&signer_info.read_tlv()?)?.0;
    if signer_digest_oid != digest_alg_oid {
        return Err(SecurityError::DerInvalid);
    }

    let signed_attributes_tlv = signer_info.read_tlv()?;
    let signed_attributes_set = implicit_to_set(&signed_attributes_tlv)?;
    let mut set_wrapper = DerReader::new(&signed_attributes_set);
    let mut attributes = set_wrapper.read_set()?;
    set_wrapper.expect_end()?;
    let mut content_type_attribute: Option<Vec<u32>> = None;
    let mut message_digest_attribute: Option<Vec<u8>> = None;
    while !attributes.is_empty() {
        let attribute_tlv = attributes.read_tlv()?;
        let mut attribute = attribute_tlv.as_sequence()?;
        let attribute_oid = attribute.read_tlv()?.as_oid()?;
        let mut values = attribute.read_set()?;
        let value = values.read_tlv()?;
        values.expect_end()?;
        attribute.expect_end()?;
        if attribute_oid == OID_CONTENT_TYPE {
            content_type_attribute = Some(value.as_oid()?);
        } else if attribute_oid == OID_MESSAGE_DIGEST {
            message_digest_attribute = Some(value.as_octets()?.to_vec());
        }
    }
    let content_type_attribute =
        content_type_attribute.ok_or(SecurityError::MissingSignedAttribute("contentType"))?;
    if content_type_attribute != OID_ID_CT_TST_INFO {
        return Err(SecurityError::TimestampTokenTypeInvalid);
    }
    let message_digest_attribute =
        message_digest_attribute.ok_or(SecurityError::MissingSignedAttribute("messageDigest"))?;

    let signature_algorithm_oid = parse_algorithm_identifier(&signer_info.read_tlv()?)?.0;
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
        if !is_context(unsigned_attributes.tag().byte(), 1) {
            return Err(SecurityError::DerInvalid);
        }
    }
    signer_info.expect_end()?;
    signed_data.expect_end()?;

    let computed_digest = digest(digest_algorithm, &tst_info_der)?;
    let digest_ok = constant_time_eq(&computed_digest, &message_digest_attribute);
    let signature_ok = provider.verify(digest_algorithm, &signed_attributes_set, signature)?;
    let info = parse_tst_info(&tst_info_der)?;

    Ok(ParsedToken {
        info,
        tsa_certificate_der,
        cryptographic_valid: digest_ok && signature_ok,
        signer_display: provider.signer_name().to_owned(),
    })
}

/// Distinct, typed outcome of timestamp validation.
///
/// Each non-`Valid` variant pins down exactly which dimension failed, so callers
/// can report cryptographic integrity, chain trust, time validity, certificate
/// validity at `genTime`, and nonce agreement independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TimestampStatus {
    /// Every check succeeded; see the module-level trust boundary for scope.
    Valid,
    /// The CMS signature was rejected or the `TSTInfo` digest did not match.
    CryptographicFailure,
    /// The `messageImprint` did not match the supplied data.
    ImprintMismatch,
    /// The TSA certificate chain did not reach a trusted anchor.
    UntrustedTsa,
    /// The TSA chain does not permit the RFC 3161 timeStamping EKU.
    TimestampEkuMissing,
    /// The TSA certificate was not valid at `genTime`.
    CertificateNotValidAtGenTime,
    /// `genTime` lay beyond the allowed future skew from the supplied clock.
    FutureTime,
    /// The token nonce did not match the caller-supplied expected nonce.
    NonceMismatch,
}

impl TimestampStatus {
    /// Short human-readable label for this status.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::CryptographicFailure => "cryptographic-failure",
            Self::ImprintMismatch => "imprint-mismatch",
            Self::UntrustedTsa => "untrusted-tsa",
            Self::TimestampEkuMissing => "timestamp-eku-missing",
            Self::CertificateNotValidAtGenTime => "certificate-not-valid-at-gen-time",
            Self::FutureTime => "future-time",
            Self::NonceMismatch => "nonce-mismatch",
        }
    }

    /// Return `true` only for [`Self::Valid`].
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl std::fmt::Display for TimestampStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// Consensus policy parameters for [`validate_timestamp_token`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampPolicy {
    /// Maximum number of seconds `genTime` may lie in the future of the clock.
    ///
    /// A documented default of 300 seconds absorbs ordinary clock skew while
    /// limiting replay of implausibly future-dated tokens.
    pub max_future_skew_seconds: u64,
    /// When `Some`, the token's nonce must equal this value.
    pub expected_nonce: Option<u64>,
    /// Extended key usage the TSA certificate must carry.
    ///
    /// Defaults to [`EkuPurpose::TimeStamping`], as required by RFC 3161
    /// section 2.3: a chain-trusted certificate that lacks this purpose is not a
    /// valid TSA. Setting this to `None` disables the requirement, and setting it
    /// to another purpose replaces it; both are explicit, documented overrides
    /// for callers with non-standard TSA certificates. This value takes
    /// precedence over [`ValidationPolicy::required_eku`] for timestamp
    /// validation.
    pub required_tsa_eku: Option<EkuPurpose>,
}

impl Default for TimestampPolicy {
    fn default() -> Self {
        Self {
            max_future_skew_seconds: 300,
            expected_nonce: None,
            required_tsa_eku: Some(EkuPurpose::TimeStamping),
        }
    }
}

/// Caller-supplied trust material and clocks for timestamp validation.
///
/// Every trust decision is delegated: the store supplies anchors, `verifier`
/// checks certificate signatures, `chain_policy` carries path constraints, and
/// `now_clock` supplies the current time for the replay/skew check. The crate
/// holds no roots and never reads the host clock.
pub struct TimestampValidationContext<'a> {
    /// The data whose imprint the token must match.
    pub data: &'a [u8],
    /// CMS boundary used to verify the token signature and supply its certificate.
    pub provider: &'a dyn SignatureProvider,
    /// Additional untrusted certificates available for path building.
    pub intermediates: &'a [Certificate],
    /// Caller-supplied trust anchors and pinned intermediates.
    pub trust_store: &'a TrustStore,
    /// Caller-supplied certificate signature verifier.
    pub chain_verifier: &'a dyn CertificateVerifier,
    /// Chain validation constraints.
    pub chain_policy: &'a ValidationPolicy,
    /// Caller-supplied "now" clock used for the future-skew check.
    pub now_clock: &'a dyn Clock,
    /// Timestamp replay and nonce policy.
    pub policy: &'a TimestampPolicy,
}

/// A full timestamp validation result.
///
/// Produced only for structurally valid tokens. The [`Self::status`] field
/// records the specific outcome, and [`Self::to_signature_status`] maps it
/// conservatively onto [`SignatureStatus`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TimestampValidation {
    info: TstInfo,
    status: TimestampStatus,
    signer_display: String,
}

impl TimestampValidation {
    /// Return the parsed `TSTInfo`.
    pub fn info(&self) -> &TstInfo {
        &self.info
    }

    /// Return the specific validation outcome.
    pub const fn status(&self) -> TimestampStatus {
        self.status
    }

    /// Return `true` only when every check passed.
    pub const fn is_valid(&self) -> bool {
        self.status.is_valid()
    }

    /// Map this result conservatively onto [`SignatureStatus`].
    ///
    /// [`SignatureStatus::Valid`] is returned only for [`TimestampStatus::Valid`]:
    /// no partially verified token is ever promoted to `Valid`. Specifically,
    /// integrity failures map to `Tampered`, an untrusted TSA or a missing
    /// timeStamping EKU to `Unknown`, a certificate invalid at `genTime` to
    /// `Expired`, and replay/nonce failures to `Invalid`.
    pub const fn to_signature_status(&self) -> SignatureStatus {
        match self.status {
            TimestampStatus::Valid => SignatureStatus::Valid,
            TimestampStatus::CryptographicFailure | TimestampStatus::ImprintMismatch => {
                SignatureStatus::Tampered
            }
            TimestampStatus::UntrustedTsa | TimestampStatus::TimestampEkuMissing => {
                SignatureStatus::Unknown
            }
            TimestampStatus::CertificateNotValidAtGenTime => SignatureStatus::Expired,
            TimestampStatus::FutureTime | TimestampStatus::NonceMismatch => {
                SignatureStatus::Invalid
            }
        }
    }

    /// Build a [`SignatureInfo`] report from this result.
    ///
    /// The `signed_at` value is the token's `genTime`, the digest algorithm is
    /// the imprint algorithm, and coverage is the whole document. The validity
    /// field is exactly [`Self::to_signature_status`], so `Valid` is never
    /// asserted unless every timestamp check passed.
    pub fn to_signature_info(&self) -> SignatureInfo {
        SignatureInfo::new(
            self.signer_display.clone(),
            self.info.gen_time,
            self.info.message_imprint.digest_algorithm,
            SignatureCoverage::WholeDocument,
            self.to_signature_status(),
        )
    }
}

fn report(parsed: ParsedToken, status: TimestampStatus) -> TimestampValidation {
    TimestampValidation {
        info: parsed.info,
        status,
        signer_display: parsed.signer_display,
    }
}

/// Validate a timestamp token against caller-supplied trust material, data and clocks.
///
/// Structural failures (malformed DER, missing required fields, an unsupported
/// version or content type, a certificate/signer mismatch) return a typed
/// [`SecurityError`]. Semantic failures are reported through
/// [`TimestampValidation::status`] so that each dimension can be distinguished.
///
/// Checks run in a fixed order: CMS integrity first, then the message imprint,
/// then the future-skew/replay guard, then the TSA chain at `genTime` (including
/// the RFC 3161 timeStamping EKU requirement), and finally the optional nonce.
/// The first failing dimension determines the status.
pub fn validate_timestamp_token(
    token_der: &[u8],
    context: &TimestampValidationContext<'_>,
) -> Result<TimestampValidation> {
    let parsed = verify_timestamp_token(token_der, context.provider)?;

    if !parsed.cryptographic_valid {
        return Ok(report(parsed, TimestampStatus::CryptographicFailure));
    }

    let computed_imprint = digest(parsed.info.message_imprint.digest_algorithm, context.data)?;
    if !constant_time_eq(
        &computed_imprint,
        &parsed.info.message_imprint.hashed_message,
    ) {
        return Ok(report(parsed, TimestampStatus::ImprintMismatch));
    }

    let now = context.now_clock.now_unix_seconds();
    let latest_allowed = now.saturating_add(context.policy.max_future_skew_seconds);
    if parsed.info.gen_time > latest_allowed {
        return Ok(report(parsed, TimestampStatus::FutureTime));
    }

    let tsa_certificate = Certificate::parse(&parsed.tsa_certificate_der)?;

    // RFC 3161 section 2.3 requires the TSA certificate to carry the
    // id-kp-timeStamping EKU. `verify_chain` deliberately treats an absent EKU
    // as unconstrained (RFC 5280), so check the TSA leaf explicitly; otherwise a
    // merely chain-trusted certificate without the purpose would pass.
    if let Some(required) = &context.policy.required_tsa_eku {
        let permits = tsa_certificate
            .extended_key_usage()
            .is_some_and(|eku| eku.allows(required));
        if !permits {
            return Ok(report(parsed, TimestampStatus::TimestampEkuMissing));
        }
    }

    // Apply the same purpose requirement to issuing CAs, without mutating the
    // caller's policy. The timestamp-specific EKU takes precedence over the
    // generic chain policy's EKU for timestamp validation.
    let mut chain_policy = context.chain_policy.clone();
    chain_policy.required_eku = context.policy.required_tsa_eku.clone();
    let gen_time_clock = FixedClock::new(parsed.info.gen_time);
    match verify_chain(
        &tsa_certificate,
        context.intermediates,
        context.trust_store,
        context.chain_verifier,
        &chain_policy,
        &gen_time_clock,
    ) {
        Ok(_) => {}
        Err(SecurityError::CertificateExpired | SecurityError::CertificateNotYetValid) => {
            return Ok(report(
                parsed,
                TimestampStatus::CertificateNotValidAtGenTime,
            ));
        }
        Err(SecurityError::CertificateEkuMissing) => {
            return Ok(report(parsed, TimestampStatus::TimestampEkuMissing));
        }
        Err(_) => return Ok(report(parsed, TimestampStatus::UntrustedTsa)),
    }

    if let Some(expected_nonce) = context.policy.expected_nonce
        && parsed.info.nonce != Some(expected_nonce)
    {
        return Ok(report(parsed, TimestampStatus::NonceMismatch));
    }

    Ok(report(parsed, TimestampStatus::Valid))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cms::SignerIdentifier;
    use crate::crypto::sha::sha256;
    use crate::der::{encode_time, write_tlv};
    use crate::error::SecurityError;
    use crate::x509::OID_SHA256_WITH_RSA;
    use crate::x509::extension::{KeyUsage, OID_BASIC_CONSTRAINTS};

    const OID_CN: [u32; 4] = [2, 5, 4, 3];
    const OID_TIME_STAMPING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 8];
    const OID_SERVER_AUTH: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 1];
    const NOW: u64 = 1_650_000_000;

    struct DigestVerifier;

    impl CertificateVerifier for DigestVerifier {
        fn verify_signature(
            &self,
            _algorithm: &crate::x509::SignatureAlgorithm,
            tbs_der: &[u8],
            signature: &[u8],
            _issuer_key: &crate::PublicKeyInfo,
        ) -> Result<bool> {
            Ok(signature == &sha256(tbs_der)[..16])
        }
    }

    struct RejectingSignatureProvider {
        inner: crate::InMemorySignatureProvider,
    }

    impl SignatureProvider for RejectingSignatureProvider {
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

    fn name(cn: &str) -> Vec<u8> {
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        encode_sequence(&encode_set(&encode_sequence(&ava)))
    }

    fn alg_id(oid: &[u32]) -> Vec<u8> {
        let mut children = encode_oid(oid).expect("oid");
        children.extend_from_slice(&encode_null());
        encode_sequence(&children)
    }

    fn bit_string(payload: &[u8]) -> Vec<u8> {
        let mut content = Vec::with_capacity(payload.len() + 1);
        content.push(0u8);
        content.extend_from_slice(payload);
        encode_tlv(Tag::BIT_STRING, &content)
    }

    fn validity(not_before: u64, not_after: u64) -> Vec<u8> {
        let mut children = encode_time(not_before).expect("notBefore");
        children.extend_from_slice(&encode_time(not_after).expect("notAfter"));
        encode_sequence(&children)
    }

    fn extension(oid: &[u32], critical: bool, payload: &[u8]) -> Vec<u8> {
        let mut children = encode_oid(oid).expect("oid");
        if critical {
            children.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
        }
        children.extend_from_slice(&encode_octet_string(payload));
        encode_sequence(&children)
    }

    fn basic_constraints(ca: bool) -> Vec<u8> {
        let payload = if ca {
            encode_sequence(&encode_tlv(Tag::BOOLEAN, &[0xff]))
        } else {
            encode_sequence(&[])
        };
        extension(&OID_BASIC_CONSTRAINTS, true, &payload)
    }

    fn key_usage(bits: u16) -> Vec<u8> {
        let mut payload = [0u8, 0];
        for index in 0..9 {
            if bits & (1u16 << index) != 0 {
                payload[index / 8] |= 1 << (7 - (index % 8));
            }
        }
        extension(&[2, 5, 29, 15], true, &bit_string_spare(&payload))
    }

    fn bit_string_spare(payload: &[u8]) -> Vec<u8> {
        let mut content = Vec::with_capacity(payload.len() + 1);
        content.push(7u8);
        content.extend_from_slice(payload);
        encode_tlv(Tag::BIT_STRING, &content)
    }

    fn eku(oid: &[u32]) -> Vec<u8> {
        extension(
            &[2, 5, 29, 37],
            false,
            &encode_octet_string(&encode_sequence(&encode_oid(oid).expect("eku"))),
        )
    }

    struct CertSpec {
        subject_cn: String,
        issuer_cn: String,
        serial: u64,
        window: (u64, u64),
        extensions: Vec<Vec<u8>>,
        eku_ext: Vec<u8>,
    }

    impl CertSpec {
        fn leaf(serial: u64) -> Self {
            Self {
                subject_cn: "TSA".to_owned(),
                issuer_cn: "TSA Root".to_owned(),
                serial,
                window: (1_500_000_000, 1_800_000_000),
                extensions: vec![
                    basic_constraints(false),
                    key_usage(KeyUsage::DIGITAL_SIGNATURE.bits()),
                ],
                eku_ext: eku(&OID_TIME_STAMPING),
            }
        }

        fn anchor() -> Self {
            Self {
                subject_cn: "TSA Root".to_owned(),
                issuer_cn: "TSA Root".to_owned(),
                serial: 100,
                window: (1_500_000_000, 1_800_000_000),
                extensions: vec![
                    basic_constraints(true),
                    key_usage(KeyUsage::KEY_CERT_SIGN.bits() | KeyUsage::CRL_SIGN.bits()),
                ],
                eku_ext: Vec::new(),
            }
        }

        fn window(mut self, start: u64, end: u64) -> Self {
            self.window = (start, end);
            self
        }

        fn with_eku(mut self, oid: &[u32]) -> Self {
            self.eku_ext = eku(oid);
            self
        }

        fn tbs(&self) -> Vec<u8> {
            let mut out = Vec::new();
            out.extend_from_slice(
                &encode_context(0, true, &encode_integer(2)).expect("version ctx"),
            );
            out.extend_from_slice(&encode_integer(self.serial));
            out.extend_from_slice(&alg_id(&OID_SHA256_WITH_RSA));
            out.extend_from_slice(&name(&self.issuer_cn));
            out.extend_from_slice(&validity(self.window.0, self.window.1));
            out.extend_from_slice(&name(&self.subject_cn));
            let mut spki_children = alg_id(&[1, 2, 840, 113_549, 1, 1, 1]);
            spki_children.extend_from_slice(&bit_string(&[7u8; 32]));
            out.extend_from_slice(&encode_sequence(&spki_children));
            let mut exts = Vec::new();
            for encoded in &self.extensions {
                exts.extend_from_slice(encoded);
            }
            if !self.eku_ext.is_empty() {
                exts.extend_from_slice(&self.eku_ext);
            }
            out.extend_from_slice(
                &encode_context(3, true, &encode_sequence(&exts)).expect("ext ctx"),
            );
            encode_sequence(&out)
        }

        fn build(&self) -> Vec<u8> {
            let tbs = self.tbs();
            let signature = &sha256(&tbs)[..16];
            let mut out = tbs;
            out.extend_from_slice(&alg_id(&OID_SHA256_WITH_RSA));
            out.extend_from_slice(&bit_string(signature));
            encode_sequence(&out)
        }

        fn certificate(&self) -> Certificate {
            Certificate::parse(&self.build()).expect("valid test certificate")
        }
    }

    fn trust_store() -> TrustStore {
        TrustStore::new().with_anchor(CertSpec::anchor().certificate())
    }

    fn provider(cert_der: Vec<u8>) -> crate::InMemorySignatureProvider {
        let signer = SignerIdentifier::new(name("TSA Root"), 7).expect("signer");
        crate::InMemorySignatureProvider::new(signer, cert_der, "RFC3161 TSA").expect("provider")
    }

    fn params(gen_time: u64, nonce: Option<u64>) -> TimestampTokenParams<'static> {
        TimestampTokenParams {
            data: b"document-bytes-to-stamp".to_vec().leak(),
            digest_algorithm: DigestAlgorithm::Sha256,
            policy: vec![1, 2, 3, 4, 5],
            serial_number: 42,
            gen_time,
            accuracy: Some(Accuracy {
                seconds: Some(1),
                millis: Some(500),
                micros: None,
            }),
            ordering: true,
            nonce,
            tsa: Some(encode_tlv(
                Tag::context(0, false).expect("tsa tag"),
                b"tsa.pdfxi.example",
            )),
        }
    }

    struct Fixture {
        token: Vec<u8>,
        provider: crate::InMemorySignatureProvider,
    }

    fn fixture(gen_time: u64, nonce: Option<u64>) -> Fixture {
        let cert_der = CertSpec::leaf(7).build();
        let provider = provider(cert_der);
        let token = build_timestamp_token(&params(gen_time, nonce), &provider).expect("token");
        Fixture { token, provider }
    }

    struct ContextRefs<'a> {
        data: &'a [u8],
        provider: &'a dyn SignatureProvider,
        store: TrustStore,
        intermediates: Vec<Certificate>,
        verifier: DigestVerifier,
        chain_policy: ValidationPolicy,
        clock: FixedClock,
        policy: TimestampPolicy,
    }

    impl<'a> ContextRefs<'a> {
        fn context(&'a self) -> TimestampValidationContext<'a> {
            TimestampValidationContext {
                data: self.data,
                provider: self.provider,
                intermediates: &self.intermediates,
                trust_store: &self.store,
                chain_verifier: &self.verifier,
                chain_policy: &self.chain_policy,
                now_clock: &self.clock,
                policy: &self.policy,
            }
        }

        fn untrusted(mut self) -> Self {
            self.store = TrustStore::new();
            self
        }

        fn with_nonce(mut self, expected: u64) -> Self {
            self.policy.expected_nonce = Some(expected);
            self
        }

        fn with_skew(mut self, skew: u64) -> Self {
            self.policy.max_future_skew_seconds = skew;
            self
        }

        fn without_eku_requirement(mut self) -> Self {
            self.policy.required_tsa_eku = None;
            self
        }
    }

    fn context_for<'a>(data: &'a [u8], provider: &'a dyn SignatureProvider) -> ContextRefs<'a> {
        ContextRefs {
            data,
            provider,
            store: trust_store(),
            intermediates: Vec::new(),
            verifier: DigestVerifier,
            chain_policy: ValidationPolicy::default(),
            clock: FixedClock::new(NOW),
            policy: TimestampPolicy::default(),
        }
    }

    fn find_subsequence(haystack: &[u8], needle: &[u8]) -> Option<usize> {
        if needle.is_empty() || needle.len() > haystack.len() {
            return None;
        }
        (0..=haystack.len() - needle.len())
            .find(|&start| &haystack[start..start + needle.len()] == needle)
    }

    #[test]
    fn valid_token_validates_and_maps_to_valid_status() {
        let fixture = fixture(NOW, Some(99));
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider).with_nonce(99);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("valid token validates");
        assert_eq!(report.status(), TimestampStatus::Valid);
        assert!(report.is_valid());
        assert_eq!(report.info().gen_time, NOW);
        assert_eq!(report.info().nonce, Some(99));
        assert_eq!(report.to_signature_status(), SignatureStatus::Valid);
        let info = report.to_signature_info();
        assert!(info.is_valid());
        assert_eq!(info.signed_at(), NOW);
        assert_eq!(info.signer(), "RFC3161 TSA");
        assert_eq!(info.digest_algorithm(), DigestAlgorithm::Sha256);
        assert_eq!(info.coverage(), SignatureCoverage::WholeDocument);
    }

    #[test]
    fn wrong_message_imprint_is_distinguished() {
        let fixture = fixture(NOW, None);
        let refs = context_for(b"different-document-bytes", &fixture.provider);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::ImprintMismatch);
        assert_eq!(report.to_signature_status(), SignatureStatus::Tampered);
        assert!(!report.is_valid());
    }

    #[test]
    fn bad_cms_signature_is_distinguished() {
        let fixture = fixture(NOW, None);
        let rejecting = RejectingSignatureProvider {
            inner: fixture.provider.clone(),
        };
        let refs = context_for(b"document-bytes-to-stamp", &rejecting);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::CryptographicFailure);
        assert_eq!(report.to_signature_status(), SignatureStatus::Tampered);
    }

    #[test]
    fn tampered_tst_info_digest_is_detected() {
        let fixture = fixture(NOW, None);
        let imprint = digest(DigestAlgorithm::Sha256, b"document-bytes-to-stamp").expect("imprint");
        let mut token = fixture.token;
        let offset = find_subsequence(&token, &imprint).expect("imprint present in token");
        token[offset + imprint.len() - 1] ^= 0x01;
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider);
        let report =
            validate_timestamp_token(&token, &refs.context()).expect("structure still parses");
        assert_eq!(report.status(), TimestampStatus::CryptographicFailure);
    }

    #[test]
    fn untrusted_tsa_is_distinguished() {
        let fixture = fixture(NOW, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider).untrusted();
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::UntrustedTsa);
        assert_eq!(report.to_signature_status(), SignatureStatus::Unknown);
    }

    #[test]
    fn trusted_tsa_without_timestamping_eku_is_rejected() {
        // Chain-trusted and time-valid, but the leaf carries only serverAuth:
        // RFC 3161 section 2.3 requires id-kp-timeStamping.
        let cert_der = CertSpec::leaf(7).with_eku(&OID_SERVER_AUTH).build();
        let provider = provider(cert_der);
        let token = build_timestamp_token(&params(NOW, None), &provider).expect("token");
        let refs = context_for(b"document-bytes-to-stamp", &provider);
        let report =
            validate_timestamp_token(&token, &refs.context()).expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::TimestampEkuMissing);
        assert_eq!(report.to_signature_status(), SignatureStatus::Unknown);
    }

    #[test]
    fn eku_requirement_can_be_explicitly_overridden() {
        let cert_der = CertSpec::leaf(7).with_eku(&OID_SERVER_AUTH).build();
        let provider = provider(cert_der);
        let token = build_timestamp_token(&params(NOW, None), &provider).expect("token");
        let refs = context_for(b"document-bytes-to-stamp", &provider).without_eku_requirement();
        let report =
            validate_timestamp_token(&token, &refs.context()).expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::Valid);
    }

    #[test]
    fn timestamp_outside_certificate_validity_is_distinguished() {
        // TSA certificate expired well before genTime; genTime itself is not in
        // the future, so the future-skew guard does not mask this dimension.
        let cert_der = CertSpec::leaf(7)
            .window(1_000_000_000, 1_100_000_000)
            .build();
        let provider = provider(cert_der);
        let token = build_timestamp_token(&params(NOW, None), &provider).expect("token");
        let refs = context_for(b"document-bytes-to-stamp", &provider);
        let report =
            validate_timestamp_token(&token, &refs.context()).expect("structurally valid token");
        assert_eq!(
            report.status(),
            TimestampStatus::CertificateNotValidAtGenTime
        );
        assert_eq!(report.to_signature_status(), SignatureStatus::Expired);
    }

    #[test]
    fn nonce_mismatch_is_distinguished() {
        let fixture = fixture(NOW, Some(11));
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider).with_nonce(22);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::NonceMismatch);
        assert_eq!(report.to_signature_status(), SignatureStatus::Invalid);
    }

    #[test]
    fn missing_nonce_when_required_is_a_mismatch() {
        let fixture = fixture(NOW, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider).with_nonce(22);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::NonceMismatch);
    }

    #[test]
    fn future_timestamp_beyond_skew_is_rejected() {
        let fixture = fixture(NOW + 1_000, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::FutureTime);
        assert_eq!(report.to_signature_status(), SignatureStatus::Invalid);
    }

    #[test]
    fn future_timestamp_within_skew_is_accepted() {
        let fixture = fixture(NOW + 100, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider);
        let report =
            validate_timestamp_token(&fixture.token, &refs.context()).expect("within skew");
        assert_eq!(report.status(), TimestampStatus::Valid);
    }

    #[test]
    fn future_timestamp_rejection_respects_policy_skew() {
        let fixture = fixture(NOW + 100, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider).with_skew(10);
        let report = validate_timestamp_token(&fixture.token, &refs.context())
            .expect("structurally valid token");
        assert_eq!(report.status(), TimestampStatus::FutureTime);
    }

    #[test]
    fn malformed_der_produces_typed_error() {
        let fixture = fixture(NOW, None);
        let refs = context_for(b"document-bytes-to-stamp", &fixture.provider);
        let truncated = &fixture.token[..fixture.token.len() - 3];
        assert!(matches!(
            validate_timestamp_token(truncated, &refs.context()),
            Err(SecurityError::DerTruncated)
                | Err(SecurityError::DerInvalid)
                | Err(SecurityError::DerUnexpectedTag { .. })
        ));
        let mut trailing = fixture.token.clone();
        trailing.push(0x00);
        assert_eq!(
            validate_timestamp_token(&trailing, &refs.context()).map(|_| ()),
            Err(SecurityError::DerTrailingBytes)
        );
    }

    #[test]
    fn missing_fields_produce_typed_error() {
        let mut info = TstInfo {
            version: 1,
            policy: vec![1, 2, 3],
            message_imprint: MessageImprint {
                digest_algorithm: DigestAlgorithm::Sha256,
                hashed_message: vec![0; 32],
            },
            serial_number: 1,
            gen_time: NOW,
            accuracy: None,
            ordering: false,
            nonce: None,
            tsa: None,
        };
        // Drop `genTime` and every later field by truncating the SEQUENCE
        // content: the parser must report a missing required field.
        let mut der = encode_tst_info(&info).expect("encode");
        der.truncate(der.len() - 16);
        assert!(matches!(
            parse_tst_info(&der),
            Err(SecurityError::TimestampFieldMissing(_)) | Err(SecurityError::DerTruncated)
        ));

        // An explicit unsupported version is rejected distinctly.
        info.version = 2;
        let version_2 = encode_tst_info(&info).expect("encode");
        assert_eq!(
            parse_tst_info(&version_2),
            Err(SecurityError::TimestampVersionUnsupported(2))
        );
    }

    #[test]
    fn tst_info_round_trips_all_fields() {
        let info = params(NOW, Some(77));
        let built = TstInfo {
            version: 1,
            policy: info.policy.clone(),
            message_imprint: MessageImprint {
                digest_algorithm: info.digest_algorithm,
                hashed_message: digest(info.digest_algorithm, info.data).expect("imprint"),
            },
            serial_number: info.serial_number,
            gen_time: info.gen_time,
            accuracy: info.accuracy.clone(),
            ordering: info.ordering,
            nonce: info.nonce,
            tsa: info.tsa.clone(),
        };
        let der = encode_tst_info(&built).expect("encode");
        assert_eq!(parse_tst_info(&der).expect("parse"), built);
    }

    fn sample_tst_info() -> TstInfo {
        TstInfo {
            version: 1,
            policy: vec![1, 2, 3, 4, 5],
            message_imprint: MessageImprint {
                digest_algorithm: DigestAlgorithm::Sha256,
                hashed_message: digest(DigestAlgorithm::Sha256, b"document-bytes-to-stamp")
                    .expect("imprint"),
            },
            serial_number: 42,
            gen_time: NOW,
            accuracy: None,
            ordering: false,
            nonce: None,
            tsa: None,
        }
    }

    #[test]
    fn status_mapping_keeps_valid_conservative() {
        let mut seen_valid = 0usize;
        for status in [
            TimestampStatus::Valid,
            TimestampStatus::CryptographicFailure,
            TimestampStatus::ImprintMismatch,
            TimestampStatus::UntrustedTsa,
            TimestampStatus::TimestampEkuMissing,
            TimestampStatus::CertificateNotValidAtGenTime,
            TimestampStatus::FutureTime,
            TimestampStatus::NonceMismatch,
        ] {
            let report = TimestampValidation {
                info: sample_tst_info(),
                status,
                signer_display: "TSA".to_owned(),
            };
            let mapped = report.to_signature_status();
            if mapped.is_valid() {
                seen_valid += 1;
                assert_eq!(status, TimestampStatus::Valid);
            }
            assert_eq!(report.is_valid(), status.is_valid());
            assert!(!status.label().is_empty());
        }
        assert_eq!(seen_valid, 1);
    }
}
