//! `pdfxi timestamp`: mint and validate RFC 3161 timestamp tokens.
//!
//! [`pdfxi_security::timestamp`] already implements the token structure, the
//! CMS `SignedData` wrapper and the full validation pipeline, but it has no
//! host: callers must supply a [`SignatureProvider`] and all trust material.
//! This module is that host. `timestamp sign` mints a token locally from a
//! caller-supplied RSA identity (an offline TSA, not a network request) and
//! `timestamp verify` validates a real token against caller-supplied
//! certificates, content, clock and nonce.
//!
//! No network transport, wall clock or hidden root is used. The validation
//! clock defaults to the system clock for the future-skew check and can be
//! pinned with `--at` for reproducible results.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use pdfxi_security::der::{DerReader, Tag};
use pdfxi_security::{
    Certificate, DigestAlgorithm, FixedClock, RsaPkcs1Verifier, SecurityError, SignatureProvider,
    SignerIdentifier, TimestampPolicy, TimestampStatus, TimestampTokenParams,
    TimestampValidationContext, TrustStore, ValidationPolicy, build_timestamp_token,
    certificates_from_pem, validate_timestamp_token,
};
use rsa::RsaPublicKey;
use rsa::pkcs1::{DecodeRsaPrivateKey, DecodeRsaPublicKey};
use rsa::pkcs1v15::{Signature as RsaSignature, SigningKey, VerifyingKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::signature::{SignatureEncoding, Signer, Verifier};
use serde::Serialize;

/// A verification-only or signing CMS provider backed by a real RSA key.
///
/// The certificate, signer identifier and public key always come from the
/// supplied certificate, so a token minted here and a token verified here use
/// exactly the same identity bytes.
struct RsaCmsProvider {
    /// Signing key; absent for verification-only use.
    private: Option<rsa::RsaPrivateKey>,
    /// Public key parsed from the certificate.
    public: RsaPublicKey,
    /// Full DER certificate.
    certificate_der: Vec<u8>,
    /// Issuer-and-serial signer identifier.
    signer: SignerIdentifier,
    /// Display name from the certificate subject.
    name: String,
}

impl RsaCmsProvider {
    /// Builds a provider from a certificate and an optional private key.
    fn new(certificate_der: Vec<u8>, private: Option<rsa::RsaPrivateKey>) -> anyhow::Result<Self> {
        let certificate = Certificate::parse(&certificate_der)
            .map_err(|error| anyhow::anyhow!("parse certificate: {error}"))?;
        let key_bits = certificate.subject_public_key_info().key_bits();
        let public = RsaPublicKey::from_pkcs1_der(key_bits)
            .map_err(|error| anyhow::anyhow!("certificate public key is not RSA: {error}"))?;
        let (issuer_der, serial) = issuer_and_serial(&certificate)?;
        let signer = SignerIdentifier::new(issuer_der, serial)
            .map_err(|error| anyhow::anyhow!("signer identifier: {error}"))?;
        Ok(Self {
            private,
            public,
            certificate_der,
            signer,
            name: certificate.subject().display_rfc4514(),
        })
    }

    /// Signs `tbs` with the private key, or returns a typed error.
    fn sign_with(&self, algorithm: DigestAlgorithm, tbs: &[u8]) -> Result<Vec<u8>, SecurityError> {
        let key = self.private.as_ref().ok_or_else(|| {
            SecurityError::SigningKeyUnsupported("this provider has no private key".to_owned())
        })?;
        let signature = match algorithm {
            DigestAlgorithm::Sha256 => SigningKey::<sha2::Sha256>::new(key.clone())
                .sign(tbs)
                .to_vec(),
            DigestAlgorithm::Sha384 => SigningKey::<sha2::Sha384>::new(key.clone())
                .sign(tbs)
                .to_vec(),
            DigestAlgorithm::Sha512 => SigningKey::<sha2::Sha512>::new(key.clone())
                .sign(tbs)
                .to_vec(),
            DigestAlgorithm::Sha1 => {
                return Err(SecurityError::UnsupportedDigestAlgorithm("sha1".to_owned()));
            }
        };
        Ok(signature)
    }

    /// Verifies `signature` over `tbs` with the certificate public key.
    fn verify_with(
        &self,
        algorithm: DigestAlgorithm,
        tbs: &[u8],
        signature: &[u8],
    ) -> Result<bool, SecurityError> {
        let Ok(signature) = RsaSignature::try_from(signature) else {
            return Ok(false);
        };
        let valid = match algorithm {
            DigestAlgorithm::Sha256 => VerifyingKey::<sha2::Sha256>::new(self.public.clone())
                .verify(tbs, &signature)
                .is_ok(),
            DigestAlgorithm::Sha384 => VerifyingKey::<sha2::Sha384>::new(self.public.clone())
                .verify(tbs, &signature)
                .is_ok(),
            DigestAlgorithm::Sha512 => VerifyingKey::<sha2::Sha512>::new(self.public.clone())
                .verify(tbs, &signature)
                .is_ok(),
            DigestAlgorithm::Sha1 => {
                return Err(SecurityError::UnsupportedDigestAlgorithm("sha1".to_owned()));
            }
        };
        Ok(valid)
    }
}

impl SignatureProvider for RsaCmsProvider {
    fn sign(&self, algorithm: DigestAlgorithm, tbs: &[u8]) -> pdfxi_security::Result<Vec<u8>> {
        self.sign_with(algorithm, tbs)
    }

    fn verify(
        &self,
        algorithm: DigestAlgorithm,
        tbs: &[u8],
        signature: &[u8],
    ) -> pdfxi_security::Result<bool> {
        self.verify_with(algorithm, tbs, signature)
    }

    fn signer_name(&self) -> &str {
        &self.name
    }

    fn certificate_der(&self) -> &[u8] {
        &self.certificate_der
    }

    fn signer_identifier(&self) -> &SignerIdentifier {
        &self.signer
    }
}

/// Extracts the DER issuer name and serial number from a certificate's TBS.
fn issuer_and_serial(certificate: &Certificate) -> anyhow::Result<(Vec<u8>, u64)> {
    let mut reader = DerReader::new(certificate.tbs_der());
    let mut tbs = reader.read_sequence()?;
    if tbs.peek_tag() == Tag::context(0, true) {
        let version = Tag::context(0, true).context("context tag 0 is always valid")?;
        tbs.read_tagged(version)?;
    }
    let serial = tbs.read_tlv()?.as_integer_u64()?;
    let _signature_algorithm = tbs.read_tlv()?;
    let issuer = tbs.read_tlv()?;
    if issuer.tag() != Tag::SEQUENCE {
        bail!("certificate issuer is not a DER SEQUENCE");
    }
    Ok((issuer.encoded().to_vec(), serial))
}

/// Loads every certificate in a PEM bundle or one DER certificate.
fn load_certificates(path: &Path) -> anyhow::Result<Vec<Certificate>> {
    let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
    let is_pem = bytes
        .windows(b"-----BEGIN".len())
        .any(|window| window == b"-----BEGIN");
    if is_pem {
        certificates_from_pem(&bytes)
            .map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))
    } else {
        let certificate = Certificate::parse(&bytes)
            .map_err(|error| anyhow::anyhow!("parse {}: {error}", path.display()))?;
        Ok(vec![certificate])
    }
}

/// Parses a dotted OID into arcs.
fn parse_oid(text: &str) -> anyhow::Result<Vec<u32>> {
    let mut arcs = Vec::new();
    for part in text.trim().split('.') {
        if part.is_empty() {
            bail!("policy OID {text:?} has an empty arc");
        }
        let arc: u32 = part
            .parse()
            .with_context(|| format!("policy OID arc {part:?} is not a number"))?;
        arcs.push(arc);
    }
    if arcs.len() < 2 {
        bail!("policy OID {text:?} needs at least two arcs");
    }
    Ok(arcs)
}

/// Parses a digest name.
fn parse_digest(text: &str) -> anyhow::Result<DigestAlgorithm> {
    match text.trim().to_ascii_lowercase().as_str() {
        "sha256" => Ok(DigestAlgorithm::Sha256),
        "sha384" => Ok(DigestAlgorithm::Sha384),
        "sha512" => Ok(DigestAlgorithm::Sha512),
        other => bail!("unsupported digest {other:?}; expected sha256, sha384 or sha512"),
    }
}

/// Current Unix time.
fn now_unix() -> anyhow::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    Ok(duration.as_secs())
}

/// JSON shape of `timestamp sign`.
#[derive(Debug, Serialize)]
struct SignReport {
    /// Written token path.
    out: String,
    /// Token size in bytes.
    bytes: usize,
    /// Content file the imprint covers.
    content: String,
    /// Digest algorithm id.
    digest: &'static str,
    /// TSA policy OID.
    policy: String,
    /// TSA serial number.
    serial: u64,
    /// `genTime` in Unix seconds.
    gen_time: u64,
    /// Optional nonce.
    nonce: Option<u64>,
    /// Signer subject.
    signer: String,
}

/// JSON shape of `timestamp verify`.
#[derive(Debug, Serialize)]
struct VerifyReport {
    /// Token file.
    token: String,
    /// Content file.
    content: String,
    /// Exact validation outcome.
    status: &'static str,
    /// Whether every check passed.
    valid: bool,
    /// Conservative signature-status mapping.
    signature_status: &'static str,
    /// Token serial number.
    serial: u64,
    /// `genTime` in Unix seconds.
    gen_time: u64,
    /// Policy OID.
    policy: String,
    /// Token nonce, if any.
    nonce: Option<u64>,
    /// Imprint digest algorithm id.
    digest: &'static str,
    /// Imprint bytes as lowercase hex.
    imprint: String,
    /// Certificate subject reported by the verified CMS signer.
    signer: String,
    /// Validation clock used for the future-skew check.
    checked_at: u64,
}

/// Loads a private key from PKCS#8 or PKCS#1 PEM.
fn load_private_key(path: &Path) -> anyhow::Result<rsa::RsaPrivateKey> {
    let pem = std::fs::read_to_string(path)
        .with_context(|| format!("read private key {}", path.display()))?;
    rsa::RsaPrivateKey::from_pkcs8_pem(&pem)
        .or_else(|_| rsa::RsaPrivateKey::from_pkcs1_pem(&pem))
        .map_err(|_| {
            anyhow::anyhow!(
                "{} is not an unencrypted PKCS#8 or PKCS#1 RSA private key",
                path.display()
            )
        })
}

/// `pdfxi timestamp sign`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn sign(
    content: &Path,
    out: &Path,
    key: &Path,
    cert: &Path,
    digest: &str,
    policy: &str,
    serial: u64,
    time: Option<u64>,
    nonce: Option<u64>,
    ordering: bool,
    json: bool,
) -> anyhow::Result<()> {
    let digest = parse_digest(digest)?;
    let policy = parse_oid(policy)?;
    let data = std::fs::read(content).with_context(|| format!("read {}", content.display()))?;
    let certificates = load_certificates(cert)?;
    let certificate = certificates
        .first()
        .context("the certificate file contains no certificate")?;
    let certificate_der = certificate.der().to_vec();
    let private = load_private_key(key)?;
    let provider = RsaCmsProvider::new(certificate_der, Some(private))?;
    let gen_time = time.unwrap_or(now_unix()?);
    let params = TimestampTokenParams {
        data: &data,
        digest_algorithm: digest,
        policy: policy.clone(),
        serial_number: serial,
        gen_time,
        accuracy: None,
        ordering,
        nonce,
        tsa: None,
    };
    let token = build_timestamp_token(&params, &provider)
        .map_err(|error| anyhow::anyhow!("build timestamp token: {error}"))?;
    std::fs::write(out, &token).with_context(|| format!("write {}", out.display()))?;
    let report = SignReport {
        out: out.display().to_string(),
        bytes: token.len(),
        content: content.display().to_string(),
        digest: digest.as_id(),
        policy: policy
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
        serial,
        gen_time,
        nonce,
        signer: provider.name.clone(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("out: {}", report.out);
        println!("bytes: {}", report.bytes);
        println!("digest: {}", report.digest);
        println!("policy: {}", report.policy);
        println!("serial: {}", report.serial);
        println!("gen-time: {}", report.gen_time);
        if let Some(nonce) = report.nonce {
            println!("nonce: {nonce}");
        }
        println!("signer: {}", report.signer);
    }
    Ok(())
}

/// `pdfxi timestamp verify`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify(
    token: &Path,
    content: &Path,
    cert: &Path,
    anchors: &[PathBuf],
    chain: &[PathBuf],
    nonce: Option<u64>,
    at: Option<u64>,
    allow_any_eku: bool,
    json: bool,
) -> anyhow::Result<()> {
    let token_bytes = std::fs::read(token).with_context(|| format!("read {}", token.display()))?;
    let data = std::fs::read(content).with_context(|| format!("read {}", content.display()))?;
    let certificates = load_certificates(cert)?;
    let leaf = certificates
        .first()
        .context("the certificate file contains no certificate")?;
    let provider = RsaCmsProvider::new(leaf.der().to_vec(), None)?;

    let mut store = TrustStore::new();
    if anchors.is_empty() {
        store.add_anchor(leaf.clone());
    } else {
        for path in anchors {
            for certificate in load_certificates(path)? {
                store.add_anchor(certificate);
            }
        }
    }
    let mut intermediates: Vec<Certificate> = certificates.iter().skip(1).cloned().collect();
    for path in chain {
        intermediates.extend(load_certificates(path)?);
    }
    let chain_policy = ValidationPolicy::default();
    let policy = TimestampPolicy {
        expected_nonce: nonce,
        required_tsa_eku: if allow_any_eku {
            None
        } else {
            Some(pdfxi_security::EkuPurpose::TimeStamping)
        },
        ..TimestampPolicy::default()
    };
    let checked_at = match at {
        Some(value) => value,
        None => now_unix()?,
    };
    let clock = FixedClock::new(checked_at);
    let verifier = RsaPkcs1Verifier;
    let context = TimestampValidationContext {
        data: &data,
        provider: &provider,
        intermediates: &intermediates,
        trust_store: &store,
        chain_verifier: &verifier,
        chain_policy: &chain_policy,
        now_clock: &clock,
        policy: &policy,
    };
    let validation = validate_timestamp_token(&token_bytes, &context)
        .map_err(|error| anyhow::anyhow!("validate {}: {error}", token.display()))?;
    let info = validation.info();
    let report = VerifyReport {
        token: token.display().to_string(),
        content: content.display().to_string(),
        status: validation.status().label(),
        valid: validation.is_valid(),
        signature_status: validation.to_signature_status().label(),
        serial: info.serial_number,
        gen_time: info.gen_time,
        policy: info
            .policy
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join("."),
        nonce: info.nonce,
        digest: info.message_imprint.digest_algorithm.as_id(),
        imprint: hex(&info.message_imprint.hashed_message),
        signer: validation.to_signature_info().signer().to_owned(),
        checked_at,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("token: {}", report.token);
        println!("status: {}", report.status);
        println!("valid: {}", report.valid);
        println!("signature-status: {}", report.signature_status);
        println!("serial: {}", report.serial);
        println!("gen-time: {}", report.gen_time);
        println!("policy: {}", report.policy);
        if let Some(nonce) = report.nonce {
            println!("nonce: {nonce}");
        }
        println!("digest: {}", report.digest);
        println!("imprint: {}", report.imprint);
        println!("signer: {}", report.signer);
        println!("checked-at: {}", report.checked_at);
    }
    if validation.status() != TimestampStatus::Valid {
        bail!(
            "timestamp token is not valid: {}",
            validation.status().label()
        );
    }
    Ok(())
}

/// Lowercase hex.
fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}
