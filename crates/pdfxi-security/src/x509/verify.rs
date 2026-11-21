//! Chain verification against a caller-supplied trust store.
//!
//! Signature checks are delegated to [`CertificateVerifier`]; this crate has
//! no keys and no built-in roots. A chain is trusted only when it terminates at
//! a certificate explicitly present in the caller's [`TrustStore`]. Every trust
//! decision is returned as [`ChainReport`] or a typed error.

use rsa::Pkcs1v15Sign;
use rsa::pkcs1::DecodeRsaPublicKey;
use sha2::{Digest as _, Sha256, Sha384, Sha512};

use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;
use crate::x509::extension::{EkuPurpose, KeyUsageBit};
use crate::x509::{
    Clock, OID_RSA_ENCRYPTION, SignatureAlgorithm, oid_string, reject_unknown_critical,
};

use super::PublicKeyInfo;

/// Boundary for certificate signature checks.
///
/// Implementors own any verification keys or crypto backend outside this crate.
/// The trait receives only public material: the signature algorithm, the exact
/// `tbsCertificate` DER bytes, the signature bits, and the issuer's public key.
pub trait CertificateVerifier {
    /// Return `true` only when `signature` is valid over `tbs_der` for the
    /// given algorithm and issuer public key.
    ///
    /// Returning `Err` aborts validation with that error; returning `Ok(false)`
    /// yields [`SecurityError::CertificateSignatureInvalid`].
    fn verify_signature(
        &self,
        algorithm: &SignatureAlgorithm,
        tbs_der: &[u8],
        signature: &[u8],
        issuer_key: &PublicKeyInfo,
    ) -> Result<bool>;
}

/// A [`CertificateVerifier`] test double that rejects every signature.
///
/// It holds no keys and is useful for negative-path tests and for callers that
/// want to fail closed until a real verifier is configured.
#[derive(Debug, Clone, Copy, Default)]
pub struct RejectingVerifier;

impl CertificateVerifier for RejectingVerifier {
    fn verify_signature(
        &self,
        _algorithm: &SignatureAlgorithm,
        _tbs_der: &[u8],
        _signature: &[u8],
        _issuer_key: &PublicKeyInfo,
    ) -> Result<bool> {
        Ok(false)
    }
}

/// A real PKCS#1 v1.5 RSA certificate-signature verifier.
///
/// This is the production [`CertificateVerifier`] for RSA issuer keys: it parses
/// the issuer's `RSAPublicKey` from [`PublicKeyInfo`] and verifies the
/// `sha256WithRSAEncryption`, `sha384WithRSAEncryption`, or
/// `sha512WithRSAEncryption` signature over the exact `tbsCertificate` bytes.
///
/// A hash mismatch, a malformed issuer key, a non-RSA issuer key, and the
/// legacy `sha1WithRSAEncryption` algorithm all return `Ok(false)`: they are
/// signature *decisions*, not structural errors, so path building can try
/// another same-subject issuer. ECDSA and unknown algorithms likewise return
/// `Ok(false)`; this crate does not implement EC verification in this round.
///
/// No key is stored in the value and no key material is ever logged.
#[derive(Debug, Clone, Copy, Default)]
pub struct RsaPkcs1Verifier;

impl CertificateVerifier for RsaPkcs1Verifier {
    fn verify_signature(
        &self,
        algorithm: &SignatureAlgorithm,
        tbs_der: &[u8],
        signature: &[u8],
        issuer_key: &PublicKeyInfo,
    ) -> Result<bool> {
        if issuer_key.algorithm_oid() != OID_RSA_ENCRYPTION {
            return Ok(false);
        }
        let Ok(key) = rsa::RsaPublicKey::from_pkcs1_der(issuer_key.key_bits()) else {
            return Ok(false);
        };
        let valid = match algorithm {
            SignatureAlgorithm::Sha256WithRsa => key
                .verify(
                    Pkcs1v15Sign::new::<Sha256>(),
                    Sha256::digest(tbs_der).as_slice(),
                    signature,
                )
                .is_ok(),
            SignatureAlgorithm::Sha384WithRsa => key
                .verify(
                    Pkcs1v15Sign::new::<Sha384>(),
                    Sha384::digest(tbs_der).as_slice(),
                    signature,
                )
                .is_ok(),
            SignatureAlgorithm::Sha512WithRsa => key
                .verify(
                    Pkcs1v15Sign::new::<Sha512>(),
                    Sha512::digest(tbs_der).as_slice(),
                    signature,
                )
                .is_ok(),
            _ => false,
        };
        Ok(valid)
    }
}

/// An explicit set of trust anchors and untrusted-but-pinned intermediates.
#[derive(Debug, Clone, Default)]
pub struct TrustStore {
    anchors: Vec<Certificate>,
    intermediates: Vec<Certificate>,
}

impl TrustStore {
    /// Create an empty trust store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a trust anchor.
    pub fn add_anchor(&mut self, certificate: Certificate) {
        self.anchors.push(certificate);
    }

    /// Add an intermediate certificate pinned by the caller.
    pub fn add_intermediate(&mut self, certificate: Certificate) {
        self.intermediates.push(certificate);
    }

    /// Builder form of [`Self::add_anchor`].
    pub fn with_anchor(mut self, certificate: Certificate) -> Self {
        self.add_anchor(certificate);
        self
    }

    /// Builder form of [`Self::add_intermediate`].
    pub fn with_intermediate(mut self, certificate: Certificate) -> Self {
        self.add_intermediate(certificate);
        self
    }

    /// Return the trust anchors.
    pub fn anchors(&self) -> &[Certificate] {
        &self.anchors
    }

    /// Return the pinned intermediates.
    pub fn intermediates(&self) -> &[Certificate] {
        &self.intermediates
    }

    /// Return `true` when `certificate` is a trust anchor by exact DER equality.
    pub fn is_anchor(&self, certificate: &Certificate) -> bool {
        self.anchors.iter().any(|anchor| anchor == certificate)
    }
}

/// Optional constraints applied during chain validation.
#[derive(Debug, Clone)]
pub struct ValidationPolicy {
    /// Maximum number of certificates in a chain, including the leaf.
    pub max_depth: usize,
    /// When set, the leaf's EKU (if present) must permit this purpose.
    pub required_eku: Option<EkuPurpose>,
    /// When set, the leaf's key usage (if present) must permit this bit.
    pub required_key_usage: Option<KeyUsageBit>,
    /// Whether to reject certificates outside their validity window.
    pub check_time: bool,
}

impl Default for ValidationPolicy {
    fn default() -> Self {
        Self {
            max_depth: 8,
            required_eku: None,
            required_key_usage: None,
            check_time: true,
        }
    }
}

impl ValidationPolicy {
    /// Set the maximum chain length.
    pub fn with_max_depth(mut self, max_depth: usize) -> Self {
        self.max_depth = max_depth;
        self
    }

    /// Require the leaf to permit `purpose` when it carries an EKU extension.
    pub fn requiring_eku(mut self, purpose: EkuPurpose) -> Self {
        self.required_eku = Some(purpose);
        self
    }

    /// Require the leaf to permit `usage` when it carries a key usage extension.
    pub fn requiring_key_usage(mut self, usage: KeyUsageBit) -> Self {
        self.required_key_usage = Some(usage);
        self
    }

    /// Enable or disable time-validity checks.
    pub fn with_time_checks(mut self, check_time: bool) -> Self {
        self.check_time = check_time;
        self
    }
}

/// The validated path from leaf to trust anchor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChainReport {
    chain: Vec<Certificate>,
}

impl ChainReport {
    /// Return the path in leaf-to-anchor order.
    pub fn chain(&self) -> &[Certificate] {
        &self.chain
    }

    /// Return the leaf certificate.
    pub fn leaf(&self) -> &Certificate {
        &self.chain[0]
    }

    /// Return the trust anchor that terminated the path.
    pub fn anchor(&self) -> &Certificate {
        &self.chain[self.chain.len() - 1]
    }

    /// Return the number of certificates in the path.
    pub fn depth(&self) -> usize {
        self.chain.len()
    }

    /// Every successful chain ends at a caller-supplied trust anchor.
    pub const fn is_trusted(&self) -> bool {
        true
    }
}

fn check_validity(certificate: &Certificate, clock: &dyn Clock, check: bool) -> Result<()> {
    if !check {
        return Ok(());
    }
    match certificate.validity_status_at(clock.now_unix_seconds()) {
        crate::x509::certificate::ValidityStatus::Valid => Ok(()),
        crate::x509::certificate::ValidityStatus::NotYetValid => {
            Err(SecurityError::CertificateNotYetValid)
        }
        crate::x509::certificate::ValidityStatus::Expired => Err(SecurityError::CertificateExpired),
    }
}

/// Reject a certificate whose signature OID this subset cannot interpret.
fn check_signature_algorithm(certificate: &Certificate) -> Result<()> {
    if certificate
        .signature_algorithm()
        .digest_algorithm()
        .is_some()
    {
        Ok(())
    } else {
        Err(SecurityError::UnsupportedSignatureAlgorithm(oid_string(
            &certificate.signature_algorithm().oid(),
        )))
    }
}

/// Intersection of EKU constraints with the caller's required purpose.
///
/// RFC 5280 4.2.1.12: a CA whose EKU extension omits a purpose must not issue a
/// certificate used for that purpose. Certificates without an EKU are
/// unconstrained.
fn check_eku_intersection(certificate: &Certificate, policy: &ValidationPolicy) -> Result<()> {
    if let Some(purpose) = &policy.required_eku
        && let Some(eku) = certificate.extended_key_usage()
        && !eku.allows(purpose)
    {
        return Err(SecurityError::CertificateEkuMissing);
    }
    Ok(())
}

/// Constraints that an issuer candidate must satisfy once its signature over
/// the current certificate has been accepted.
fn check_issuer_constraints(
    candidate: &Certificate,
    clock: &dyn Clock,
    policy: &ValidationPolicy,
    chain_len: usize,
) -> Result<()> {
    check_signature_algorithm(candidate)?;
    if !candidate.is_ca() {
        return Err(SecurityError::CertificateNotCa);
    }
    if !candidate.allows_usage(KeyUsageBit::KeyCertSign) {
        return Err(SecurityError::CertificateKeyUsageViolation(
            KeyUsageBit::KeyCertSign.label(),
        ));
    }
    reject_unknown_critical(candidate.unknown_critical_extensions())?;
    check_validity(candidate, clock, policy.check_time)?;
    check_eku_intersection(candidate, policy)?;
    if let Some(path_len) = candidate.basic_constraints().and_then(|bc| bc.path_len()) {
        // Number of intermediate CA certificates below `candidate`: `chain`
        // holds every certificate under `current`, which is itself the nearest
        // of those intermediates.
        if chain_len as u64 > path_len {
            return Err(SecurityError::CertificateChainTooLong);
        }
    }
    Ok(())
}

/// Verify `leaf` against `store` using `verifier` for all signature checks.
///
/// `untrusted` is an ordered list of additional certificates available for path
/// building (for example, certificates embedded in a PDF signature). The clock
/// is supplied by the caller, so validation is deterministic and never reads
/// the host clock.
///
/// # Errors
///
/// - [`SecurityError::CertificateExpired`] / [`SecurityError::CertificateNotYetValid`]
///   when time checks fail.
/// - [`SecurityError::CertificateIssuerUnknown`] or [`SecurityError::CertificateUntrusted`]
///   when no path reaches an anchor.
/// - [`SecurityError::CertificateSignatureInvalid`] when the verifier rejects a
///   signature.
/// - Key-usage, CA-constraint, EKU, and critical-extension violations as typed
///   errors.
pub fn verify_chain(
    leaf: &Certificate,
    untrusted: &[Certificate],
    store: &TrustStore,
    verifier: &dyn CertificateVerifier,
    policy: &ValidationPolicy,
    clock: &dyn Clock,
) -> Result<ChainReport> {
    if let Some(usage) = policy.required_key_usage
        && !leaf.allows_usage(usage)
    {
        return Err(SecurityError::CertificateKeyUsageViolation(usage.label()));
    }
    let chain = build_path(leaf, &[], untrusted, store, verifier, policy, clock)?;
    Ok(ChainReport { chain })
}

/// Depth-first path construction with backtracking.
///
/// `below` holds the certificates already selected below `current`, in
/// leaf-to-issuer order. A candidate that fails local constraints, or that
/// starts a branch with no route to a trust anchor, does not abort the search:
/// the next same-subject candidate is tried instead. This is what makes
/// cross-signed and decoy-populated paths verifiable.
#[allow(clippy::too_many_arguments)]
fn build_path(
    current: &Certificate,
    below: &[Certificate],
    untrusted: &[Certificate],
    store: &TrustStore,
    verifier: &dyn CertificateVerifier,
    policy: &ValidationPolicy,
    clock: &dyn Clock,
) -> Result<Vec<Certificate>> {
    if below.len() >= policy.max_depth {
        return Err(SecurityError::CertificateChainTooLong);
    }
    check_signature_algorithm(current)?;
    reject_unknown_critical(current.unknown_critical_extensions())?;
    check_validity(current, clock, policy.check_time)?;
    check_eku_intersection(current, policy)?;

    if store.is_anchor(current) {
        let mut chain = below.to_vec();
        chain.push(current.clone());
        return Ok(chain);
    }
    if below.len() + 1 >= policy.max_depth {
        return Err(SecurityError::CertificateChainTooLong);
    }

    let candidates: Vec<&Certificate> = untrusted
        .iter()
        .chain(store.intermediates().iter())
        .chain(store.anchors().iter())
        .filter(|candidate| candidate.subject() == current.issuer())
        .filter(|candidate| candidate.der() != current.der())
        .filter(|candidate| {
            !below
                .iter()
                .any(|selected| selected.der() == candidate.der())
        })
        .collect();
    if candidates.is_empty() {
        if store.anchors().is_empty() {
            return Err(SecurityError::CertificateUntrusted);
        }
        return Err(SecurityError::CertificateIssuerUnknown);
    }

    let mut first_error: Option<SecurityError> = None;
    for candidate in candidates {
        // Signature matching precedes every local constraint: an attacker-
        // controlled same-subject decoy must never abort the whole search.
        let valid_signature = verifier.verify_signature(
            current.signature_algorithm(),
            current.tbs_der(),
            current.signature_bits(),
            candidate.subject_public_key_info(),
        )?;
        if !valid_signature {
            continue;
        }
        if let Err(error) = check_issuer_constraints(candidate, clock, policy, below.len()) {
            if first_error.is_none() {
                first_error = Some(error);
            }
            continue;
        }
        let mut next_below = below.to_vec();
        next_below.push(current.clone());
        match build_path(
            candidate,
            &next_below,
            untrusted,
            store,
            verifier,
            policy,
            clock,
        ) {
            Ok(chain) => return Ok(chain),
            Err(error) => {
                if first_error.is_none() {
                    first_error = Some(error);
                }
            }
        }
    }
    Err(first_error.unwrap_or(SecurityError::CertificateSignatureInvalid))
}
