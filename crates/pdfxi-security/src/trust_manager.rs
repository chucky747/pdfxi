//! Local trust manager and revocation checks (SEC-007).
//!
//! This module layers deterministic trust decisions and certificate-revocation
//! checking on top of the existing X.509 chain builder
//! ([`crate::x509::verify_chain`]). A [`TrustManager`] owns a caller-supplied
//! [`TrustStore`], a [`ValidationPolicy`], a [`RevocationPolicy`], and a fixed
//! evaluation clock. It never reads the host clock and never fetches anything
//! from the network: callers supply every CRL explicitly.
//!
//! Every call returns a typed [`TrustReport`] carrying the decision plus
//! evidence (chain serials, anchor, and per-certificate revocation status).
//! Only configuration / malformed-input problems produce a
//! [`SecurityError`]; an untrusted or revoked chain is a *decision*, not an
//! error, so callers get a complete, deterministic report in all cases.
//!
//! # CRL signatures
//!
//! A [`Crl`] may carry a signature over its canonical body. [`Crl::verify_signature`]
//! checks that signature against a caller-supplied issuer certificate through
//! the same [`CertificateVerifier`] boundary used for certificate-chain and
//! timestamp verification. When [`RevocationPolicy::verify_crl_signatures`] is
//! enabled, [`TrustManager::evaluate`] ignores CRLs whose signature does not
//! verify or whose issuer does not match the chain issuer, and returns a typed
//! [`TrustFailureReason::CrlSignatureInvalid`] when no authentic CRL remains.
//! No network access and no host clock are involved.

use crate::der::{
    DerReader, Tag, encode_integer, encode_null, encode_oid, encode_sequence, encode_time,
};
use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;
use crate::x509::name::Name;
use crate::x509::verify::{CertificateVerifier, TrustStore, ValidationPolicy, verify_chain};
use crate::x509::{Clock, FixedClock, KeyUsageBit, SignatureAlgorithm};

/// RFC 5280 CRL entry reason codes used by this model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationReason {
    /// No specific reason.
    Unspecified,
    /// The subject's private key was compromised.
    KeyCompromise,
    /// The CA's private key was compromised.
    CaCompromise,
    /// The subject's affiliation changed.
    AffiliationChanged,
    /// The certificate was superseded.
    Superseded,
    /// Operation is no longer required.
    CessationOfOperation,
    /// The certificate is temporarily on hold.
    CertificateHold,
}

impl RevocationReason {
    /// Stable integer code for DER encoding.
    pub const fn code(self) -> u64 {
        match self {
            Self::Unspecified => 0,
            Self::KeyCompromise => 1,
            Self::CaCompromise => 2,
            Self::AffiliationChanged => 3,
            Self::Superseded => 4,
            Self::CessationOfOperation => 5,
            Self::CertificateHold => 6,
        }
    }

    fn from_code(code: u64) -> Self {
        match code {
            1 => Self::KeyCompromise,
            2 => Self::CaCompromise,
            3 => Self::AffiliationChanged,
            4 => Self::Superseded,
            5 => Self::CessationOfOperation,
            6 => Self::CertificateHold,
            _ => Self::Unspecified,
        }
    }
}

/// One revoked-certificate entry in a CRL.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RevokedCertificate {
    serial: u64,
    revocation_date: u64,
    reason: RevocationReason,
}

impl RevokedCertificate {
    /// Create a revocation entry.
    pub const fn new(serial: u64, revocation_date: u64, reason: RevocationReason) -> Self {
        Self {
            serial,
            revocation_date,
            reason,
        }
    }

    /// Serial number of the revoked certificate.
    pub const fn serial(&self) -> u64 {
        self.serial
    }

    /// Unix seconds at which revocation took effect.
    pub const fn revocation_date(&self) -> u64 {
        self.revocation_date
    }

    /// Reason for revocation.
    pub const fn reason(&self) -> RevocationReason {
        self.reason
    }
}

/// A caller-supplied certificate revocation list.
///
/// This is a clean-room model of an RFC 5280 CRL: it carries the issuing CA
/// name, an update window, revoked serials, and an optional signature over the
/// canonical body. [`Crl::verify_signature`] checks that signature against a
/// caller-supplied issuer certificate through the shared
/// [`CertificateVerifier`] boundary; this crate never fetches a CRL or holds a
/// signing key. The DER form is deterministic and round-trippable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crl {
    issuer: Name,
    issuer_der: Vec<u8>,
    this_update: u64,
    next_update: u64,
    revoked: Vec<RevokedCertificate>,
    tbs_der: Vec<u8>,
    signature_algorithm: Option<SignatureAlgorithm>,
    signature: Vec<u8>,
}

/// OID `0.0`, used as a placeholder for an unsigned CRL's AlgorithmIdentifier.
const OID_UNSIGNED_CRL: [u32; 1] = [0];

fn encode_oid_or_fallback(oid: &[u32]) -> Vec<u8> {
    match encode_oid(oid) {
        Ok(bytes) => bytes,
        Err(_) => vec![0x06, 0x01, 0x00],
    }
}

/// Encode the canonical, signed body (`tbsCertList`) of a CRL.
fn encode_crl_tbs(
    issuer_der: &[u8],
    this_update: u64,
    next_update: u64,
    revoked: &[RevokedCertificate],
) -> Vec<u8> {
    let mut revoked_der = Vec::new();
    for entry in revoked {
        let mut item = encode_integer(entry.serial);
        if let Ok(time) = encode_time(entry.revocation_date) {
            item.extend_from_slice(&time);
        }
        item.extend_from_slice(&encode_integer(entry.reason.code()));
        revoked_der.extend_from_slice(&encode_sequence(&item));
    }
    let mut content = issuer_der.to_vec();
    if let Ok(time) = encode_time(this_update) {
        content.extend_from_slice(&time);
    }
    if let Ok(time) = encode_time(next_update) {
        content.extend_from_slice(&time);
    }
    content.extend_from_slice(&encode_sequence(&revoked_der));
    encode_sequence(&content)
}

impl Crl {
    /// Build an unsigned CRL from a DER-encoded issuer `Name`, an update
    /// window, and revocation entries.
    ///
    /// Returns [`SecurityError::CrlInvalidWindow`] when `next_update` precedes
    /// `this_update`, and a typed DER error if `issuer_der` is not one
    /// well-formed `Name` value. Use [`Self::with_signature`] to attach a
    /// signature over the canonical body returned by [`Self::tbs_der`].
    pub fn new(
        issuer_der: &[u8],
        this_update: u64,
        next_update: u64,
        revoked: Vec<RevokedCertificate>,
    ) -> Result<Self> {
        if next_update < this_update {
            return Err(SecurityError::CrlInvalidWindow);
        }
        let mut reader = DerReader::new(issuer_der);
        let tlv = reader.read_tlv()?;
        reader.expect_end()?;
        let issuer = Name::parse(&tlv)?;
        let issuer_der = tlv.encoded().to_vec();
        let tbs_der = encode_crl_tbs(&issuer_der, this_update, next_update, &revoked);
        Ok(Self {
            issuer,
            issuer_der,
            this_update,
            next_update,
            revoked,
            tbs_der,
            signature_algorithm: None,
            signature: Vec::new(),
        })
    }

    /// Attach a signature to this CRL.
    ///
    /// `signature` must be the caller's signature over [`Self::tbs_der`] using
    /// `algorithm`; the caller owns the signing key. This crate never signs.
    pub fn with_signature(mut self, algorithm: SignatureAlgorithm, signature: Vec<u8>) -> Self {
        self.signature_algorithm = Some(algorithm);
        self.signature = signature;
        self
    }

    /// Canonical DER bytes covered by the CRL signature (`tbsCertList`).
    pub fn tbs_der(&self) -> &[u8] {
        &self.tbs_der
    }

    /// Signature algorithm attached to this CRL, if any.
    pub const fn signature_algorithm(&self) -> Option<&SignatureAlgorithm> {
        self.signature_algorithm.as_ref()
    }

    /// Signature bytes attached to this CRL (empty when unsigned).
    pub fn signature(&self) -> &[u8] {
        &self.signature
    }

    /// Return `true` when this CRL carries a non-empty signature.
    pub fn is_signed(&self) -> bool {
        !self.signature.is_empty()
    }

    /// Verify this CRL's signature against the caller-supplied issuer
    /// certificate using the shared [`CertificateVerifier`] boundary.
    ///
    /// The check enforces that the issuer subject matches the CRL issuer name,
    /// that the issuer key usage permits `cRLSign`, that a signature is present
    /// and its algorithm is recognized, and finally delegates the signature
    /// check to `verifier`. A cryptographic mismatch returns `Ok(false)`;
    /// structural problems return typed errors.
    pub fn verify_signature(
        &self,
        issuer: &Certificate,
        verifier: &dyn CertificateVerifier,
    ) -> Result<bool> {
        if issuer.subject() != &self.issuer {
            return Ok(false);
        }
        if !issuer.allows_usage(KeyUsageBit::CrlSign) {
            return Err(SecurityError::CertificateKeyUsageViolation("cRLSign"));
        }
        let algorithm = self
            .signature_algorithm
            .as_ref()
            .ok_or(SecurityError::CrlSignatureMissing)?;
        if self.signature.is_empty() {
            return Err(SecurityError::CrlSignatureMissing);
        }
        if algorithm.digest_algorithm().is_none() {
            return Err(SecurityError::UnsupportedSignatureAlgorithm(
                algorithm.as_str(),
            ));
        }
        verifier.verify_signature(
            algorithm,
            &self.tbs_der,
            &self.signature,
            issuer.subject_public_key_info(),
        )
    }

    /// Parse a CRL from its canonical DER encoding (inverse of [`Self::to_der`]).
    ///
    /// The encoding is `CertificateList { tbsCertList, signatureAlgorithm,
    /// signatureValue }`. An empty signature value denotes an unsigned CRL.
    /// Truncated or structurally invalid input yields a typed
    /// [`SecurityError::CrlMalformed`].
    pub fn from_der(der: &[u8]) -> Result<Self> {
        let mut top = DerReader::new(der);
        let outer = top.read_tlv()?;
        top.expect_end()?;
        let mut outer_sequence = outer
            .as_sequence()
            .map_err(|_| SecurityError::CrlMalformed)?;

        let tbs_tlv = outer_sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?;
        let mut sequence = tbs_tlv
            .as_sequence()
            .map_err(|_| SecurityError::CrlMalformed)?;

        let issuer_tlv = sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?;
        let issuer = Name::parse(&issuer_tlv).map_err(|_| SecurityError::CrlMalformed)?;
        let this_update = sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?
            .as_time()
            .map_err(|_| SecurityError::CrlMalformed)?;
        let next_update = sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?
            .as_time()
            .map_err(|_| SecurityError::CrlMalformed)?;
        if next_update < this_update {
            return Err(SecurityError::CrlInvalidWindow);
        }

        let mut revoked = Vec::new();
        if let Some(tag) = sequence.peek_tag()
            && tag == Tag::SEQUENCE
        {
            let mut list = sequence
                .read_tlv()
                .map_err(|_| SecurityError::CrlMalformed)?
                .as_sequence()
                .map_err(|_| SecurityError::CrlMalformed)?;
            while !list.is_empty() {
                let mut entry = list
                    .read_tlv()
                    .map_err(|_| SecurityError::CrlMalformed)?
                    .as_sequence()
                    .map_err(|_| SecurityError::CrlMalformed)?;
                let serial = entry
                    .read_tlv()
                    .map_err(|_| SecurityError::CrlMalformed)?
                    .as_integer_u64()
                    .map_err(|_| SecurityError::CrlMalformed)?;
                let revocation_date = entry
                    .read_tlv()
                    .map_err(|_| SecurityError::CrlMalformed)?
                    .as_time()
                    .map_err(|_| SecurityError::CrlMalformed)?;
                let reason = match entry.read_tlv() {
                    Ok(tlv) => RevocationReason::from_code(
                        tlv.as_integer_u64()
                            .map_err(|_| SecurityError::CrlMalformed)?,
                    ),
                    Err(_) => RevocationReason::Unspecified,
                };
                entry
                    .expect_end()
                    .map_err(|_| SecurityError::CrlMalformed)?;
                revoked.push(RevokedCertificate::new(serial, revocation_date, reason));
            }
        }
        sequence
            .expect_end()
            .map_err(|_| SecurityError::CrlMalformed)?;

        let algorithm_tlv = outer_sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?;
        let (oid, _parameters) = crate::x509::parse_algorithm_identifier(&algorithm_tlv)
            .map_err(|_| SecurityError::CrlMalformed)?;
        let signature_tlv = outer_sequence
            .read_tlv()
            .map_err(|_| SecurityError::CrlMalformed)?;
        let signature = signature_tlv
            .as_octets()
            .map_err(|_| SecurityError::CrlMalformed)?
            .to_vec();
        outer_sequence
            .expect_end()
            .map_err(|_| SecurityError::CrlMalformed)?;

        let (signature_algorithm, signature) = if signature.is_empty() || oid == OID_UNSIGNED_CRL {
            (None, Vec::new())
        } else {
            (Some(SignatureAlgorithm::from_oid(&oid)), signature)
        };

        Ok(Self {
            issuer,
            issuer_der: issuer_tlv.encoded().to_vec(),
            this_update,
            next_update,
            revoked,
            tbs_der: tbs_tlv.encoded().to_vec(),
            signature_algorithm,
            signature,
        })
    }

    /// Encode this CRL in the canonical DER form.
    ///
    /// The body bytes emitted as `tbsCertList` are exactly those returned by
    /// [`Self::tbs_der`], so a signature over that slice verifies after a
    /// `to_der` / `from_der` round trip.
    pub fn to_der(&self) -> Vec<u8> {
        let algorithm = match &self.signature_algorithm {
            Some(algorithm) => algorithm.oid(),
            None => OID_UNSIGNED_CRL.to_vec(),
        };
        let mut algorithm_id = encode_oid_or_fallback(&algorithm);
        algorithm_id.extend_from_slice(&encode_null());

        let mut content = self.tbs_der.clone();
        content.extend_from_slice(&encode_sequence(&algorithm_id));
        content.extend_from_slice(&crate::der::encode_tlv(Tag::OCTET_STRING, &self.signature));
        encode_sequence(&content)
    }

    /// Issuing CA name of this CRL.
    pub fn issuer(&self) -> &Name {
        &self.issuer
    }

    /// Start of the CRL's validity window (Unix seconds).
    pub const fn this_update(&self) -> u64 {
        self.this_update
    }

    /// End of the CRL's validity window (Unix seconds).
    pub const fn next_update(&self) -> u64 {
        self.next_update
    }

    /// Revocation entries in insertion order.
    pub fn revoked(&self) -> &[RevokedCertificate] {
        &self.revoked
    }

    /// Return `true` when `now` falls inside the CRL's update window.
    pub const fn is_fresh_at(&self, now: u64) -> bool {
        now >= self.this_update && now <= self.next_update
    }

    /// Classify one serial against this CRL at `now`.
    pub fn status_for(&self, serial: u64, now: u64) -> RevocationStatus {
        if let Some(entry) = self.revoked.iter().find(|entry| entry.serial == serial) {
            return RevocationStatus::Revoked {
                date: entry.revocation_date,
                reason: entry.reason,
            };
        }
        if self.is_fresh_at(now) {
            RevocationStatus::Good
        } else {
            RevocationStatus::Stale
        }
    }
}

/// Result of checking a certificate serial for revocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RevocationStatus {
    /// A current CRL was found and the serial is not listed.
    Good,
    /// The serial is listed as revoked by a supplied CRL.
    Revoked {
        /// Unix seconds at which revocation took effect.
        date: u64,
        /// Recorded reason.
        reason: RevocationReason,
    },
    /// No CRL was supplied for the certificate's issuer.
    Unknown,
    /// A CRL was supplied but is outside its update window at evaluation time.
    Stale,
}

impl RevocationStatus {
    /// Return `true` only for [`Self::Good`].
    pub const fn is_good(self) -> bool {
        matches!(self, Self::Good)
    }
}

/// How strictly revocation information is required during evaluation.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RevocationPolicy {
    /// Require at least one CRL for every non-anchor certificate.
    pub require_present: bool,
    /// Treat a supplied-but-stale CRL as a failure.
    pub require_fresh: bool,
    /// Require each matching CRL's signature to verify against the chain
    /// issuer before its revocation entries are trusted.
    ///
    /// When enabled, CRLs with an invalid signature, wrong issuer, or missing
    /// signature are excluded from evidence. If no authentic CRL remains, the
    /// decision carries [`TrustFailureReason::CrlSignatureInvalid`].
    pub verify_crl_signatures: bool,
}

impl RevocationPolicy {
    /// Conservative policy: presence, freshness, and CRL signatures are all
    /// required.
    pub const fn strict() -> Self {
        Self {
            require_present: true,
            require_fresh: true,
            verify_crl_signatures: true,
        }
    }
}

/// Why a chain was not trusted.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustFailureReason {
    /// No trust anchor was reachable.
    NoAnchor,
    /// An issuer certificate could not be found.
    IssuerUnknown,
    /// The chain exceeded the configured depth.
    ChainTooLong,
    /// A certificate signature was invalid or unsupported.
    SignatureInvalid,
    /// A certificate had expired.
    Expired,
    /// A certificate was not yet valid.
    NotYetValid,
    /// An issuing certificate was not a CA.
    NotCa,
    /// A certificate's key usage did not permit the operation.
    KeyUsageViolation,
    /// A required extended key usage was absent.
    EkuMissing,
    /// An unknown critical extension was present.
    UnknownCriticalExtension,
    /// A certificate or CRL input was malformed.
    MalformedCertificate,
    /// A certificate in the chain was revoked.
    Revoked {
        /// Serial of the revoked certificate.
        serial: u64,
    },
    /// Revocation information was required but unavailable for a serial.
    RevocationUnknown {
        /// Serial with no matching CRL.
        serial: u64,
    },
    /// A required fresh CRL was stale for a serial.
    RevocationStale {
        /// Serial whose CRL was stale.
        serial: u64,
    },
    /// Matching CRLs were present but none had a valid signature from the
    /// chain issuer (wrong issuer, missing or invalid signature).
    CrlSignatureInvalid {
        /// Serial whose CRL signatures all failed verification.
        serial: u64,
    },
}

impl TrustFailureReason {
    pub(crate) fn from_security_error(error: &SecurityError) -> Self {
        match error {
            SecurityError::CertificateUntrusted => Self::NoAnchor,
            SecurityError::CertificateIssuerUnknown => Self::IssuerUnknown,
            SecurityError::CertificateChainTooLong => Self::ChainTooLong,
            SecurityError::CertificateSignatureInvalid
            | SecurityError::UnsupportedSignatureAlgorithm(_) => Self::SignatureInvalid,
            SecurityError::CertificateExpired => Self::Expired,
            SecurityError::CertificateNotYetValid => Self::NotYetValid,
            SecurityError::CertificateNotCa => Self::NotCa,
            SecurityError::CertificateKeyUsageViolation(_) => Self::KeyUsageViolation,
            SecurityError::CertificateEkuMissing => Self::EkuMissing,
            SecurityError::CertificateUnknownCriticalExtension => Self::UnknownCriticalExtension,
            _ => Self::MalformedCertificate,
        }
    }
}

/// The trust decision for one evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TrustDecision {
    /// The chain reached a trust anchor and passed revocation checks.
    Trusted,
    /// The chain failed with a typed reason.
    Untrusted {
        /// Why the chain was rejected.
        reason: TrustFailureReason,
    },
}

impl TrustDecision {
    /// Return `true` only for [`Self::Trusted`].
    pub const fn is_trusted(self) -> bool {
        matches!(self, Self::Trusted)
    }
}

/// Per-certificate revocation evidence in a [`TrustReport`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RevocationEvidence {
    serial: u64,
    status: RevocationStatus,
    crl_issuer_display: Option<String>,
    crl_signature_verified: bool,
}

impl RevocationEvidence {
    /// Serial of the checked certificate.
    pub const fn serial(&self) -> u64 {
        self.serial
    }

    /// Revocation status determined for the certificate.
    pub const fn status(&self) -> RevocationStatus {
        self.status
    }

    /// RFC 4514 display of the matching CRL issuer, when one was found.
    pub fn crl_issuer_display(&self) -> Option<&str> {
        self.crl_issuer_display.as_deref()
    }

    /// Whether at least one matching CRL had a signature that verified against
    /// the chain issuer.
    ///
    /// This is always `false` when [`RevocationPolicy::verify_crl_signatures`]
    /// is disabled, because no signature check was performed.
    pub const fn crl_signature_verified(&self) -> bool {
        self.crl_signature_verified
    }
}

/// Typed, deterministic evidence for one trust evaluation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrustReport {
    decision: TrustDecision,
    chain_serials: Vec<u64>,
    anchor_serial: Option<u64>,
    revocation: Vec<RevocationEvidence>,
    evaluated_at: u64,
}

impl TrustReport {
    /// The trust decision.
    pub const fn decision(&self) -> TrustDecision {
        self.decision
    }

    /// Shortcut for `decision().is_trusted()`.
    pub const fn is_trusted(&self) -> bool {
        self.decision.is_trusted()
    }

    /// Serials of the validated chain in leaf-to-anchor order.
    pub fn chain_serials(&self) -> &[u64] {
        &self.chain_serials
    }

    /// Serial of the terminating trust anchor, when a chain was built.
    pub const fn anchor_serial(&self) -> Option<u64> {
        self.anchor_serial
    }

    /// Revocation evidence in leaf-to-anchor order (excluding the anchor).
    pub fn revocation(&self) -> &[RevocationEvidence] {
        &self.revocation
    }

    /// Unix seconds at which the decision was made.
    pub const fn evaluated_at(&self) -> u64 {
        self.evaluated_at
    }
}

fn combined_status(crls: &[&Crl], serial: u64, now: u64) -> (RevocationStatus, Option<String>) {
    let mut saw_good = false;
    let mut saw_stale = false;
    let mut display = None;
    for crl in crls {
        let status = crl.status_for(serial, now);
        if display.is_none() {
            display = Some(crl.issuer().display_rfc4514());
        }
        match status {
            RevocationStatus::Revoked { .. } => return (status, display),
            RevocationStatus::Good => saw_good = true,
            RevocationStatus::Stale => saw_stale = true,
            RevocationStatus::Unknown => {}
        }
    }
    let status = if crls.is_empty() {
        RevocationStatus::Unknown
    } else if saw_good {
        RevocationStatus::Good
    } else if saw_stale {
        RevocationStatus::Stale
    } else {
        RevocationStatus::Unknown
    };
    (status, display)
}

fn reason_from_status(
    status: RevocationStatus,
    serial: u64,
    policy: RevocationPolicy,
) -> Option<TrustFailureReason> {
    match status {
        RevocationStatus::Revoked { .. } => Some(TrustFailureReason::Revoked { serial }),
        RevocationStatus::Unknown if policy.require_present => {
            Some(TrustFailureReason::RevocationUnknown { serial })
        }
        RevocationStatus::Stale if policy.require_fresh => {
            Some(TrustFailureReason::RevocationStale { serial })
        }
        _ => None,
    }
}

/// A deterministic trust decision engine over a local trust store.
#[derive(Debug, Clone)]
pub struct TrustManager {
    store: TrustStore,
    policy: ValidationPolicy,
    revocation_policy: RevocationPolicy,
    now: u64,
}

impl TrustManager {
    /// Create a manager over `store` with default policies and a pinned clock.
    ///
    /// # Security note
    ///
    /// The default [`RevocationPolicy`] is deliberately permissive: a missing
    /// CRL yields [`RevocationStatus::Unknown`] and a stale CRL yields
    /// [`RevocationStatus::Stale`] evidence, but neither fails the decision on
    /// its own. Callers that require fail-closed revocation must pass
    /// [`RevocationPolicy::strict`] via [`Self::with_revocation_policy`]. A CRL
    /// that explicitly lists a serial as revoked always fails the decision,
    /// regardless of policy.
    pub fn new(store: TrustStore, now: u64) -> Self {
        Self {
            store,
            policy: ValidationPolicy::default(),
            revocation_policy: RevocationPolicy::default(),
            now,
        }
    }

    /// Replace the chain-validation policy.
    pub fn with_policy(mut self, policy: ValidationPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Replace the revocation policy.
    pub fn with_revocation_policy(mut self, policy: RevocationPolicy) -> Self {
        self.revocation_policy = policy;
        self
    }

    /// The trust store used by this manager.
    pub const fn store(&self) -> &TrustStore {
        &self.store
    }

    /// The pinned evaluation clock, in Unix seconds.
    pub const fn now(&self) -> u64 {
        self.now
    }

    /// Evaluate `leaf` against the configured store and the supplied CRLs.
    ///
    /// Trust and revocation outcomes — including an empty trust store — are
    /// reported in the returned [`TrustReport`]; the report is deterministic for
    /// a given input. A malformed `Crl` is rejected at construction time with a
    /// typed error before it can reach this method.
    pub fn evaluate(
        &self,
        leaf: &Certificate,
        untrusted: &[Certificate],
        crls: &[Crl],
        verifier: &dyn CertificateVerifier,
    ) -> Result<TrustReport> {
        if self.store.anchors().is_empty() {
            return Ok(TrustReport {
                decision: TrustDecision::Untrusted {
                    reason: TrustFailureReason::NoAnchor,
                },
                chain_serials: Vec::new(),
                anchor_serial: None,
                revocation: Vec::new(),
                evaluated_at: self.now,
            });
        }
        let clock = FixedClock::new(self.now);
        let chain_report = match verify_chain(
            leaf,
            untrusted,
            &self.store,
            verifier,
            &self.policy,
            &clock as &dyn Clock,
        ) {
            Ok(report) => report,
            Err(error) => {
                return Ok(TrustReport {
                    decision: TrustDecision::Untrusted {
                        reason: TrustFailureReason::from_security_error(&error),
                    },
                    chain_serials: Vec::new(),
                    anchor_serial: None,
                    revocation: Vec::new(),
                    evaluated_at: self.now,
                });
            }
        };

        let chain = chain_report.chain();
        let chain_serials: Vec<u64> = chain.iter().map(Certificate::serial_number).collect();
        let anchor_serial = chain.last().map(Certificate::serial_number);

        let mut revocation = Vec::new();
        let mut decision = TrustDecision::Trusted;
        // The trust anchor is the revocation authority terminus; only issued
        // certificates are checked against CRLs. In a validated chain the next
        // certificate is the issuer of the current one.
        let issued_count = chain.len().saturating_sub(1);
        for (index, certificate) in chain.iter().take(issued_count).enumerate() {
            let issuer_certificate = &chain[index + 1];
            let name_matching: Vec<&Crl> = crls
                .iter()
                .filter(|crl| crl.issuer() == certificate.issuer())
                .collect();
            let mut signature_rejected = false;
            let mut signature_verified = false;
            let matching: Vec<&Crl> = if self.revocation_policy.verify_crl_signatures {
                let mut verified = Vec::new();
                for crl in name_matching {
                    match crl.verify_signature(issuer_certificate, verifier) {
                        Ok(true) => {
                            signature_verified = true;
                            verified.push(crl);
                        }
                        Ok(false) | Err(_) => signature_rejected = true,
                    }
                }
                verified
            } else {
                name_matching
            };
            let (status, display) =
                combined_status(&matching, certificate.serial_number(), self.now);
            revocation.push(RevocationEvidence {
                serial: certificate.serial_number(),
                status,
                crl_issuer_display: display,
                crl_signature_verified: signature_verified,
            });
            if decision.is_trusted() {
                if signature_rejected && matching.is_empty() {
                    decision = TrustDecision::Untrusted {
                        reason: TrustFailureReason::CrlSignatureInvalid {
                            serial: certificate.serial_number(),
                        },
                    };
                } else if let Some(reason) =
                    reason_from_status(status, certificate.serial_number(), self.revocation_policy)
                {
                    decision = TrustDecision::Untrusted { reason };
                }
            }
        }

        Ok(TrustReport {
            decision,
            chain_serials,
            anchor_serial,
            revocation,
            evaluated_at: self.now,
        })
    }

    /// Convenience wrapper returning only whether the leaf is trusted.
    pub fn is_trusted(
        &self,
        leaf: &Certificate,
        untrusted: &[Certificate],
        crls: &[Crl],
        verifier: &dyn CertificateVerifier,
    ) -> Result<bool> {
        Ok(self.evaluate(leaf, untrusted, crls, verifier)?.is_trusted())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::{Tag, encode_oid, encode_tlv, write_tlv};
    use crate::x509::OID_SHA256_WITH_RSA;
    use crate::x509::extension::KeyUsage;

    const OID_CN: [u32; 4] = [2, 5, 4, 3];
    const OID_BASIC_CONSTRAINTS: [u32; 4] = [2, 5, 29, 19];
    const OID_KEY_USAGE: [u32; 4] = [2, 5, 29, 15];

    /// Test verifier: first 16 bytes of SHA-256(TBS) acts as a signature.
    struct DigestVerifier;

    impl CertificateVerifier for DigestVerifier {
        fn verify_signature(
            &self,
            _algorithm: &crate::x509::SignatureAlgorithm,
            tbs_der: &[u8],
            signature: &[u8],
            _issuer_key: &crate::PublicKeyInfo,
        ) -> Result<bool> {
            Ok(signature == &crate::crypto::sha::sha256(tbs_der)[..16])
        }
    }

    fn name_der(cn: &str) -> Vec<u8> {
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        encode_sequence(&encode_set_of(&encode_sequence(&ava)))
    }

    fn encode_set_of(children: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_tlv(&mut out, Tag::SET, children);
        out
    }

    fn bit_string(payload: &[u8]) -> Vec<u8> {
        let mut content = vec![0u8];
        content.extend_from_slice(payload);
        encode_tlv(Tag::BIT_STRING, &content)
    }

    fn alg_id() -> Vec<u8> {
        let mut children = encode_oid(&OID_SHA256_WITH_RSA).expect("oid");
        children.extend_from_slice(&crate::der::encode_null());
        encode_sequence(&children)
    }

    fn validity() -> Vec<u8> {
        let mut children = crate::der::encode_time(1_600_000_000).expect("time");
        children.extend_from_slice(&crate::der::encode_time(1_700_000_000).expect("time"));
        encode_sequence(&children)
    }

    fn basic_constraints_ext(ca: bool) -> Vec<u8> {
        let mut inner = Vec::new();
        if ca {
            inner.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
        }
        let mut children = encode_oid(&OID_BASIC_CONSTRAINTS).expect("oid");
        children.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
        let ext_value = encode_sequence(&inner);
        let mut payload = Vec::new();
        write_tlv(&mut payload, Tag::OCTET_STRING, &ext_value);
        children.extend_from_slice(&payload);
        encode_sequence(&children)
    }

    fn key_usage_ext(bits: u16) -> Vec<u8> {
        let mut data = vec![0u8; 2];
        for index in 0..9 {
            if bits & (1u16 << index) != 0 {
                data[index / 8] |= 1 << (7 - (index % 8));
            }
        }
        let mut bit_payload = vec![7u8];
        bit_payload.extend_from_slice(&data);
        let mut children = encode_oid(&OID_KEY_USAGE).expect("oid");
        children.extend_from_slice(&encode_tlv(Tag::BOOLEAN, &[0xff]));
        let mut payload = Vec::new();
        write_tlv(
            &mut payload,
            Tag::OCTET_STRING,
            &encode_tlv(Tag::BIT_STRING, &bit_payload),
        );
        children.extend_from_slice(&payload);
        encode_sequence(&children)
    }

    fn cert(cn: &str, serial: u64, issuer_cn: &str, ca: bool, seed: u8) -> Certificate {
        let subject = name_der(cn);
        let issuer = if issuer_cn == cn {
            subject.clone()
        } else {
            name_der(issuer_cn)
        };
        let mut tbs = Vec::new();
        tbs.extend_from_slice(
            &crate::der::encode_context(0, true, &encode_integer(2)).expect("ctx0"),
        ); // v3
        tbs.extend_from_slice(&encode_integer(serial));
        tbs.extend_from_slice(&alg_id());
        tbs.extend_from_slice(&issuer);
        tbs.extend_from_slice(&validity());
        tbs.extend_from_slice(&subject);
        let mut spki = alg_id();
        spki.extend_from_slice(&bit_string(&[seed; 32]));
        tbs.extend_from_slice(&encode_sequence(&spki));
        let mut exts = Vec::new();
        exts.extend_from_slice(&basic_constraints_ext(ca));
        exts.extend_from_slice(&key_usage_ext(if ca {
            KeyUsage::KEY_CERT_SIGN.bits() | KeyUsage::CRL_SIGN.bits()
        } else {
            KeyUsage::DIGITAL_SIGNATURE.bits()
        }));
        tbs.extend_from_slice(
            &crate::der::encode_context(3, true, &encode_sequence(&exts)).expect("ctx3"),
        );
        let tbs = encode_sequence(&tbs);
        let mut out = tbs.clone();
        out.extend_from_slice(&alg_id());
        out.extend_from_slice(&bit_string(&crate::crypto::sha::sha256(&tbs)[..16]));
        Certificate::parse(&encode_sequence(&out)).expect("valid test certificate")
    }

    fn manager(now: u64) -> TrustManager {
        let root = cert("Root", 100, "Root", true, 1);
        let mut store = TrustStore::new();
        store.add_anchor(root);
        TrustManager::new(store, now)
    }

    fn chain() -> (Certificate, Certificate) {
        (
            cert("Leaf", 7, "Root", false, 2),
            cert("Root", 100, "Root", true, 1),
        )
    }

    /// Build a CRL signed with the `DigestVerifier` scheme (SHA-256/TBS[:16]).
    fn signed_crl(
        issuer_cn: &str,
        this_update: u64,
        next_update: u64,
        revoked: Vec<RevokedCertificate>,
    ) -> Crl {
        let crl =
            Crl::new(&name_der(issuer_cn), this_update, next_update, revoked).expect("base CRL");
        let signature = crate::crypto::sha::sha256(crl.tbs_der())[..16].to_vec();
        crl.with_signature(SignatureAlgorithm::Sha256WithRsa, signature)
    }

    fn root_crl(revoked: Vec<RevokedCertificate>) -> Crl {
        signed_crl("Root", 1_600_000_000, 1_700_000_000, revoked)
    }

    #[test]
    fn trust_anchor_match_yields_trusted_report() {
        let mgr = manager(1_650_000_000);
        let (leaf, _root) = chain();
        let report = mgr
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert!(report.is_trusted());
        assert_eq!(report.decision(), TrustDecision::Trusted);
        assert_eq!(report.chain_serials(), &[7, 100]);
        assert_eq!(report.anchor_serial(), Some(100));
        assert_eq!(report.evaluated_at(), 1_650_000_000);
        assert_eq!(report.revocation().len(), 1);
        assert_eq!(report.revocation()[0].serial(), 7);
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Unknown);
    }

    #[test]
    fn untrusted_chain_yields_untrusted_report() {
        let (leaf, _root) = chain();
        let empty = TrustStore::new();
        let mgr = TrustManager::new(empty, 1_650_000_000);
        let report = mgr
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert!(!report.is_trusted());
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::NoAnchor
            }
        );
        assert!(report.revocation().is_empty());
        assert_eq!(mgr.is_trusted(&leaf, &[], &[], &DigestVerifier), Ok(false));
    }

    #[test]
    fn wrong_issuer_yields_issuer_unknown_reason() {
        let mut store = TrustStore::new();
        store.add_anchor(cert("Other", 200, "Other", true, 9));
        let (leaf, _root) = chain();
        let mgr = TrustManager::new(store, 1_650_000_000);
        let report = mgr
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::IssuerUnknown
            }
        );
    }

    #[test]
    fn expired_certificate_is_reported_untrusted() {
        let mgr = manager(1_800_000_000);
        let (leaf, _root) = chain();
        let report = mgr
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::Expired
            }
        );
    }

    #[test]
    fn crl_revoked_certificate_is_detected() {
        let mgr = manager(1_650_000_000);
        let (leaf, _root) = chain();
        let crl = root_crl(vec![RevokedCertificate::new(
            7,
            1_640_000_000,
            RevocationReason::KeyCompromise,
        )]);
        let report = mgr
            .evaluate(&leaf, &[], &[crl], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::Revoked { serial: 7 }
            }
        );
        assert_eq!(
            report.revocation()[0].status(),
            RevocationStatus::Revoked {
                date: 1_640_000_000,
                reason: RevocationReason::KeyCompromise,
            }
        );
        assert_eq!(report.revocation()[0].crl_issuer_display(), Some("CN=Root"));
    }

    #[test]
    fn crl_valid_certificate_is_confirmed() {
        let mgr = manager(1_650_000_000);
        let (leaf, _root) = chain();
        let crl = root_crl(vec![RevokedCertificate::new(
            999,
            1_640_000_000,
            RevocationReason::Unspecified,
        )]);
        let report = mgr
            .evaluate(&leaf, &[], &[crl], &DigestVerifier)
            .expect("report");
        assert_eq!(report.decision(), TrustDecision::Trusted);
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Good);
        assert!(report.revocation()[0].status().is_good());
    }

    #[test]
    fn crl_unknown_and_stale_are_deterministic() {
        let (leaf, _root) = chain();
        // No matching CRL: Unknown evidence, permissive default stays trusted.
        let permissive = manager(1_650_000_000);
        let report = permissive
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Unknown);
        assert!(report.is_trusted());

        let strict = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let report = strict
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::RevocationUnknown { serial: 7 }
            }
        );

        // A stale CRL present in strict mode is a distinct typed reason.
        let stale = signed_crl("Root", 1_000_000, 1_100_000, Vec::new());
        let report = strict
            .evaluate(&leaf, &[], &[stale], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::RevocationStale { serial: 7 }
            }
        );
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Stale);
    }

    #[test]
    fn crl_der_round_trips_deterministically() {
        let crl = root_crl(vec![RevokedCertificate::new(
            7,
            1_640_000_000,
            RevocationReason::Superseded,
        )]);
        let der = crl.to_der();
        assert_eq!(crl.to_der(), der);
        let parsed = Crl::from_der(&der).expect("round trip");
        assert_eq!(parsed, crl);
        assert_eq!(parsed.revoked()[0].reason(), RevocationReason::Superseded);
    }

    #[test]
    fn crl_malformed_and_bad_window_are_typed_errors() {
        assert_eq!(
            Crl::from_der(&[0x30, 0x02, 0x05, 0x00]).map(|_| ()),
            Err(SecurityError::CrlMalformed)
        );
        assert_eq!(
            Crl::from_der(&[]).map(|_| ()),
            Err(SecurityError::DerTruncated)
        );
        assert_eq!(
            Crl::new(&name_der("Root"), 2_000, 1_000, Vec::new()).map(|_| ()),
            Err(SecurityError::CrlInvalidWindow)
        );
    }

    #[test]
    fn trust_report_is_deterministic() {
        let mgr = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let (leaf, _root) = chain();
        let crl = root_crl(vec![RevokedCertificate::new(
            999,
            1_640_000_000,
            RevocationReason::Unspecified,
        )]);
        let a = mgr
            .evaluate(&leaf, &[], std::slice::from_ref(&crl), &DigestVerifier)
            .expect("report");
        let b = mgr
            .evaluate(&leaf, &[], &[crl], &DigestVerifier)
            .expect("report");
        assert_eq!(a, b);
        assert_eq!(a.chain_serials(), b.chain_serials());
        assert_eq!(a.revocation(), b.revocation());
    }

    #[test]
    fn empty_store_yields_untrusted_no_anchor_report() {
        let (leaf, _root) = chain();
        let mgr = TrustManager::new(TrustStore::new(), 1_650_000_000);
        let report = mgr
            .evaluate(&leaf, &[], &[], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::NoAnchor
            }
        );
        assert!(!report.is_trusted());
        assert_eq!(report.chain_serials(), &[] as &[u64]);
        assert_eq!(report.anchor_serial(), None);
    }

    #[test]
    fn intermediate_chain_evidence_is_leaf_to_anchor() {
        let root = cert("Root", 100, "Root", true, 1);
        let intermediate = cert("Inter", 50, "Root", true, 3);
        let leaf = cert("Leaf", 7, "Inter", false, 2);
        let mut store = TrustStore::new();
        store.add_anchor(root);
        let mgr = TrustManager::new(store, 1_650_000_000);
        let crl_root = Crl::new(&name_der("Root"), 1_600_000_000, 1_700_000_000, Vec::new())
            .expect("root crl");
        let crl_inter = Crl::new(&name_der("Inter"), 1_600_000_000, 1_700_000_000, Vec::new())
            .expect("inter crl");
        let report = mgr
            .evaluate(
                &leaf,
                &[intermediate],
                &[crl_root, crl_inter],
                &DigestVerifier,
            )
            .expect("report");
        assert!(report.is_trusted());
        assert_eq!(report.chain_serials(), &[7, 50, 100]);
        assert_eq!(report.anchor_serial(), Some(100));
        assert_eq!(report.revocation().len(), 2);
        assert_eq!(report.revocation()[0].serial(), 7);
        assert_eq!(report.revocation()[1].serial(), 50);
    }

    #[test]
    fn bad_signature_maps_to_signature_invalid_reason() {
        let mgr = manager(1_650_000_000);
        let (leaf, _root) = chain();
        let report = mgr
            .evaluate(&leaf, &[], &[], &crate::x509::RejectingVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::SignatureInvalid
            }
        );
    }

    #[test]
    fn crl_signature_verifies_against_issuer() {
        let root = cert("Root", 100, "Root", true, 1);
        let crl = signed_crl("Root", 1_600_000_000, 1_700_000_000, Vec::new());
        assert!(crl.is_signed());
        assert_eq!(
            crl.signature_algorithm(),
            Some(&SignatureAlgorithm::Sha256WithRsa)
        );
        assert_eq!(crl.signature().len(), 16);
        assert!(
            crl.verify_signature(&root, &DigestVerifier)
                .expect("verify")
        );
    }

    #[test]
    fn crl_signature_rejects_invalid_signature() {
        let root = cert("Root", 100, "Root", true, 1);
        let crl = signed_crl("Root", 1_600_000_000, 1_700_000_000, Vec::new());
        let mut forged = crl.clone();
        let last = forged.signature.len() - 1;
        forged.signature[last] ^= 0x01;
        assert!(
            !forged
                .verify_signature(&root, &DigestVerifier)
                .expect("verify")
        );
        assert_eq!(
            forged.verify_signature(&root, &crate::x509::RejectingVerifier),
            Ok(false)
        );
    }

    #[test]
    fn crl_signature_rejects_wrong_issuer() {
        let other = cert("Other", 200, "Other", true, 9);
        let crl = signed_crl("Root", 1_600_000_000, 1_700_000_000, Vec::new());
        assert!(
            !crl.verify_signature(&other, &DigestVerifier)
                .expect("verify")
        );
    }

    #[test]
    fn crl_signature_requires_crl_sign_key_usage() {
        let subject = name_der("Root");
        let mut tbs = Vec::new();
        tbs.extend_from_slice(
            &crate::der::encode_context(0, true, &encode_integer(2)).expect("ctx0"),
        );
        tbs.extend_from_slice(&encode_integer(101));
        tbs.extend_from_slice(&alg_id());
        tbs.extend_from_slice(&subject);
        tbs.extend_from_slice(&validity());
        tbs.extend_from_slice(&subject);
        let mut spki = alg_id();
        spki.extend_from_slice(&bit_string(&[5u8; 32]));
        tbs.extend_from_slice(&encode_sequence(&spki));
        let mut exts = Vec::new();
        exts.extend_from_slice(&basic_constraints_ext(true));
        exts.extend_from_slice(&key_usage_ext(KeyUsage::KEY_CERT_SIGN.bits()));
        tbs.extend_from_slice(
            &crate::der::encode_context(3, true, &encode_sequence(&exts)).expect("ctx3"),
        );
        let tbs = encode_sequence(&tbs);
        let mut out = tbs.clone();
        out.extend_from_slice(&alg_id());
        out.extend_from_slice(&bit_string(&crate::crypto::sha::sha256(&tbs)[..16]));
        let no_crl_sign = Certificate::parse(&encode_sequence(&out)).expect("CA without cRLSign");

        let crl = signed_crl("Root", 1_600_000_000, 1_700_000_000, Vec::new());
        assert_eq!(
            crl.verify_signature(&no_crl_sign, &DigestVerifier),
            Err(SecurityError::CertificateKeyUsageViolation("cRLSign"))
        );
    }

    #[test]
    fn crl_signature_missing_is_typed_error() {
        let root = cert("Root", 100, "Root", true, 1);
        let unsigned = Crl::new(&name_der("Root"), 1_600_000_000, 1_700_000_000, Vec::new())
            .expect("unsigned CRL");
        assert!(!unsigned.is_signed());
        assert_eq!(
            unsigned.verify_signature(&root, &DigestVerifier),
            Err(SecurityError::CrlSignatureMissing)
        );
    }

    #[test]
    fn crl_signed_der_round_trips_and_preserves_signature() {
        let root = cert("Root", 100, "Root", true, 1);
        let crl = signed_crl(
            "Root",
            1_600_000_000,
            1_700_000_000,
            vec![RevokedCertificate::new(
                7,
                1_640_000_000,
                RevocationReason::KeyCompromise,
            )],
        );
        let der = crl.to_der();
        assert_eq!(crl.to_der(), der);
        let parsed = Crl::from_der(&der).expect("round trip");
        assert_eq!(parsed, crl);
        assert!(parsed.is_signed());
        assert!(
            parsed
                .verify_signature(&root, &DigestVerifier)
                .expect("verify")
        );
    }

    #[test]
    fn stale_this_update_is_reported_with_signed_crl() {
        let mgr = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let (leaf, _root) = chain();
        let stale = signed_crl("Root", 1_000_000, 1_100_000, Vec::new());
        let report = mgr
            .evaluate(&leaf, &[], &[stale], &DigestVerifier)
            .expect("report");
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Stale);
        assert!(report.revocation()[0].crl_signature_verified());
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::RevocationStale { serial: 7 }
            }
        );
    }

    #[test]
    fn evaluate_with_crl_signature_verification_accepts_signed_crl() {
        let mgr = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let (leaf, _root) = chain();
        let good = root_crl(vec![RevokedCertificate::new(
            999,
            1_640_000_000,
            RevocationReason::Unspecified,
        )]);
        let report = mgr
            .evaluate(&leaf, &[], &[good], &DigestVerifier)
            .expect("report");
        assert!(report.is_trusted());
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Good);
        assert!(report.revocation()[0].crl_signature_verified());

        let revoked = root_crl(vec![RevokedCertificate::new(
            7,
            1_640_000_000,
            RevocationReason::KeyCompromise,
        )]);
        let report = mgr
            .evaluate(&leaf, &[], &[revoked], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::Revoked { serial: 7 }
            }
        );
        assert!(report.revocation()[0].crl_signature_verified());
    }

    #[test]
    fn evaluate_with_crl_signature_verification_rejects_forged_crl() {
        let mgr = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let (leaf, _root) = chain();
        let mut forged = root_crl(Vec::new());
        let last = forged.signature.len() - 1;
        forged.signature[last] ^= 0x40;
        let report = mgr
            .evaluate(&leaf, &[], &[forged], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::CrlSignatureInvalid { serial: 7 }
            }
        );
        assert_eq!(report.revocation()[0].status(), RevocationStatus::Unknown);
        assert!(!report.revocation()[0].crl_signature_verified());
    }

    #[test]
    fn evaluate_with_crl_signature_verification_rejects_wrong_signer() {
        let mgr = manager(1_650_000_000).with_revocation_policy(RevocationPolicy::strict());
        let (leaf, _root) = chain();
        // CRL claims issuer "Root" but is signed with a different key.
        let base = Crl::new(&name_der("Root"), 1_600_000_000, 1_700_000_000, Vec::new())
            .expect("base CRL");
        let signature = crate::crypto::sha::sha256(base.tbs_der())[..16].to_vec();
        let mut wrong_signer = base.with_signature(SignatureAlgorithm::Sha256WithRsa, signature);
        wrong_signer.signature[0] ^= 0x7f;
        let report = mgr
            .evaluate(&leaf, &[], &[wrong_signer], &DigestVerifier)
            .expect("report");
        assert_eq!(
            report.decision(),
            TrustDecision::Untrusted {
                reason: TrustFailureReason::CrlSignatureInvalid { serial: 7 }
            }
        );
    }
}
