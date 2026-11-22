//! Explicit signature trust anchors loaded from a PEM or DER bundle (T-0267).
//!
//! This module is the trust-anchor half of the signature-trust work deferred
//! when T-0249 was narrowed to the PKCS#12 identity path. It turns a caller-
//! supplied bundle of root certificates into a [`TrustStore`] and evaluates a
//! signer chain with the existing [`crate::x509::verify_chain`] path builder.
//!
//! # Trust model
//!
//! - Anchors come only from an explicit PEM or DER bundle. There is no built-in
//!   root set, no operating-system default, and no network fetch.
//! - A chain is trusted only when it terminates at a certificate that is
//!   byte-for-byte one of the configured anchors. A cryptographically valid
//!   signature under an unknown issuer is *not* trust.
//! - Validity windows, CA basic constraints, `keyCertSign` usage, path length,
//!   unknown critical extensions, and each certificate signature are checked by
//!   [`verify_chain`]. A failure is a typed decision reason, never a silent
//!   success.
//! - The clock is caller-supplied, so evaluation is deterministic and never
//!   reads the host clock.
//!
//! # Three-valued outcome
//!
//! [`TrustAnchorBundle::certificate_chain_trusted`] returns:
//!
//! - `Ok(Some(true))` — a path reached a configured anchor;
//! - `Ok(Some(false))` — anchors are configured and the chain did not reach one;
//! - `Ok(None)` — the bundle is empty, so trust was not evaluated.
//!
//! `Ok(None)` is deliberately distinct from `Ok(Some(false))`: a caller that
//! has not configured anchors must display trust as *unverified*, not as
//! untrusted. [`TrustAnchorBundle::evaluate`] additionally carries the typed
//! [`TrustFailureReason`] for a negative decision.
//!
//! # Scope
//!
//! This round covers anchors and path building only. RFC 3161 timestamp
//! validation and CRL/OCSP revocation are separate work and are not claimed
//! here.

use std::path::Path;

use der::{Decode, Encode};

use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;
use crate::x509::verify::{CertificateVerifier, TrustStore, ValidationPolicy, verify_chain};
use crate::x509::{Clock, FixedClock};

use crate::trust_manager::TrustFailureReason;

/// A deterministic, non-secret trust evaluation result.
///
/// The report never contains private keys. Certificate `Debug` implementations
/// redact key material, and this type only exposes parsed certificates plus a
/// typed decision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainTrustReport {
    trusted: Option<bool>,
    reason: Option<TrustFailureReason>,
    chain: Vec<Certificate>,
    evaluated_at: u64,
}

impl ChainTrustReport {
    /// Return `Some(true)` for a chain that reached an anchor, `Some(false)`
    /// for a chain that did not, or `None` when no anchors are configured.
    pub const fn trusted(&self) -> Option<bool> {
        self.trusted
    }

    /// Return the typed failure reason for a negative decision, if any.
    ///
    /// A trusted report and an unevaluated (`None`) report carry no reason.
    pub const fn reason(&self) -> Option<TrustFailureReason> {
        self.reason
    }

    /// Return the validated path in leaf-to-anchor order.
    ///
    /// The path is empty when the bundle is empty or validation failed.
    pub fn chain(&self) -> &[Certificate] {
        &self.chain
    }

    /// Return the terminating trust anchor when the chain was trusted.
    pub fn anchor(&self) -> Option<&Certificate> {
        if self.trusted == Some(true) {
            self.chain.last()
        } else {
            None
        }
    }

    /// Return the caller-supplied evaluation time as Unix seconds.
    pub const fn evaluated_at(&self) -> u64 {
        self.evaluated_at
    }

    /// Return `true` only when the chain reached a configured anchor.
    pub const fn is_trusted(&self) -> bool {
        matches!(self.trusted, Some(true))
    }
}

/// A caller-supplied set of signature trust anchors.
///
/// Construct one from PEM bytes, one DER certificate, concatenated DER
/// certificates, or a file, then evaluate signer chains against it. The bundle
/// is immutable after construction; callers that need to change anchors build a
/// new bundle so an active decision never observes a mutated store.
#[derive(Debug, Clone, Default)]
pub struct TrustAnchorBundle {
    store: TrustStore,
}

impl TrustAnchorBundle {
    /// Create an empty bundle. Evaluation returns `None` until an anchor is
    /// added.
    pub fn empty() -> Self {
        Self::default()
    }

    /// Load every `CERTIFICATE` block in a PEM bundle.
    ///
    /// A bundle with no certificate block, a non-PEM input, or a malformed
    /// block returns [`SecurityError::TrustAnchorInvalid`]. The PEM text is
    /// parsed by `x509-cert`; each certificate is then re-parsed by this
    /// crate's strict X.509 parser so path building uses one representation.
    pub fn from_pem(pem: &[u8]) -> Result<Self> {
        let certificates = decode_pem_chain(pem)?;
        let mut bundle = Self::empty();
        for certificate in certificates {
            let der = certificate
                .to_der()
                .map_err(|_| SecurityError::TrustAnchorInvalid("PEM"))?;
            bundle.add_der(&der)?;
        }
        Ok(bundle)
    }

    /// Load one DER-encoded trust-anchor certificate.
    pub fn from_der(der: &[u8]) -> Result<Self> {
        let mut bundle = Self::empty();
        bundle.add_der(der)?;
        Ok(bundle)
    }

    /// Load a concatenation of DER-encoded trust-anchor certificates.
    ///
    /// This is the DER counterpart of a PEM bundle. Trailing or truncated bytes
    /// are rejected with the underlying DER or certificate error rather than
    /// being ignored.
    pub fn from_der_bundle(der: &[u8]) -> Result<Self> {
        let mut bundle = Self::empty();
        let mut offset = 0_usize;
        while offset < der.len() {
            let mut reader = crate::der::DerReader::new(&der[offset..]);
            let tlv = reader.read_tlv()?;
            let encoded = tlv.encoded();
            bundle.add_der(encoded)?;
            offset += encoded.len();
        }
        if bundle.is_empty() {
            return Err(SecurityError::TrustAnchorInvalid("empty"));
        }
        Ok(bundle)
    }

    /// Load a PEM bundle from a file.
    ///
    /// File read failures map to [`SecurityError::Configuration`] without
    /// embedding the path in the error value.
    pub fn from_pem_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = read_file(path.as_ref())?;
        Self::from_pem(&bytes)
    }

    /// Load one DER certificate from a file.
    pub fn from_der_file(path: impl AsRef<Path>) -> Result<Self> {
        let bytes = read_file(path.as_ref())?;
        Self::from_der(&bytes)
    }

    /// Add one DER-encoded anchor to the bundle.
    pub fn add_der(&mut self, der: &[u8]) -> Result<()> {
        let certificate = Certificate::parse(der)?;
        self.store.add_anchor(certificate);
        Ok(())
    }

    /// Add every `CERTIFICATE` block in a PEM input to the bundle.
    pub fn add_pem(&mut self, pem: &[u8]) -> Result<()> {
        let certificates = decode_pem_chain(pem)?;
        for certificate in certificates {
            let der = certificate
                .to_der()
                .map_err(|_| SecurityError::TrustAnchorInvalid("PEM"))?;
            self.add_der(&der)?;
        }
        Ok(())
    }

    /// Return the configured anchors in insertion order.
    pub fn anchors(&self) -> &[Certificate] {
        self.store.anchors()
    }

    /// Return the underlying store.
    pub const fn store(&self) -> &TrustStore {
        &self.store
    }

    /// Return the number of configured anchors.
    pub fn len(&self) -> usize {
        self.store.anchors().len()
    }

    /// Return `true` when no anchor is configured.
    pub fn is_empty(&self) -> bool {
        self.store.anchors().is_empty()
    }

    /// Evaluate `leaf` and its `intermediates` against the configured anchors.
    ///
    /// The default [`ValidationPolicy`] checks validity, CA constraints,
    /// `keyCertSign`, path length, unknown critical extensions, and every
    /// signature. An empty bundle returns a report with `trusted() == None`;
    /// every other failure returns `trusted() == Some(false)` with a typed
    /// [`TrustFailureReason`]. Structural input errors are returned as
    /// [`SecurityError`] before any decision is made.
    pub fn evaluate(
        &self,
        leaf: &Certificate,
        intermediates: &[Certificate],
        verifier: &dyn CertificateVerifier,
        now: u64,
    ) -> Result<ChainTrustReport> {
        self.evaluate_with_policy(
            leaf,
            intermediates,
            verifier,
            now,
            &ValidationPolicy::default(),
        )
    }

    /// Evaluate with an explicit validation policy.
    ///
    /// The same three-valued outcome rules apply. A policy that disables time
    /// checks is honoured; an empty bundle still returns `None`.
    pub fn evaluate_with_policy(
        &self,
        leaf: &Certificate,
        intermediates: &[Certificate],
        verifier: &dyn CertificateVerifier,
        now: u64,
        policy: &ValidationPolicy,
    ) -> Result<ChainTrustReport> {
        if self.is_empty() {
            return Ok(ChainTrustReport {
                trusted: None,
                reason: None,
                chain: Vec::new(),
                evaluated_at: now,
            });
        }
        let clock = FixedClock::new(now);
        match verify_chain(
            leaf,
            intermediates,
            &self.store,
            verifier,
            policy,
            &clock as &dyn Clock,
        ) {
            Ok(report) => Ok(ChainTrustReport {
                trusted: Some(true),
                reason: None,
                chain: report.chain().to_vec(),
                evaluated_at: now,
            }),
            Err(error) => Ok(ChainTrustReport {
                trusted: Some(false),
                reason: Some(TrustFailureReason::from_security_error(&error)),
                chain: Vec::new(),
                evaluated_at: now,
            }),
        }
    }

    /// Three-valued trust decision for one signer chain.
    ///
    /// Returns `Some(true)` only when the chain terminates at a configured
    /// anchor, `Some(false)` when anchors are configured but no path reaches
    /// one, and `None` when the bundle is empty. Use [`Self::evaluate`] when the
    /// typed failure reason is needed.
    pub fn certificate_chain_trusted(
        &self,
        leaf: &Certificate,
        intermediates: &[Certificate],
        verifier: &dyn CertificateVerifier,
        now: u64,
    ) -> Result<Option<bool>> {
        Ok(self.evaluate(leaf, intermediates, verifier, now)?.trusted())
    }
}

/// Parse every `CERTIFICATE` block in a PEM input into this crate's
/// [`Certificate`] type.
///
/// This is the caller-facing counterpart of [`TrustAnchorBundle::from_pem`]:
/// the same tolerant block scanner and the same strict re-parse, but it returns
/// the certificates so a host can build a leaf/intermediate list or a
/// [`TrustStore`] from user-supplied files.
///
/// # Errors
///
/// [`SecurityError::TrustAnchorInvalid`] for an empty, block-free or malformed
/// PEM input.
pub fn certificates_from_pem(pem: &[u8]) -> Result<Vec<Certificate>> {
    let certificates = decode_pem_chain(pem)?;
    certificates
        .into_iter()
        .map(|certificate| {
            let der = certificate
                .to_der()
                .map_err(|_| SecurityError::TrustAnchorInvalid("PEM"))?;
            Certificate::parse(&der)
        })
        .collect()
}

/// Decode every `CERTIFICATE` block in a PEM input.
///
/// The block scanner accepts CRLF or LF line endings, a UTF-8 BOM, arbitrary
/// base64 line lengths, and surrounding whitespace. It intentionally does not
/// use the dependency's PEM chain helper: Windows tooling writes 76-column
/// bodies, and strict RFC 7468 decoders reject line lengths other than 64.
/// Empty and block-free inputs return a typed
/// [`SecurityError::TrustAnchorInvalid`].
fn decode_pem_chain(pem: &[u8]) -> Result<Vec<x509_cert::Certificate>> {
    let input = pem.strip_prefix(&[0xEF, 0xBB, 0xBF]).unwrap_or(pem);
    let mut certificates = Vec::new();
    let mut body: Option<Vec<u8>> = None;
    for raw in input.split(|byte| *byte == b'\n') {
        let line = trim_ascii(raw);
        if line == b"-----BEGIN CERTIFICATE-----" {
            if body.is_some() {
                return Err(SecurityError::TrustAnchorInvalid("PEM"));
            }
            body = Some(Vec::new());
        } else if line == b"-----END CERTIFICATE-----" {
            let Some(encoded) = body.take() else {
                return Err(SecurityError::TrustAnchorInvalid("PEM"));
            };
            let der = decode_base64(&encoded).ok_or(SecurityError::TrustAnchorInvalid("PEM"))?;
            let certificate = x509_cert::Certificate::from_der(&der)
                .map_err(|_| SecurityError::TrustAnchorInvalid("PEM"))?;
            certificates.push(certificate);
        } else if let Some(encoded) = body.as_mut() {
            encoded.extend_from_slice(line);
        }
    }
    if body.is_some() {
        return Err(SecurityError::TrustAnchorInvalid("PEM"));
    }
    if certificates.is_empty() {
        return Err(SecurityError::TrustAnchorInvalid("empty"));
    }
    Ok(certificates)
}

fn trim_ascii(mut line: &[u8]) -> &[u8] {
    while line.first().is_some_and(u8::is_ascii_whitespace) {
        line = &line[1..];
    }
    while line.last().is_some_and(u8::is_ascii_whitespace) {
        line = &line[..line.len() - 1];
    }
    line
}

/// Strict standard-alphabet base64 decoder used for PEM bodies.
fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    let cleaned: Vec<u8> = input
        .iter()
        .copied()
        .filter(|byte| !byte.is_ascii_whitespace())
        .collect();
    if cleaned.is_empty() || !cleaned.len().is_multiple_of(4) {
        return None;
    }
    let mut out = Vec::with_capacity(cleaned.len() / 4 * 3);
    for chunk in cleaned.chunks(4) {
        let padding = chunk.iter().rev().take_while(|byte| **byte == b'=').count();
        if padding > 2 {
            return None;
        }
        let mut values = [0_u8; 4];
        for (index, byte) in chunk[..4 - padding].iter().enumerate() {
            values[index] = base64_value(*byte)?;
        }
        let packed = (u32::from(values[0]) << 18)
            | (u32::from(values[1]) << 12)
            | (u32::from(values[2]) << 6)
            | u32::from(values[3]);
        out.push((packed >> 16) as u8);
        if padding < 2 {
            out.push((packed >> 8) as u8);
        }
        if padding == 0 {
            out.push(packed as u8);
        }
    }
    Some(out)
}

const fn base64_value(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

fn read_file(path: &Path) -> Result<Vec<u8>> {
    std::fs::read(path)
        .map_err(|_| SecurityError::Configuration("trust-anchor file could not be read".to_owned()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::x509::RsaPkcs1Verifier;

    fn fixture(name: &str) -> Vec<u8> {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/pfx")
            .join(name);
        std::fs::read(path).expect("read committed PEM fixture")
    }

    fn cert(name: &str) -> Certificate {
        let pem = fixture(name);
        let certificates = decode_pem_chain(&pem).expect("parse PEM");
        let der = certificates[0].to_der().expect("certificate DER");
        Certificate::parse(&der).expect("parse fixture certificate")
    }

    /// The committed fixtures are valid from 2026-11-20 to 2031-11-20.
    const VALID_NOW: u64 = 1_800_000_000;

    #[test]
    fn real_fixture_chain_reaches_configured_anchor() {
        let root = cert("root.pem");
        let intermediate = cert("intermediate.pem");
        let leaf = cert("leaf.pem");
        assert_eq!(root.serial_number_bytes().len(), 16);
        assert_eq!(leaf.serial_number_bytes().len(), 16);

        let bundle = TrustAnchorBundle::from_pem(&fixture("root.pem")).expect("root bundle");
        assert_eq!(bundle.len(), 1);
        let report = bundle
            .evaluate(
                &leaf,
                std::slice::from_ref(&intermediate),
                &RsaPkcs1Verifier,
                VALID_NOW,
            )
            .expect("evaluation");
        assert_eq!(report.trusted(), Some(true));
        assert!(report.is_trusted());
        assert_eq!(report.reason(), None);
        assert_eq!(report.chain().len(), 3);
        assert_eq!(
            report.anchor().map(Certificate::subject),
            Some(root.subject())
        );
        assert_eq!(
            bundle
                .certificate_chain_trusted(
                    &leaf,
                    std::slice::from_ref(&intermediate),
                    &RsaPkcs1Verifier,
                    VALID_NOW,
                )
                .expect("decision"),
            Some(true)
        );
    }

    #[test]
    fn chain_without_path_to_anchor_is_some_false() {
        let root = cert("root.pem");
        let leaf = cert("leaf.pem");
        // The real root is configured, but the intermediate needed to reach it
        // is absent, so no configured path exists. Anchors are present, so the
        // outcome is `Some(false)`, never `None`.
        let bundle = TrustAnchorBundle::from_der(root.der()).expect("root anchor");
        let report = bundle
            .evaluate(&leaf, &[], &RsaPkcs1Verifier, VALID_NOW)
            .expect("evaluation");
        assert_eq!(report.trusted(), Some(false));
        assert_eq!(report.reason(), Some(TrustFailureReason::IssuerUnknown));
        assert!(report.chain().is_empty());
        assert_eq!(report.anchor(), None);
    }

    #[test]
    fn empty_bundle_yields_none_not_untrusted() {
        let intermediate = cert("intermediate.pem");
        let leaf = cert("leaf.pem");
        let bundle = TrustAnchorBundle::empty();
        assert!(bundle.is_empty());
        assert_eq!(
            bundle
                .certificate_chain_trusted(
                    &leaf,
                    std::slice::from_ref(&intermediate),
                    &RsaPkcs1Verifier,
                    VALID_NOW,
                )
                .expect("decision"),
            None
        );
        let report = bundle
            .evaluate(
                &leaf,
                std::slice::from_ref(&intermediate),
                &RsaPkcs1Verifier,
                VALID_NOW,
            )
            .expect("evaluation");
        assert_eq!(report.trusted(), None);
        assert_eq!(report.reason(), None);
    }

    #[test]
    fn expired_and_not_yet_valid_chains_are_typed_failures() {
        let root = cert("root.pem");
        let intermediate = cert("intermediate.pem");
        let leaf = cert("leaf.pem");
        let bundle = TrustAnchorBundle::from_der(root.der()).expect("bundle");

        let expired = bundle
            .evaluate(
                &leaf,
                std::slice::from_ref(&intermediate),
                &RsaPkcs1Verifier,
                2_000_000_000,
            )
            .expect("expired evaluation");
        assert_eq!(expired.trusted(), Some(false));
        assert_eq!(expired.reason(), Some(TrustFailureReason::Expired));

        let early = bundle
            .evaluate(
                &leaf,
                std::slice::from_ref(&intermediate),
                &RsaPkcs1Verifier,
                1_600_000_000,
            )
            .expect("not-yet-valid evaluation");
        assert_eq!(early.trusted(), Some(false));
        assert_eq!(early.reason(), Some(TrustFailureReason::NotYetValid));
    }

    #[test]
    fn bad_signature_is_a_typed_failure() {
        let root = cert("root.pem");
        let intermediate = cert("intermediate.pem");
        let leaf = cert("leaf.pem");
        let bundle = TrustAnchorBundle::from_der(root.der()).expect("bundle");
        let report = bundle
            .evaluate(
                &leaf,
                std::slice::from_ref(&intermediate),
                &crate::x509::RejectingVerifier,
                VALID_NOW,
            )
            .expect("evaluation");
        assert_eq!(report.trusted(), Some(false));
        assert_eq!(report.reason(), Some(TrustFailureReason::SignatureInvalid));
    }

    #[test]
    fn bundles_load_multiple_blocks_and_ignore_other_pem_blocks() {
        let root = cert("root.pem");
        let intermediate = cert("intermediate.pem");
        let mut pem = fixture("root.pem");
        pem.extend_from_slice(b"-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n");
        pem.extend_from_slice(&fixture("intermediate.pem"));
        let bundle = TrustAnchorBundle::from_pem(&pem).expect("multi-block PEM bundle");
        assert_eq!(bundle.len(), 2);
        assert_eq!(bundle.anchors()[0].der(), root.der());
        assert_eq!(bundle.anchors()[1].der(), intermediate.der());

        let mut der = root.der().to_vec();
        der.extend_from_slice(intermediate.der());
        let bundle = TrustAnchorBundle::from_der_bundle(&der).expect("DER bundle");
        assert_eq!(bundle.len(), 2);
    }

    #[test]
    fn malformed_bundle_inputs_are_typed_errors() {
        assert!(matches!(
            TrustAnchorBundle::from_pem(b"not a pem"),
            Err(SecurityError::TrustAnchorInvalid(_))
        ));
        assert!(matches!(
            TrustAnchorBundle::from_pem(b""),
            Err(SecurityError::TrustAnchorInvalid(_))
        ));
        assert_eq!(
            TrustAnchorBundle::from_der_bundle(&[]).map(|_| ()),
            Err(SecurityError::TrustAnchorInvalid("empty"))
        );
        assert!(TrustAnchorBundle::from_der(&[0x30, 0x00]).is_err());
    }
}
