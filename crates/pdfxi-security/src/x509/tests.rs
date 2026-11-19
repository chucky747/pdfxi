//! Tests for the X.509 subset: parsing, extensions, names, chain trust, and IDs.
//!
//! Test certificates are built from the public DER primitives in [`crate::der`]
//! so the vectors are deterministic and require no external key material. The
//! [`DigestVerifier`] test double "signs" by truncating a SHA-256 digest of the
//! TBS bytes; it is not a signature algorithm and is never used outside tests.

use std::sync::Arc;

use crate::crypto::sha::sha256;
use crate::der::{
    DerReader, Tag, encode_context, encode_integer, encode_null, encode_octet_string, encode_oid,
    encode_sequence, encode_set, encode_time, encode_tlv, write_tlv,
};
use crate::error::SecurityError;
use crate::x509::certificate::ValidityStatus;
use crate::x509::extension::{
    EkuPurpose, ExtensionValue, KeyUsage, KeyUsageBit, OID_BASIC_CONSTRAINTS, OID_DOCUMENT_SIGNING,
};
use crate::x509::{Certificate, DigitalId, DigitalIdStore, FixedClock, Name, OpaqueKeyHandle};
use crate::{
    CertificateVerifier, RejectingVerifier, Result, SignatureAlgorithm, TrustStore,
    ValidationPolicy, verify_chain,
};

/// Test-only verifier: treats the first 16 bytes of SHA-256(TBS) as a signature.
struct DigestVerifierForTests;

impl CertificateVerifier for DigestVerifierForTests {
    fn verify_signature(
        &self,
        _algorithm: &SignatureAlgorithm,
        tbs_der: &[u8],
        signature: &[u8],
        _issuer_key: &crate::PublicKeyInfo,
    ) -> Result<bool> {
        Ok(signature == &sha256(tbs_der)[..16])
    }
}

const OID_CN: [u32; 4] = [2, 5, 4, 3];
const OID_O: [u32; 4] = [2, 5, 4, 10];
const OID_C: [u32; 4] = [2, 5, 4, 6];
const OID_KEY_USAGE: [u32; 4] = [2, 5, 29, 15];
const OID_SUBJECT_ALT_NAME: [u32; 4] = [2, 5, 29, 17];
const OID_EXT_KEY_USAGE: [u32; 4] = [2, 5, 29, 37];
const OID_ANY_EKU: [u32; 5] = [2, 5, 29, 37, 0];
const OID_SERVER_AUTH: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 1];
const OID_TIME_STAMPING: [u32; 9] = [1, 3, 6, 1, 5, 5, 7, 3, 8];

fn name(attributes: &[(&[u32], &str)]) -> Vec<u8> {
    let mut out = Vec::new();
    for (oid, value) in attributes {
        let mut ava = encode_oid(oid).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, value.as_bytes());
        out.extend_from_slice(&encode_set(&encode_sequence(&ava)));
    }
    encode_sequence(&out)
}

fn name_from_rdns(rdns: &[Vec<Vec<u8>>]) -> Vec<u8> {
    let mut out = Vec::new();
    for rdn in rdns {
        let mut attributes = Vec::new();
        for ava in rdn {
            attributes.extend_from_slice(ava);
        }
        out.extend_from_slice(&encode_set(&attributes));
    }
    encode_sequence(&out)
}

fn ava(oid: &[u32], value: &str) -> Vec<u8> {
    let mut children = encode_oid(oid).expect("oid");
    write_tlv(&mut children, Tag::UTF8_STRING, value.as_bytes());
    encode_sequence(&children)
}

fn bit_string(payload: &[u8], unused: u8) -> Vec<u8> {
    let mut content = Vec::with_capacity(payload.len() + 1);
    content.push(unused);
    content.extend_from_slice(payload);
    encode_tlv(Tag::BIT_STRING, &content)
}

fn alg_id(oid: &[u32]) -> Vec<u8> {
    let mut children = encode_oid(oid).expect("oid");
    children.extend_from_slice(&encode_null());
    encode_sequence(&children)
}

fn validity(not_before: u64, not_after: u64) -> Vec<u8> {
    let mut children = encode_time(not_before).expect("notBefore");
    children.extend_from_slice(&encode_time(not_after).expect("notAfter"));
    encode_sequence(&children)
}

fn spki(seed: u8) -> Vec<u8> {
    let mut children = alg_id(&[1, 2, 840, 113_549, 1, 1, 1]);
    children.extend_from_slice(&bit_string(&[seed; 32], 0));
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

fn basic_constraints_ext(ca: bool, path_len: Option<u64>) -> Vec<u8> {
    let mut children = Vec::new();
    if ca {
        children.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
    }
    if let Some(length) = path_len {
        children.extend_from_slice(&encode_integer(length));
    }
    extension(&OID_BASIC_CONSTRAINTS, true, &encode_sequence(&children))
}

fn key_usage_ext(bits: u16) -> Vec<u8> {
    let mut payload = vec![0u8; 2];
    for index in 0..9 {
        if bits & (1u16 << index) != 0 {
            payload[index / 8] |= 1 << (7 - (index % 8));
        }
    }
    extension(&OID_KEY_USAGE, true, &bit_string(&payload, 7))
}

fn san_ext() -> Vec<u8> {
    let mut names = Vec::new();
    names.extend_from_slice(&encode_tlv(
        Tag::context(2, false).expect("dNSName tag"),
        b"pdfxi.example",
    ));
    names.extend_from_slice(&encode_tlv(
        Tag::context(1, false).expect("rfc822Name tag"),
        b"signer@pdfxi.example",
    ));
    names.extend_from_slice(&encode_tlv(
        Tag::context(6, false).expect("URI tag"),
        b"https://pdfxi.example/id",
    ));
    names.extend_from_slice(&encode_tlv(
        Tag::context(7, false).expect("iPAddress tag"),
        &[127, 0, 0, 1],
    ));
    names.extend_from_slice(&encode_tlv(
        Tag::context(8, false).expect("registeredID tag"),
        &encode_oid(&[1, 2, 3, 4]).expect("oid")[2..],
    ));
    extension(
        &OID_SUBJECT_ALT_NAME,
        false,
        &encode_octet_string(&encode_sequence(&names)),
    )
}

fn eku_ext(purposes: &[&[u32]]) -> Vec<u8> {
    let mut children = Vec::new();
    for purpose in purposes {
        children.extend_from_slice(&encode_oid(purpose).expect("oid"));
    }
    extension(
        &OID_EXT_KEY_USAGE,
        false,
        &encode_octet_string(&encode_sequence(&children)),
    )
}

struct CertSpec {
    version: u8,
    serial: u64,
    issuer: Vec<u8>,
    subject: Vec<u8>,
    not_before: u64,
    not_after: u64,
    key_seed: u8,
    extensions: Vec<Vec<u8>>,
    signature: Option<Vec<u8>>,
    sig_oid: Vec<u32>,
    outer_sig_oid: Option<Vec<u32>>,
    invalid_signature_bits: bool,
    version_override: Option<u64>,
    validity_override: Option<Vec<u8>>,
    raw_tbs_fields: Vec<Vec<u8>>,
}

impl CertSpec {
    fn new(subject_cn: &str, serial: u64) -> Self {
        let subject = name(&[(&OID_CN, subject_cn)]);
        Self {
            version: 2,
            serial,
            issuer: subject.clone(),
            subject,
            not_before: 1_600_000_000,
            not_after: 1_700_000_000,
            key_seed: 1,
            extensions: Vec::new(),
            signature: None,
            sig_oid: crate::x509::OID_SHA256_WITH_RSA.to_vec(),
            outer_sig_oid: None,
            invalid_signature_bits: false,
            version_override: None,
            validity_override: None,
            raw_tbs_fields: Vec::new(),
        }
    }

    fn issuer_cn(mut self, cn: &str) -> Self {
        self.issuer = name(&[(&OID_CN, cn)]);
        self
    }

    fn subject_der(mut self, der: Vec<u8>) -> Self {
        self.subject = der;
        self
    }

    fn version(mut self, version: u8) -> Self {
        self.version = version;
        self
    }

    fn serial(mut self, serial: u64) -> Self {
        self.serial = serial;
        self
    }

    fn window(mut self, not_before: u64, not_after: u64) -> Self {
        self.not_before = not_before;
        self.not_after = not_after;
        self
    }

    fn key_seed(mut self, seed: u8) -> Self {
        self.key_seed = seed;
        self
    }

    fn ext(mut self, encoded: Vec<u8>) -> Self {
        self.extensions.push(encoded);
        self
    }

    fn unsigned(mut self) -> Self {
        self.signature = Some(vec![0xa5; 16]);
        self
    }

    fn signature_override(mut self, bytes: Vec<u8>) -> Self {
        self.signature = Some(bytes);
        self
    }

    fn sig_oid(mut self, oid: Vec<u32>) -> Self {
        self.sig_oid = oid;
        self
    }

    fn bad_signature_bits(mut self) -> Self {
        self.invalid_signature_bits = true;
        self
    }

    fn version_override(mut self, value: u64) -> Self {
        self.version_override = Some(value);
        self
    }

    fn validity_override(mut self, der: Vec<u8>) -> Self {
        self.validity_override = Some(der);
        self
    }

    fn mismatched_outer_algorithm(mut self) -> Self {
        self.outer_sig_oid = Some(crate::x509::OID_SHA1_WITH_RSA.to_vec());
        self
    }

    fn raw_tbs_field(mut self, der: Vec<u8>) -> Self {
        self.raw_tbs_fields.push(der);
        self
    }

    fn tbs(&self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.version >= 1 {
            let version = self.version_override.unwrap_or(u64::from(self.version));
            out.extend_from_slice(
                &encode_context(0, true, &encode_integer(version)).expect("ctx0"),
            );
        }
        out.extend_from_slice(&encode_integer(self.serial));
        out.extend_from_slice(&alg_id(&self.sig_oid));
        out.extend_from_slice(&self.issuer);
        match &self.validity_override {
            Some(der) => out.extend_from_slice(der),
            None => out.extend_from_slice(&validity(self.not_before, self.not_after)),
        }
        out.extend_from_slice(&self.subject);
        out.extend_from_slice(&spki(self.key_seed));
        if !self.extensions.is_empty() {
            let mut inner = Vec::new();
            for encoded in &self.extensions {
                inner.extend_from_slice(encoded);
            }
            out.extend_from_slice(
                &encode_context(3, true, &encode_sequence(&inner)).expect("ctx3"),
            );
        }
        for field in &self.raw_tbs_fields {
            out.extend_from_slice(field);
        }
        encode_sequence(&out)
    }

    fn build(&self) -> Vec<u8> {
        let tbs = self.tbs();
        let signature = match &self.signature {
            Some(bytes) => bytes.clone(),
            None => sha256(&tbs)[..16].to_vec(),
        };
        let mut out = tbs;
        let outer = self.outer_sig_oid.as_deref().unwrap_or(&self.sig_oid);
        out.extend_from_slice(&alg_id(outer));
        let unused = if self.invalid_signature_bits { 8 } else { 0 };
        out.extend_from_slice(&bit_string(&signature, unused));
        encode_sequence(&out)
    }
}

fn cert(spec: &CertSpec) -> Certificate {
    Certificate::parse(&spec.build()).expect("valid test certificate")
}

fn parsed_name(der: &[u8]) -> Name {
    cert(&CertSpec::new("subject", 1).subject_der(der.to_vec()))
        .subject()
        .clone()
}

fn anchor(name: &str) -> Certificate {
    cert(
        &CertSpec::new(name, 100)
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(
                KeyUsage::KEY_CERT_SIGN.bits() | KeyUsage::CRL_SIGN.bits(),
            )),
    )
}

fn use_digest_verifier() -> DigestVerifierForTests {
    DigestVerifierForTests
}

fn fixed_clock() -> FixedClock {
    FixedClock::new(1_650_000_000)
}

fn leaf_signed_by(name: &str) -> Certificate {
    cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn(name)
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits())),
    )
}

#[test]
fn parse_certificate_extracts_all_fields() {
    let parsed = cert(
        &CertSpec::new("Leaf", 42)
            .key_seed(9)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits()))
            .ext(san_ext())
            .ext(eku_ext(&[&OID_DOCUMENT_SIGNING])),
    );

    assert_eq!(parsed.version(), 2);
    assert_eq!(parsed.serial_number(), 42);
    assert_eq!(parsed.subject().display_rfc4514(), "CN=Leaf");
    assert_eq!(parsed.issuer().display_rfc4514(), "CN=Root");
    assert_eq!(parsed.validity().not_before(), 1_600_000_000);
    assert_eq!(parsed.validity().not_after(), 1_700_000_000);
    assert_eq!(
        parsed.signature_algorithm(),
        &SignatureAlgorithm::Sha256WithRsa
    );
    assert_eq!(
        parsed.outer_signature_algorithm(),
        &SignatureAlgorithm::Sha256WithRsa
    );
    assert_eq!(parsed.extensions().len(), 4);
    assert_eq!(parsed.signature_bits().len(), 16);
    assert_eq!(parsed.fingerprint_sha256().len(), 32);
    assert_eq!(parsed.subject_public_key_info().bit_len(), 256);
    assert!(!parsed.tbs_der().is_empty());

    let constraints = parsed.basic_constraints().expect("basic constraints");
    assert!(!constraints.is_ca());
    assert_eq!(constraints.path_len(), None);
    assert!(
        parsed
            .key_usage()
            .expect("key usage")
            .permits(KeyUsageBit::DigitalSignature)
    );
    let san = parsed.subject_alt_name().expect("san");
    assert_eq!(san.dns_names(), ["pdfxi.example"]);
    assert_eq!(san.rfc822_names(), ["signer@pdfxi.example"]);
    assert_eq!(san.uris(), ["https://pdfxi.example/id"]);
    assert_eq!(san.ip_addresses(), [vec![127, 0, 0, 1]]);
    let eku = parsed.extended_key_usage().expect("eku");
    assert!(eku.allows(&EkuPurpose::DocumentSigning));
}

#[test]
fn parse_certificate_rejects_truncated_and_trailing_der() {
    let der = CertSpec::new("Leaf", 1).build();
    let truncated = &der[..der.len() - 1];
    assert_eq!(
        Certificate::parse(truncated).map(|_| ()),
        Err(SecurityError::DerTruncated)
    );
    let mut trailing = der;
    trailing.push(0);
    assert_eq!(
        Certificate::parse(&trailing).map(|_| ()),
        Err(SecurityError::DerTrailingBytes)
    );
    assert_eq!(
        Certificate::parse(&[]).map(|_| ()),
        Err(SecurityError::DerTruncated)
    );
}

#[test]
fn parse_certificate_rejects_malformed_structure() {
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).version_override(5).build()).map(|_| ()),
        Err(SecurityError::CertificateVersionUnsupported(5))
    );
    assert_eq!(
        Certificate::parse(
            &CertSpec::new("Leaf", 1)
                .mismatched_outer_algorithm()
                .build()
        )
        .map(|_| ()),
        Err(SecurityError::DerInvalid)
    );
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).bad_signature_bits().build()).map(|_| ()),
        Err(SecurityError::DerInvalid)
    );
    assert_eq!(
        Certificate::parse(
            &CertSpec::new("Leaf", 1)
                .version(0)
                .ext(basic_constraints_ext(true, None))
                .build()
        )
        .map(|_| ()),
        Err(SecurityError::DerInvalid)
    );
    let reversed = validity(2_000_000_000, 1_000_000_000);
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).validity_override(reversed).build())
            .map(|_| ()),
        Err(SecurityError::DerInvalid)
    );
}

#[test]
fn name_display_is_deterministic_and_escaped() {
    let common_name = ava(&OID_CN, "Leaf");
    let organization = ava(&OID_O, "PDFXI, Inc.");
    let country = ava(&OID_C, "US");
    let der = name_from_rdns(&[
        vec![organization.clone(), country.clone()],
        vec![common_name.clone()],
    ]);
    let parsed = parsed_name(&der);
    assert_eq!(parsed.display_rfc4514(), "CN=Leaf,O=PDFXI\\, Inc.+C=US");
    assert_eq!(parsed.to_string(), "CN=Leaf,O=PDFXI\\, Inc.+C=US");
    assert_eq!(parsed.first_attribute_value(&OID_CN), Some("Leaf"));
    assert!(parsed.first_attribute_value(&[1, 2, 3, 4]).is_none());

    // Reversing the SET OF order must not change the deterministic display.
    let reordered = name_from_rdns(&[vec![country, organization], vec![common_name]]);
    assert_eq!(
        parsed_name(&reordered).display_rfc4514(),
        parsed.display_rfc4514()
    );
}

#[test]
fn validity_boundaries_and_clock_are_inclusive() {
    let parsed = cert(&CertSpec::new("Leaf", 1).window(1000, 2000));
    assert_eq!(parsed.validity_status_at(999), ValidityStatus::NotYetValid);
    assert_eq!(parsed.validity_status_at(1000), ValidityStatus::Valid);
    assert_eq!(parsed.validity_status_at(2000), ValidityStatus::Valid);
    assert_eq!(parsed.validity_status_at(2001), ValidityStatus::Expired);
    assert_eq!(
        parsed.validity_status_at(1500).is_valid(),
        ValidityStatus::Valid.is_valid()
    );
    assert!(parsed.is_valid_at(&FixedClock::new(1500)));
    assert!(!parsed.is_valid_at(&FixedClock::new(999)));
}

#[test]
fn extensions_parse_all_known_forms_and_preserve_unknown() {
    let unknown = extension(&[1, 2, 3, 4], false, &encode_octet_string(&[0x05, 0x00]));
    let parsed = cert(
        &CertSpec::new("Leaf", 1)
            .ext(basic_constraints_ext(true, Some(1)))
            .ext(key_usage_ext(
                KeyUsage::DIGITAL_SIGNATURE.bits()
                    | KeyUsage::NON_REPUDIATION.bits()
                    | KeyUsage::DECIPHER_ONLY.bits(),
            ))
            .ext(san_ext())
            .ext(eku_ext(&[&OID_SERVER_AUTH, &OID_ANY_EKU]))
            .ext(unknown),
    );
    let constraints = parsed.basic_constraints().expect("bc");
    assert!(constraints.is_ca());
    assert_eq!(constraints.path_len(), Some(1));
    let usage = parsed.key_usage().expect("ku");
    assert!(usage.permits(KeyUsageBit::DigitalSignature));
    assert!(usage.permits(KeyUsageBit::NonRepudiation));
    assert!(usage.permits(KeyUsageBit::DecipherOnly));
    assert!(!usage.permits(KeyUsageBit::KeyCertSign));
    assert!(
        parsed
            .subject_alt_name()
            .expect("san")
            .registered_ids()
            .len()
            == 1
    );
    let eku = parsed.extended_key_usage().expect("eku");
    assert!(eku.allows(&EkuPurpose::ServerAuth));
    assert!(eku.allows(&EkuPurpose::DocumentSigning));
    match parsed.extensions().last().expect("unknown").value() {
        ExtensionValue::Unknown(bytes) => assert_eq!(bytes, &[0x04, 0x02, 0x05, 0x00]),
        other => panic!("expected unknown extension, got {other:?}"),
    }
    assert!(parsed.unknown_critical_extensions().is_empty());
}

#[test]
fn extension_parsing_rejects_malformed_payloads() {
    let bad_bc = extension(&OID_BASIC_CONSTRAINTS, true, &encode_octet_string(&[]));
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).ext(bad_bc).build()).map(|_| ()),
        Err(SecurityError::CertificateExtensionInvalid(
            "basicConstraints"
        ))
    );

    let bad_ku = extension(&OID_KEY_USAGE, true, &encode_octet_string(&[]));
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).ext(bad_ku).build()).map(|_| ()),
        Err(SecurityError::CertificateExtensionInvalid("keyUsage"))
    );

    let bad_san = extension(
        &OID_SUBJECT_ALT_NAME,
        false,
        &encode_octet_string(&encode_sequence(&encode_tlv(
            Tag::context(3, false).expect("tag"),
            b"x",
        ))),
    );
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).ext(bad_san).build()).map(|_| ()),
        Err(SecurityError::CertificateExtensionInvalid("subjectAltName"))
    );

    let empty_eku = extension(
        &OID_EXT_KEY_USAGE,
        false,
        &encode_octet_string(&encode_sequence(&[])),
    );
    assert_eq!(
        Certificate::parse(&CertSpec::new("Leaf", 1).ext(empty_eku).build()).map(|_| ()),
        Err(SecurityError::CertificateExtensionInvalid("extKeyUsage"))
    );
}

#[test]
fn verify_chain_accepts_direct_anchor() {
    let root = anchor("Root");
    let leaf = leaf_signed_by("Root");
    let mut store = TrustStore::new();
    store.add_anchor(root.clone());
    let report = verify_chain(
        &leaf,
        &[],
        &store,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("trusted chain");
    assert_eq!(report.depth(), 2);
    assert_eq!(report.leaf(), &leaf);
    assert_eq!(report.anchor(), &root);
    assert!(report.is_trusted());
}

#[test]
fn verify_chain_accepts_intermediate_from_untrusted_pool_and_store() {
    let root = anchor("Root");
    let intermediate = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    let leaf = leaf_signed_by("Intermediate");
    let store = TrustStore::new().with_anchor(root.clone());

    let via_pool = verify_chain(
        &leaf,
        std::slice::from_ref(&intermediate),
        &store,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("chain via untrusted pool");
    assert_eq!(via_pool.depth(), 3);
    assert_eq!(via_pool.chain()[1], intermediate);

    let store_with_intermediate = store.clone().with_intermediate(intermediate);
    let via_store = verify_chain(
        &leaf,
        &[],
        &store_with_intermediate,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("chain via pinned intermediate");
    assert_eq!(via_store.depth(), 3);
}

#[test]
fn verify_chain_rejects_unknown_issuer_and_exceeds_depth() {
    let leaf = leaf_signed_by("Root");
    let empty_store = TrustStore::new();
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &empty_store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateUntrusted)
    );

    let unrelated = anchor("Unrelated");
    let store = TrustStore::new().with_anchor(unrelated);
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateIssuerUnknown)
    );

    let root = anchor("Root");
    let shallow = store.with_anchor(root);
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &shallow,
            &use_digest_verifier(),
            &ValidationPolicy::default().with_max_depth(1),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateChainTooLong)
    );
}

#[test]
fn verify_chain_rejects_bad_or_tampered_signatures() {
    let root = anchor("Root");
    let store = TrustStore::new().with_anchor(root);

    let bad_leaf = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(false, None))
            .unsigned(),
    );
    assert_eq!(
        verify_chain(
            &bad_leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateSignatureInvalid)
    );

    let good_leaf = leaf_signed_by("Root");
    assert_eq!(
        verify_chain(
            &good_leaf,
            &[],
            &store,
            &RejectingVerifier,
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateSignatureInvalid)
    );

    let tampered = cert(
        &CertSpec::new("Leaf", 8)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(false, None))
            .signature_override(sha256(b"tampered")[..16].to_vec()),
    );
    assert_eq!(
        verify_chain(
            &tampered,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateSignatureInvalid)
    );

    // A caller that explicitly disables time checks gets a signature-only
    // decision even when the supplied clock lies outside the validity window.
    assert!(
        verify_chain(
            &good_leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().with_time_checks(false),
            &FixedClock::new(1_000_000_000),
        )
        .is_ok()
    );
}

#[test]
fn verify_chain_enforces_key_usage_and_ca_constraints() {
    let root = anchor("Root");

    let no_ca_intermediate = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    let leaf = leaf_signed_by("Intermediate");
    let store = TrustStore::new().with_anchor(root.clone());
    assert_eq!(
        verify_chain(
            &leaf,
            std::slice::from_ref(&no_ca_intermediate),
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateNotCa)
    );

    let no_sign_intermediate = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits())),
    );
    assert_eq!(
        verify_chain(
            &leaf,
            &[no_sign_intermediate],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateKeyUsageViolation("keyCertSign"))
    );

    let path_zero_root = cert(
        &CertSpec::new("Root", 100)
            .ext(basic_constraints_ext(true, Some(0)))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    let ca_intermediate = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    assert_eq!(
        verify_chain(
            &leaf,
            &[ca_intermediate],
            &TrustStore::new().with_anchor(path_zero_root),
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateChainTooLong)
    );

    let leaf_without_signing = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::KEY_ENCIPHERMENT.bits())),
    );
    assert_eq!(
        verify_chain(
            &leaf_without_signing,
            &[],
            &TrustStore::new().with_anchor(root),
            &use_digest_verifier(),
            &ValidationPolicy::default().requiring_key_usage(KeyUsageBit::DigitalSignature),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateKeyUsageViolation(
            "digitalSignature"
        ))
    );
}

#[test]
fn verify_chain_rejects_expired_and_not_yet_valid_certificates() {
    let root = anchor("Root");
    let store = TrustStore::new().with_anchor(root);
    let leaf = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .window(1_600_000_000, 1_700_000_000)
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits())),
    );
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &FixedClock::new(1_800_000_000),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateExpired)
    );
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &FixedClock::new(1_500_000_000),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateNotYetValid)
    );
    assert!(
        verify_chain(
            &leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().with_time_checks(false),
            &FixedClock::new(1_800_000_000),
        )
        .is_ok()
    );
}

#[test]
fn verify_chain_enforces_required_eku() {
    let root = anchor("Root");
    let store = TrustStore::new().with_anchor(root);
    let document_signer = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(eku_ext(&[&OID_DOCUMENT_SIGNING])),
    );
    assert!(
        verify_chain(
            &document_signer,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().requiring_eku(EkuPurpose::DocumentSigning),
            &fixed_clock(),
        )
        .is_ok()
    );
    assert_eq!(
        verify_chain(
            &document_signer,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().requiring_eku(EkuPurpose::ServerAuth),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateEkuMissing)
    );

    let any_eku = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(eku_ext(&[&OID_ANY_EKU])),
    );
    assert!(
        verify_chain(
            &any_eku,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().requiring_eku(EkuPurpose::ServerAuth),
            &fixed_clock(),
        )
        .is_ok()
    );

    let unconstrained = leaf_signed_by("Root");
    assert!(
        verify_chain(
            &unconstrained,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default().requiring_eku(EkuPurpose::ServerAuth),
            &fixed_clock(),
        )
        .is_ok()
    );
}

#[test]
fn verify_chain_rejects_unknown_critical_extensions() {
    let root = anchor("Root");
    let critical_unknown = extension(&[1, 2, 3, 4], true, &encode_octet_string(&[0x05, 0x00]));
    let leaf = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(critical_unknown),
    );
    let store = TrustStore::new().with_anchor(root);
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateUnknownCriticalExtension)
    );

    let noncritical_unknown = extension(&[1, 2, 3, 4], false, &encode_octet_string(&[0x05, 0x00]));
    let permissive = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Root")
            .ext(noncritical_unknown),
    );
    assert!(
        verify_chain(
            &permissive,
            &[],
            &store,
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .is_ok()
    );
}

#[test]
fn parse_v1_certificate_has_no_extensions() {
    let parsed = cert(&CertSpec::new("Legacy", 1).serial(3).version(0));
    assert_eq!(parsed.version(), 0);
    assert!(parsed.extensions().is_empty());
    assert!(parsed.basic_constraints().is_none());
    assert!(!parsed.is_ca());
    assert!(parsed.allows_usage(KeyUsageBit::KeyCertSign));
}

#[test]
fn digital_id_store_rejects_duplicates_and_looks_up() {
    let first_der = CertSpec::new("Alice", 11).build();
    let second_der = CertSpec::new("Bob", 12).build();
    let first_handle: Arc<dyn crate::KeyHandle> =
        Arc::new(OpaqueKeyHandle::new("windows-cng", "key-alice", false).expect("handle"));
    let second_handle: Arc<dyn crate::KeyHandle> =
        Arc::new(OpaqueKeyHandle::new("pkcs11", "key-bob", true).expect("handle"));
    let first =
        DigitalId::new("alice", &first_der, Some(first_handle.clone()), 1_000).expect("first id");
    let second =
        DigitalId::new("bob", &second_der, Some(second_handle.clone()), 1_001).expect("second id");

    let mut store = DigitalIdStore::new();
    store.insert(first.clone()).expect("insert first");
    assert_eq!(
        store.insert(first.clone()),
        Err(SecurityError::DigitalIdDuplicate)
    );
    let same_identity =
        DigitalId::new("renamed", &first_der, Some(first_handle), 1_002).expect("same identity");
    assert_eq!(
        store.insert(same_identity),
        Err(SecurityError::DigitalIdDuplicate)
    );
    store.insert(second).expect("insert second");
    assert_eq!(store.len(), 2);

    assert_eq!(store.get("alice"), Some(&first));
    assert_eq!(store.find_by_serial(11).len(), 1);
    assert_eq!(store.find_by_subject_display("CN=Alice").len(), 1);
    assert_eq!(
        store.find_by_subject(first.certificate().subject()).len(),
        1
    );
    assert!(store.find_by_serial(999).is_empty());
    assert_eq!(store.export_metadata().len(), 2);

    let removed = store.remove("alice").expect("remove");
    assert_eq!(removed.label(), "alice");
    assert_eq!(
        store.remove("alice").map(|_| ()),
        Err(SecurityError::DigitalIdNotFound)
    );
}

#[test]
fn digital_id_import_round_trip_rejects_metadata_tampering() {
    let der = CertSpec::new("Carol", 21).build();
    let handle: Arc<dyn crate::KeyHandle> =
        Arc::new(OpaqueKeyHandle::new("windows-cng", "key-carol", true).expect("handle"));
    let original = DigitalId::new("carol", &der, Some(handle.clone()), 2_000).expect("original id");
    let mut source = DigitalIdStore::new();
    source.insert(original).expect("insert");
    let metadata = &source.export_metadata()[0];

    let mut restored = DigitalIdStore::new();
    restored
        .import(metadata, &der, Some(handle.clone()))
        .expect("import");
    assert_eq!(restored.len(), 1);
    assert_eq!(
        restored.import(metadata, &der, Some(handle)),
        Err(SecurityError::DigitalIdDuplicate)
    );

    let mut bad_fingerprint = metadata.clone();
    bad_fingerprint.certificate_fingerprint_sha256 = "00".repeat(32);
    assert_eq!(
        restored.import(&bad_fingerprint, &der, None),
        Err(SecurityError::DigitalIdMetadataMismatch)
    );

    let mut bad_subject = metadata.clone();
    bad_subject.subject_display = "CN=SomeoneElse".to_owned();
    assert_eq!(
        restored.import(&bad_subject, &der, None),
        Err(SecurityError::DigitalIdMetadataMismatch)
    );

    let mut bad_key = metadata.clone();
    bad_key.key_handle_id = Some("different-handle".to_owned());
    assert_eq!(
        restored.import(&bad_key, &der, None),
        Err(SecurityError::DigitalIdMetadataMismatch)
    );
}

struct SecretBearingHandle {
    provider: &'static str,
    handle_id: &'static str,
    _secret_never_logged: &'static str,
}

impl crate::KeyHandle for SecretBearingHandle {
    fn provider(&self) -> &str {
        self.provider
    }

    fn handle_id(&self) -> &str {
        self.handle_id
    }

    fn is_hardware_backed(&self) -> bool {
        true
    }
}

impl std::fmt::Debug for SecretBearingHandle {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SecretBearingHandle")
            .field("provider", &self.provider)
            .field("handle_id", &self.handle_id)
            .field("key", &"<redacted>")
            .finish()
    }
}

#[test]
fn key_handle_is_opaque_and_secret_bytes_are_never_logged() {
    let der = CertSpec::new("Dave", 31).build();
    let handle: Arc<dyn crate::KeyHandle> = Arc::new(SecretBearingHandle {
        provider: "test-hsm",
        handle_id: "slot-7",
        _secret_never_logged: "TOP-SECRET-NEVER-LOGGED",
    });
    let id = DigitalId::new("dave", &der, Some(handle), 3_000).expect("id");
    assert_eq!(id.key_identity(), Some("test-hsm:slot-7".to_owned()));
    let metadata = id.metadata();
    assert_eq!(metadata.key_provider.as_deref(), Some("test-hsm"));
    assert_eq!(metadata.key_handle_id.as_deref(), Some("slot-7"));
    assert_eq!(metadata.hardware_backed, Some(true));

    let mut store = DigitalIdStore::new();
    store.insert(id.clone()).expect("insert");
    for rendered in [
        format!("{id:?}"),
        format!("{store:?}"),
        format!("{metadata:?}"),
        format!("{:?}", metadata.key_provider),
    ] {
        assert!(!rendered.contains("TOP-SECRET-NEVER-LOGGED"));
    }
    assert!(OpaqueKeyHandle::new("", "id", false).is_err());
    assert!(OpaqueKeyHandle::new("provider", "", false).is_err());
    assert!(OpaqueKeyHandle::new("provider\n", "id", false).is_err());
}

#[test]
fn signature_algorithm_maps_oids_and_digests() {
    assert_eq!(
        SignatureAlgorithm::from_oid(&crate::x509::OID_SHA256_WITH_RSA).digest_algorithm(),
        Some(crate::DigestAlgorithm::Sha256)
    );
    assert_eq!(
        SignatureAlgorithm::from_oid(&crate::x509::OID_ECDSA_SHA384).digest_algorithm(),
        Some(crate::DigestAlgorithm::Sha384)
    );
    let unknown = SignatureAlgorithm::from_oid(&[1, 2, 3, 4]);
    assert_eq!(unknown, SignatureAlgorithm::Other(vec![1, 2, 3, 4]));
    assert_eq!(unknown.as_str(), "1.2.3.4");
    assert_eq!(
        verify_chain(
            &cert(
                &CertSpec::new("Leaf", 1)
                    .issuer_cn("Root")
                    .sig_oid(vec![1, 2, 3, 4])
            ),
            &[],
            &TrustStore::new().with_anchor(anchor("Root")),
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::UnsupportedSignatureAlgorithm(
            "1.2.3.4".to_owned()
        ))
    );
}

#[test]
fn der_reader_still_guards_x509_shapes() {
    let integer = encode_integer(7);
    let tlv = DerReader::new(&integer).read_tlv().expect("integer");
    assert_eq!(tlv.as_integer_u64(), Ok(7));
    assert_eq!(
        tlv.as_bit_string().map(|_| ()),
        Err(SecurityError::DerUnexpectedTag {
            expected: Tag::BIT_STRING.byte(),
            found: Tag::INTEGER.byte(),
        })
    );
    let boolean_true = encode_tlv(Tag::BOOLEAN, &[0xff]);
    let tlv = DerReader::new(&boolean_true).read_tlv().expect("boolean");
    assert_eq!(tlv.as_bool(), Ok(true));
    let noncanonical = encode_tlv(Tag::BOOLEAN, &[0x01]);
    let tlv = DerReader::new(&noncanonical).read_tlv().expect("boolean");
    assert_eq!(tlv.as_bool(), Err(SecurityError::DerInvalid));
}

#[test]
fn verify_chain_skips_decoy_candidate_and_uses_valid_issuer() {
    let root = anchor("Root");
    // Same subject as the real root but not a CA: a locally-invalid decoy that
    // must not abort path building when it is listed first.
    let decoy = cert(
        &CertSpec::new("Root", 900)
            .ext(basic_constraints_ext(false, None))
            .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits())),
    );
    let leaf = leaf_signed_by("Root");
    let store = TrustStore::new().with_anchor(root.clone());
    let report = verify_chain(
        &leaf,
        &[decoy],
        &store,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("valid issuer selected after decoy");
    assert_eq!(report.depth(), 2);
    assert_eq!(report.anchor(), &root);
}

#[test]
fn verify_chain_accepts_cross_signed_path() {
    let trusted_root = anchor("RootA");
    let untrusted_root = anchor("RootB");
    let cross_signed_by_a = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("RootA")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    let cross_signed_by_b = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("RootB")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    let leaf = leaf_signed_by("Intermediate");
    let store = TrustStore::new().with_anchor(trusted_root.clone());
    // The dead-end branch is listed first so a successful validation requires
    // backtracking to the cross-signed certificate issued by the trusted root.
    let pool = [cross_signed_by_b, untrusted_root, cross_signed_by_a.clone()];
    let report = verify_chain(
        &leaf,
        &pool,
        &store,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("cross-signed path");
    assert_eq!(report.depth(), 3);
    assert_eq!(report.chain()[1], cross_signed_by_a);
    assert_eq!(report.anchor(), &trusted_root);
}

#[test]
fn verify_chain_intersects_eku_constraints_from_cas() {
    let root = anchor("Root");
    let restricted_ca = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits()))
            .ext(eku_ext(&[&OID_TIME_STAMPING])),
    );
    let document_signer = cert(
        &CertSpec::new("Leaf", 7)
            .issuer_cn("Intermediate")
            .ext(eku_ext(&[&crate::x509::extension::OID_DOCUMENT_SIGNING])),
    );
    let store = TrustStore::new().with_anchor(root);
    let policy = ValidationPolicy::default().requiring_eku(EkuPurpose::DocumentSigning);
    assert_eq!(
        verify_chain(
            &document_signer,
            std::slice::from_ref(&restricted_ca),
            &store,
            &use_digest_verifier(),
            &policy,
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::CertificateEkuMissing)
    );

    let unconstrained_ca = cert(
        &CertSpec::new("Intermediate", 50)
            .issuer_cn("Root")
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    assert!(
        verify_chain(
            &document_signer,
            std::slice::from_ref(&unconstrained_ca),
            &store,
            &use_digest_verifier(),
            &policy,
            &fixed_clock(),
        )
        .is_ok()
    );
}

#[test]
fn verify_chain_rejects_issuer_with_unsupported_signature_algorithm() {
    let leaf = leaf_signed_by("Root");
    let unsupported_root = cert(
        &CertSpec::new("Root", 100)
            .sig_oid(vec![1, 2, 3, 4])
            .ext(basic_constraints_ext(true, None))
            .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits())),
    );
    assert_eq!(
        verify_chain(
            &leaf,
            &[],
            &TrustStore::new().with_anchor(unsupported_root.clone()),
            &use_digest_verifier(),
            &ValidationPolicy::default(),
            &fixed_clock(),
        )
        .map(|_| ()),
        Err(SecurityError::UnsupportedSignatureAlgorithm(
            "1.2.3.4".to_owned()
        ))
    );

    // An unsupported-algorithm decoy must be skipped in favour of a valid root.
    let good_root = anchor("Root");
    let store = TrustStore::new().with_anchor(good_root.clone());
    let report = verify_chain(
        &leaf,
        &[unsupported_root],
        &store,
        &use_digest_verifier(),
        &ValidationPolicy::default(),
        &fixed_clock(),
    )
    .expect("valid anchor after unsupported-algorithm decoy");
    assert_eq!(report.anchor(), &good_root);
}

#[test]
fn parse_certificate_rejects_duplicate_optional_tbs_fields() {
    let issuer_unique = encode_tlv(Tag::context(1, false).expect("tag"), &[0, 0]);
    assert_eq!(
        Certificate::parse(
            &CertSpec::new("Leaf", 1)
                .raw_tbs_field(issuer_unique.clone())
                .raw_tbs_field(issuer_unique)
                .build()
        )
        .map(|_| ()),
        Err(SecurityError::CertificateDuplicateField("issuerUniqueID"))
    );

    let subject_unique = encode_tlv(Tag::context(2, false).expect("tag"), &[0, 0]);
    assert_eq!(
        Certificate::parse(
            &CertSpec::new("Leaf", 1)
                .raw_tbs_field(subject_unique.clone())
                .raw_tbs_field(subject_unique)
                .build()
        )
        .map(|_| ()),
        Err(SecurityError::CertificateDuplicateField("subjectUniqueID"))
    );

    let extensions = encode_context(3, true, &encode_sequence(&[])).expect("ctx3");
    assert_eq!(
        Certificate::parse(
            &CertSpec::new("Leaf", 1)
                .raw_tbs_field(extensions.clone())
                .raw_tbs_field(extensions)
                .build()
        )
        .map(|_| ()),
        Err(SecurityError::CertificateDuplicateField("extensions"))
    );
}

#[test]
fn parse_certificate_rejects_duplicate_extension_oids() {
    let duplicated = CertSpec::new("Leaf", 1)
        .ext(key_usage_ext(KeyUsage::DIGITAL_SIGNATURE.bits()))
        .ext(key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits()));
    assert_eq!(
        Certificate::parse(&duplicated.build()).map(|_| ()),
        Err(SecurityError::CertificateDuplicateExtension)
    );
}

#[test]
fn name_display_hex_escapes_utf8_bytes() {
    // RFC 4514 emits the UTF-8 octets of a control character, not its scalar.
    let parsed = parsed_name(&name(&[(&OID_CN, "A\u{0085}B")]));
    assert_eq!(parsed.display_rfc4514(), r"CN=A\C2\85B");
}
