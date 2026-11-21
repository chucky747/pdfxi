//! X.509 v3 extension subset: basic constraints, key usage, SAN, and EKU.
//!
//! An `Extension` is parsed from its DER `SEQUENCE`. Known extensions are
//! decoded into typed values; unknown extensions keep their raw payload and, if
//! marked critical, are reported so chain validation can refuse them. No
//! extension value is ever logged.

use crate::der::{DerReader, Tag, Tlv};
use crate::error::{Result, SecurityError};
use crate::x509::oid_string;

/// OID `2.5.29.19` — `basicConstraints`.
pub(crate) const OID_BASIC_CONSTRAINTS: [u32; 4] = [2, 5, 29, 19];
/// OID `2.5.29.15` — `keyUsage`.
const OID_KEY_USAGE: [u32; 4] = [2, 5, 29, 15];
/// OID `2.5.29.17` — `subjectAltName`.
const OID_SUBJECT_ALT_NAME: [u32; 4] = [2, 5, 29, 17];
/// OID `2.5.29.37` — `extKeyUsage`.
const OID_EXT_KEY_USAGE: [u32; 4] = [2, 5, 29, 37];

/// OID `1.3.6.1.5.5.7.3.1` — `serverAuth`.
const OID_SERVER_AUTH: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 1];
/// OID `1.3.6.1.5.5.7.3.2` — `clientAuth`.
const OID_CLIENT_AUTH: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 2];
/// OID `1.3.6.1.5.5.7.3.3` — `codeSigning`.
const OID_CODE_SIGNING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 3];
/// OID `1.3.6.1.5.5.7.3.4` — `emailProtection`.
const OID_EMAIL_PROTECTION: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 4];
/// OID `1.3.6.1.5.5.7.3.8` — `timeStamping`.
const OID_TIME_STAMPING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 8];
/// OID `1.3.6.1.5.5.7.3.9` — `OCSPSigning`.
const OID_OCSP_SIGNING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 9];
/// OID `1.2.840.113583.1.1.5` — Adobe document signing.
pub(crate) const OID_DOCUMENT_SIGNING: [u32; 7] = [1, 2, 840, 113_583, 1, 1, 5];
/// OID `2.5.29.37.0` — `anyExtendedKeyUsage`.
const OID_ANY_EKU: [u32; 5] = [2, 5, 29, 37, 0];

fn bit_set(payload: &[u8], index: usize) -> bool {
    let byte = index / 8;
    let mask = 1u8 << (7 - (index % 8));
    payload.get(byte).is_some_and(|value| value & mask != 0)
}

/// RFC 5280 basic constraints: whether the certificate is a CA and its path limit.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BasicConstraints {
    ca: bool,
    path_len: Option<u64>,
}

impl BasicConstraints {
    /// Return `true` when the certificate may issue other certificates.
    pub const fn is_ca(self) -> bool {
        self.ca
    }

    /// Return the `pathLenConstraint`, when present.
    pub const fn path_len(self) -> Option<u64> {
        self.path_len
    }
}

fn parse_basic_constraints(value: &[u8]) -> Result<BasicConstraints> {
    let mut reader = DerReader::new(value);
    let seq_tlv = reader.read_tlv()?;
    reader.expect_end()?;
    if seq_tlv.tag() != Tag::SEQUENCE {
        return Err(SecurityError::CertificateExtensionInvalid(
            "basicConstraints",
        ));
    }
    let mut sequence = seq_tlv.as_sequence()?;
    let ca = if sequence.peek_tag() == Some(Tag::BOOLEAN) {
        sequence.read_tlv()?.as_bool()?
    } else {
        false
    };
    let path_len = if sequence.peek_tag() == Some(Tag::INTEGER) {
        Some(sequence.read_tlv()?.as_integer_u64()?)
    } else {
        None
    };
    sequence.expect_end()?;
    Ok(BasicConstraints { ca, path_len })
}

/// RFC 5280 key usage bits (BIT STRING positions 0 through 8).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyUsageBit {
    /// Bit 0 — signatures other than CRL/CA signatures.
    DigitalSignature,
    /// Bit 1 — non-repudiation / content commitment.
    NonRepudiation,
    /// Bit 2 — RSA key encipherment.
    KeyEncipherment,
    /// Bit 3 — symmetric data encipherment.
    DataEncipherment,
    /// Bit 4 — key agreement.
    KeyAgreement,
    /// Bit 5 — signing certificates and CRLs.
    KeyCertSign,
    /// Bit 6 — signing CRLs.
    CrlSign,
    /// Bit 7 — encipher only during key agreement.
    EncipherOnly,
    /// Bit 8 — decipher only during key agreement.
    DecipherOnly,
}

impl KeyUsageBit {
    const fn index(self) -> usize {
        match self {
            Self::DigitalSignature => 0,
            Self::NonRepudiation => 1,
            Self::KeyEncipherment => 2,
            Self::DataEncipherment => 3,
            Self::KeyAgreement => 4,
            Self::KeyCertSign => 5,
            Self::CrlSign => 6,
            Self::EncipherOnly => 7,
            Self::DecipherOnly => 8,
        }
    }

    const fn flag(self) -> u16 {
        1u16 << self.index()
    }

    /// Stable label used in key-usage violation errors.
    pub const fn label(self) -> &'static str {
        match self {
            Self::DigitalSignature => "digitalSignature",
            Self::NonRepudiation => "nonRepudiation",
            Self::KeyEncipherment => "keyEncipherment",
            Self::DataEncipherment => "dataEncipherment",
            Self::KeyAgreement => "keyAgreement",
            Self::KeyCertSign => "keyCertSign",
            Self::CrlSign => "cRLSign",
            Self::EncipherOnly => "encipherOnly",
            Self::DecipherOnly => "decipherOnly",
        }
    }
}

/// The decoded key usage bit mask.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct KeyUsage(u16);

impl KeyUsage {
    /// Bit 0 — digital signature.
    pub const DIGITAL_SIGNATURE: Self = Self(KeyUsageBit::DigitalSignature.flag());
    /// Bit 1 — non-repudiation.
    pub const NON_REPUDIATION: Self = Self(KeyUsageBit::NonRepudiation.flag());
    /// Bit 2 — key encipherment.
    pub const KEY_ENCIPHERMENT: Self = Self(KeyUsageBit::KeyEncipherment.flag());
    /// Bit 3 — data encipherment.
    pub const DATA_ENCIPHERMENT: Self = Self(KeyUsageBit::DataEncipherment.flag());
    /// Bit 4 — key agreement.
    pub const KEY_AGREEMENT: Self = Self(KeyUsageBit::KeyAgreement.flag());
    /// Bit 5 — certificate signing.
    pub const KEY_CERT_SIGN: Self = Self(KeyUsageBit::KeyCertSign.flag());
    /// Bit 6 — CRL signing.
    pub const CRL_SIGN: Self = Self(KeyUsageBit::CrlSign.flag());
    /// Bit 7 — encipher only.
    pub const ENCIPHER_ONLY: Self = Self(KeyUsageBit::EncipherOnly.flag());
    /// Bit 8 — decipher only.
    pub const DECIPHER_ONLY: Self = Self(KeyUsageBit::DecipherOnly.flag());

    /// Build a key usage mask from raw bits.
    pub const fn from_bits(bits: u16) -> Self {
        Self(bits)
    }

    /// Return the raw bit mask.
    pub const fn bits(self) -> u16 {
        self.0
    }

    /// Return `true` when all bits in `other` are present.
    pub const fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }

    /// Return `true` when no bits are set.
    pub const fn is_empty(self) -> bool {
        self.0 == 0
    }

    /// Return `true` when the given RFC 5280 usage bit is set.
    pub const fn permits(self, usage: KeyUsageBit) -> bool {
        self.contains(Self(usage.flag()))
    }
}

fn parse_key_usage(value: &[u8]) -> Result<KeyUsage> {
    let mut reader = DerReader::new(value);
    let tlv = reader.read_tlv()?;
    reader.expect_end()?;
    if tlv.tag() != Tag::BIT_STRING {
        return Err(SecurityError::CertificateExtensionInvalid("keyUsage"));
    }
    let (unused, payload) = tlv.as_bit_string()?;
    let total_bits = payload
        .len()
        .checked_mul(8)
        .and_then(|bits| bits.checked_sub(usize::from(unused)))
        .ok_or(SecurityError::CertificateExtensionInvalid("keyUsage"))?;
    if let Some(last) = payload.last() {
        let mask = (1u8 << unused) - 1;
        if last & mask != 0 {
            return Err(SecurityError::CertificateExtensionInvalid("keyUsage"));
        }
    }
    let mut bits = 0u16;
    for usage in [
        KeyUsageBit::DigitalSignature,
        KeyUsageBit::NonRepudiation,
        KeyUsageBit::KeyEncipherment,
        KeyUsageBit::DataEncipherment,
        KeyUsageBit::KeyAgreement,
        KeyUsageBit::KeyCertSign,
        KeyUsageBit::CrlSign,
        KeyUsageBit::EncipherOnly,
        KeyUsageBit::DecipherOnly,
    ] {
        if usage.index() < total_bits && bit_set(payload, usage.index()) {
            bits |= usage.flag();
        }
    }
    Ok(KeyUsage::from_bits(bits))
}

fn decode_ia5(content: &[u8]) -> Result<String> {
    if content.iter().any(|byte| *byte > 0x7f) {
        return Err(SecurityError::CertificateExtensionInvalid("subjectAltName"));
    }
    Ok(content.iter().copied().map(char::from).collect())
}

/// RFC 5280 subject alternative names (the common GeneralName forms).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SubjectAltName {
    rfc822_names: Vec<String>,
    dns_names: Vec<String>,
    uris: Vec<String>,
    ip_addresses: Vec<Vec<u8>>,
    registered_ids: Vec<Vec<u32>>,
}

impl SubjectAltName {
    /// Return `rfc822Name` (email) entries.
    pub fn rfc822_names(&self) -> &[String] {
        &self.rfc822_names
    }

    /// Return `dNSName` entries.
    pub fn dns_names(&self) -> &[String] {
        &self.dns_names
    }

    /// Return `uniformResourceIdentifier` entries.
    pub fn uris(&self) -> &[String] {
        &self.uris
    }

    /// Return raw `iPAddress` octets (4 or 16 bytes per entry).
    pub fn ip_addresses(&self) -> &[Vec<u8>] {
        &self.ip_addresses
    }

    /// Return `registeredID` OID arcs.
    pub fn registered_ids(&self) -> &[Vec<u32>] {
        &self.registered_ids
    }

    /// Return `true` when no supported general names were present.
    pub fn is_empty(&self) -> bool {
        self.rfc822_names.is_empty()
            && self.dns_names.is_empty()
            && self.uris.is_empty()
            && self.ip_addresses.is_empty()
            && self.registered_ids.is_empty()
    }
}

fn parse_subject_alt_name(value: &[u8]) -> Result<SubjectAltName> {
    // `parse_extension` already unwrapped the extension's `extnValue` OCTET
    // STRING, so `value` is the `GeneralNames` SEQUENCE itself.
    let mut wrapper = DerReader::new(value);
    let names_tlv = wrapper.read_tlv()?;
    wrapper.expect_end()?;
    if names_tlv.tag() != Tag::SEQUENCE {
        return Err(SecurityError::CertificateExtensionInvalid("subjectAltName"));
    }
    let mut names = names_tlv.as_sequence()?;
    let mut result = SubjectAltName::default();
    while !names.is_empty() {
        let tlv = names.read_tlv()?;
        let tag = tlv.tag().byte();
        if tag
            == Tag::context(1, false)
                .ok_or(SecurityError::DerInvalid)?
                .byte()
        {
            result.rfc822_names.push(decode_ia5(tlv.content())?);
        } else if tag
            == Tag::context(2, false)
                .ok_or(SecurityError::DerInvalid)?
                .byte()
        {
            result.dns_names.push(decode_ia5(tlv.content())?);
        } else if tag
            == Tag::context(6, false)
                .ok_or(SecurityError::DerInvalid)?
                .byte()
        {
            result.uris.push(decode_ia5(tlv.content())?);
        } else if tag
            == Tag::context(7, false)
                .ok_or(SecurityError::DerInvalid)?
                .byte()
        {
            // `[7] iPAddress` replaces the OCTET STRING tag implicitly.
            let address = tlv.content();
            if address.len() != 4 && address.len() != 16 {
                return Err(SecurityError::CertificateExtensionInvalid("subjectAltName"));
            }
            result.ip_addresses.push(address.to_vec());
        } else if tag
            == Tag::context(8, false)
                .ok_or(SecurityError::DerInvalid)?
                .byte()
        {
            // `[8] registeredID` replaces the OBJECT IDENTIFIER tag implicitly.
            result
                .registered_ids
                .push(crate::der::decode_oid_content(tlv.content())?);
        } else {
            return Err(SecurityError::CertificateExtensionInvalid("subjectAltName"));
        }
    }
    Ok(result)
}

/// A recognised extended key usage purpose.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EkuPurpose {
    /// TLS server authentication.
    ServerAuth,
    /// TLS client authentication.
    ClientAuth,
    /// Object code signing.
    CodeSigning,
    /// S/MIME email protection.
    EmailProtection,
    /// RFC 3161 time stamping.
    TimeStamping,
    /// OCSP response signing.
    OcspSigning,
    /// Adobe document signing (PDF signatures).
    DocumentSigning,
    /// `anyExtendedKeyUsage` — permits every purpose.
    AnyExtendedKeyUsage,
    /// An unrecognised purpose, retained as its OID.
    Other(Vec<u32>),
}

impl EkuPurpose {
    fn from_oid(oid: &[u32]) -> Self {
        match oid {
            o if o == OID_SERVER_AUTH => Self::ServerAuth,
            o if o == OID_CLIENT_AUTH => Self::ClientAuth,
            o if o == OID_CODE_SIGNING => Self::CodeSigning,
            o if o == OID_EMAIL_PROTECTION => Self::EmailProtection,
            o if o == OID_TIME_STAMPING => Self::TimeStamping,
            o if o == OID_OCSP_SIGNING => Self::OcspSigning,
            o if o == OID_DOCUMENT_SIGNING => Self::DocumentSigning,
            o if o == OID_ANY_EKU => Self::AnyExtendedKeyUsage,
            other => Self::Other(other.to_vec()),
        }
    }

    /// Return the OID arcs for this purpose.
    pub fn oid(&self) -> Vec<u32> {
        match self {
            Self::ServerAuth => OID_SERVER_AUTH.to_vec(),
            Self::ClientAuth => OID_CLIENT_AUTH.to_vec(),
            Self::CodeSigning => OID_CODE_SIGNING.to_vec(),
            Self::EmailProtection => OID_EMAIL_PROTECTION.to_vec(),
            Self::TimeStamping => OID_TIME_STAMPING.to_vec(),
            Self::OcspSigning => OID_OCSP_SIGNING.to_vec(),
            Self::DocumentSigning => OID_DOCUMENT_SIGNING.to_vec(),
            Self::AnyExtendedKeyUsage => OID_ANY_EKU.to_vec(),
            Self::Other(oid) => oid.clone(),
        }
    }

    /// Stable label (dotted OID for unrecognised purposes).
    pub fn label(&self) -> String {
        match self {
            Self::ServerAuth => "serverAuth".to_owned(),
            Self::ClientAuth => "clientAuth".to_owned(),
            Self::CodeSigning => "codeSigning".to_owned(),
            Self::EmailProtection => "emailProtection".to_owned(),
            Self::TimeStamping => "timeStamping".to_owned(),
            Self::OcspSigning => "ocspSigning".to_owned(),
            Self::DocumentSigning => "documentSigning".to_owned(),
            Self::AnyExtendedKeyUsage => "anyExtendedKeyUsage".to_owned(),
            Self::Other(_) => oid_string(&self.oid()),
        }
    }
}

/// The decoded `extKeyUsage` purposes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtendedKeyUsage {
    purposes: Vec<EkuPurpose>,
}

impl ExtendedKeyUsage {
    /// Return the declared purposes in encoded order.
    pub fn purposes(&self) -> &[EkuPurpose] {
        &self.purposes
    }

    /// Return `true` when `purpose` is allowed.
    ///
    /// `anyExtendedKeyUsage` satisfies every purpose.
    pub fn allows(&self, purpose: &EkuPurpose) -> bool {
        self.purposes
            .iter()
            .any(|declared| declared == purpose || *declared == EkuPurpose::AnyExtendedKeyUsage)
    }
}

fn parse_extended_key_usage(value: &[u8]) -> Result<ExtendedKeyUsage> {
    // `parse_extension` already unwrapped the extension's `extnValue` OCTET
    // STRING, so `value` is the `ExtKeyUsageSyntax` SEQUENCE itself.
    let mut wrapper = DerReader::new(value);
    let seq_tlv = wrapper.read_tlv()?;
    wrapper.expect_end()?;
    if seq_tlv.tag() != Tag::SEQUENCE {
        return Err(SecurityError::CertificateExtensionInvalid("extKeyUsage"));
    }
    let mut sequence = seq_tlv.as_sequence()?;
    let mut purposes = Vec::new();
    while !sequence.is_empty() {
        purposes.push(EkuPurpose::from_oid(&sequence.read_tlv()?.as_oid()?));
    }
    if purposes.is_empty() {
        return Err(SecurityError::CertificateExtensionInvalid("extKeyUsage"));
    }
    Ok(ExtendedKeyUsage { purposes })
}

/// A parsed X.509 extension value.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExtensionValue {
    /// `basicConstraints`.
    BasicConstraints(BasicConstraints),
    /// `keyUsage`.
    KeyUsage(KeyUsage),
    /// `subjectAltName`.
    SubjectAltName(SubjectAltName),
    /// `extKeyUsage`.
    ExtendedKeyUsage(ExtendedKeyUsage),
    /// An unrecognised extension, retained as raw bytes.
    Unknown(Vec<u8>),
}

/// One parsed certificate extension.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extension {
    oid: Vec<u32>,
    critical: bool,
    value: ExtensionValue,
}

impl Extension {
    /// Return the extension OID arcs.
    pub fn oid(&self) -> &[u32] {
        &self.oid
    }

    /// Return `true` when the extension was marked critical.
    pub const fn is_critical(&self) -> bool {
        self.critical
    }

    /// Return the decoded value.
    pub fn value(&self) -> &ExtensionValue {
        &self.value
    }
}

fn parse_extension(tlv: &Tlv<'_>) -> Result<(Extension, Option<Vec<u32>>)> {
    let mut sequence = tlv.as_sequence()?;
    let oid = sequence.read_tlv()?.as_oid()?;
    let critical = if sequence.peek_tag() == Some(Tag::BOOLEAN) {
        sequence.read_tlv()?.as_bool()?
    } else {
        false
    };
    let payload = sequence.read_tlv()?.as_octets()?.to_vec();
    sequence.expect_end()?;

    let (value, unknown_critical) = if oid == OID_BASIC_CONSTRAINTS {
        (
            ExtensionValue::BasicConstraints(parse_basic_constraints(&payload)?),
            None,
        )
    } else if oid == OID_KEY_USAGE {
        (ExtensionValue::KeyUsage(parse_key_usage(&payload)?), None)
    } else if oid == OID_SUBJECT_ALT_NAME {
        (
            ExtensionValue::SubjectAltName(parse_subject_alt_name(&payload)?),
            None,
        )
    } else if oid == OID_EXT_KEY_USAGE {
        (
            ExtensionValue::ExtendedKeyUsage(parse_extended_key_usage(&payload)?),
            None,
        )
    } else {
        let unknown = if critical { Some(oid.clone()) } else { None };
        (ExtensionValue::Unknown(payload), unknown)
    };
    Ok((
        Extension {
            oid,
            critical,
            value,
        },
        unknown_critical,
    ))
}

/// Parse the optional `[3] EXPLICIT Extensions` field of a `TBSCertificate`.
///
/// Returns the decoded known/unknown extensions together with the OIDs of any
/// critical extensions this subset cannot interpret.
pub(crate) fn parse_extensions(wrapper: &Tlv<'_>) -> Result<(Vec<Extension>, Vec<Vec<u32>>)> {
    let expected = Tag::context(3, true).ok_or(SecurityError::DerInvalid)?;
    if wrapper.tag() != expected {
        return Err(SecurityError::DerUnexpectedTag {
            expected: expected.byte(),
            found: wrapper.tag().byte(),
        });
    }
    let mut outer = DerReader::new(wrapper.content());
    let seq_tlv = outer.read_tlv()?;
    outer.expect_end()?;
    let mut sequence = seq_tlv.as_sequence()?;
    let mut extensions = Vec::new();
    let mut unknown_critical = Vec::new();
    let mut seen_oids: Vec<Vec<u32>> = Vec::new();
    while !sequence.is_empty() {
        let (extension, unknown) = parse_extension(&sequence.read_tlv()?)?;
        if seen_oids.iter().any(|oid| oid == extension.oid()) {
            return Err(SecurityError::CertificateDuplicateExtension);
        }
        seen_oids.push(extension.oid().to_vec());
        extensions.push(extension);
        if let Some(oid) = unknown {
            unknown_critical.push(oid);
        }
    }
    Ok((extensions, unknown_critical))
}
