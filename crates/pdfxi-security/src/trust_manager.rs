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

use crate::der::{DerReader, Tag, encode_integer, encode_sequence, encode_time};
use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;
use crate::x509::name::Name;
use crate::x509::verify::{CertificateVerifier, TrustStore, ValidationPolicy, verify_chain};
use crate::x509::{Clock, FixedClock};

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
/// This is a clean-room *model*: a simplified, signed-field-free CRL carrying
/// the issuing CA name, an update window, and revoked serials. Callers supply
/// only CRLs they consider authentic; this crate does not fetch or verify CRL
/// signatures. The DER form is deterministic and round-trippable.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Crl {
    issuer: Name,
    issuer_der: Vec<u8>,
    this_update: u64,
    next_update: u64,
    revoked: Vec<RevokedCertificate>,
}

impl Crl {
    /// Build a CRL from a DER-encoded issuer `Name`, an update window, and
    /// revocation entries.
    ///
    /// Returns [`SecurityError::CrlInvalidWindow`] when `next_update` precedes
    /// `this_update`, and a typed DER error if `issuer_der` is not one
    /// well-formed `Name` value.
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
        Ok(Self {
            issuer,
            issuer_der: tlv.encoded().to_vec(),
            this_update,
            next_update,
            revoked,
        })
    }

    /// Parse a CRL from its canonical DER encoding (inverse of [`Self::to_der`]).
    ///
    /// Truncated or structurally invalid input yields a typed
    /// [`SecurityError::CrlMalformed`].
    pub fn from_der(der: &[u8]) -> Result<Self> {
        let mut top = DerReader::new(der);
        let outer = top.read_tlv()?;
        top.expect_end()?;
        let mut sequence = outer
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

        Ok(Self {
            issuer,
            issuer_der: issuer_tlv.encoded().to_vec(),
            this_update,
            next_update,
            revoked,
        })
    }

    /// Encode this CRL in the canonical DER form.
    pub fn to_der(&self) -> Vec<u8> {
        let mut revoked_der = Vec::new();
        for entry in &self.revoked {
            let mut item = encode_integer(entry.serial);
            if let Ok(time) = encode_time(entry.revocation_date) {
                item.extend_from_slice(&time);
            }
            item.extend_from_slice(&encode_integer(entry.reason.code()));
            revoked_der.extend_from_slice(&encode_sequence(&item));
        }
        let mut content = self.issuer_der.clone();
        if let Ok(time) = encode_time(self.this_update) {
            content.extend_from_slice(&time);
        }
        if let Ok(time) = encode_time(self.next_update) {
            content.extend_from_slice(&time);
        }
        content.extend_from_slice(&encode_sequence(&revoked_der));
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
}

impl RevocationPolicy {
    /// Conservative policy: presence and freshness are both required.
    pub const fn strict() -> Self {
        Self {
            require_present: true,
            require_fresh: true,
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
}

impl TrustFailureReason {
    fn from_security_error(error: &SecurityError) -> Self {
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
        // certificates are checked against CRLs.
        for certificate in chain.iter().take(chain.len().saturating_sub(1)) {
            let matching: Vec<&Crl> = crls
                .iter()
                .filter(|crl| crl.issuer() == certificate.issuer())
                .collect();
            let (status, display) =
                combined_status(&matching, certificate.serial_number(), self.now);
            revocation.push(RevocationEvidence {
                serial: certificate.serial_number(),
                status,
                crl_issuer_display: display,
            });
            if decision.is_trusted()
                && let Some(reason) =
                    reason_from_status(status, certificate.serial_number(), self.revocation_policy)
            {
                decision = TrustDecision::Untrusted { reason };
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

    fn root_crl(revoked: Vec<RevokedCertificate>) -> Crl {
        Crl::new(&name_der("Root"), 1_600_000_000, 1_700_000_000, revoked).expect("root CRL")
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
        let stale =
            Crl::new(&name_der("Root"), 1_000_000, 1_100_000, Vec::new()).expect("stale crl");
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
}
