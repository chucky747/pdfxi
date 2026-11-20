//! Timestamp-authority token model and detached-signature validation wiring (SEC-006).
//!
//! The existing [`crate::timestamp`] module validates a complete RFC 3161 CMS
//! token, including its signature, chain and EKU. This module complements it
//! with two smaller, host-facing building blocks that share the same trust
//! boundary:
//!
//! - [`TsaTokenModel`] — a caller-supplied TSA certificate plus the fields that
//!   matter to a relying party (message imprint, nonce, policy OID, generation
//!   time). It verifies the imprint against supplied signed bytes, checks the
//!   nonce and the certificate validity window at a *caller-supplied* time, and
//!   records the policy OID in a deterministic [`TsaValidationReport`].
//! - [`validate_detached_signature_against_certificate`] — wiring that checks a
//!   detached CMS signature against a caller-supplied signer certificate and
//!   the caller's content, then reports a conservative [`SignatureStatus`].
//!
//! No network, filesystem, wall-clock, or hidden trust store is used. Every
//! certificate, clock value, policy and key boundary is supplied by the caller.
//! The [`SignatureProvider`] trait keeps signing keys out of this crate.

use crate::cms::{SignatureProvider, digest, verify_signed_data_detached};
use crate::crypto::constant_time_eq;
use crate::error::{Result, SecurityError};
use crate::signature::{DigestAlgorithm, SignatureStatus};
use crate::x509::certificate::{Certificate, ValidityStatus};

fn oid_string(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// A caller-supplied timestamp-authority token model.
///
/// The certificate, imprint, nonce, policy and time are all provided by the
/// caller; nothing here is fetched or read from the host. Construction is
/// strict: malformed certificates, an imprint of the wrong length for its
/// digest algorithm, or an empty policy OID are typed errors.
#[derive(Clone, PartialEq, Eq)]
pub struct TsaTokenModel {
    certificate: Certificate,
    imprint_algorithm: DigestAlgorithm,
    imprint: Vec<u8>,
    nonce: Option<u64>,
    policy_oid: Vec<u32>,
    gen_time: u64,
}

impl TsaTokenModel {
    /// Build a token model from a parsed certificate and an explicit imprint.
    ///
    /// The imprint length must equal the digest algorithm's output length and
    /// the policy OID must be non-empty. No secret material is accepted.
    pub fn new(
        certificate_der: &[u8],
        imprint_algorithm: DigestAlgorithm,
        imprint: Vec<u8>,
        nonce: Option<u64>,
        policy_oid: Vec<u32>,
        gen_time: u64,
    ) -> Result<Self> {
        if imprint.len() != imprint_algorithm.output_len_bytes() {
            return Err(SecurityError::TsaTokenInvalid("message_imprint"));
        }
        if policy_oid.is_empty() {
            return Err(SecurityError::TsaTokenInvalid("policy"));
        }
        let certificate = Certificate::parse(certificate_der)?;
        Ok(Self {
            certificate,
            imprint_algorithm,
            imprint,
            nonce,
            policy_oid,
            gen_time,
        })
    }

    /// Build a token model whose imprint is the digest of `signed_bytes`.
    ///
    /// This is the convenient path when the caller already holds the bytes a
    /// timestamp was requested over. SHA-1 is rejected by the shared digest
    /// helper with a typed error.
    pub fn from_signed_bytes(
        certificate_der: &[u8],
        signed_bytes: &[u8],
        imprint_algorithm: DigestAlgorithm,
        nonce: Option<u64>,
        policy_oid: Vec<u32>,
        gen_time: u64,
    ) -> Result<Self> {
        let imprint = digest(imprint_algorithm, signed_bytes)?;
        Self::new(
            certificate_der,
            imprint_algorithm,
            imprint,
            nonce,
            policy_oid,
            gen_time,
        )
    }

    /// Return the caller-supplied TSA certificate.
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// Return the imprint's digest algorithm.
    pub const fn imprint_algorithm(&self) -> DigestAlgorithm {
        self.imprint_algorithm
    }

    /// Return the message-imprint bytes.
    pub fn imprint(&self) -> &[u8] {
        &self.imprint
    }

    /// Return the optional nonce.
    pub const fn nonce(&self) -> Option<u64> {
        self.nonce
    }

    /// Return the policy OID arcs.
    pub fn policy_oid(&self) -> &[u32] {
        &self.policy_oid
    }

    /// Return the generation time as Unix seconds.
    pub const fn gen_time(&self) -> u64 {
        self.gen_time
    }
}

impl std::fmt::Debug for TsaTokenModel {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TsaTokenModel")
            .field("subject", &self.certificate.subject().display_rfc4514())
            .field("imprint_algorithm", &self.imprint_algorithm)
            .field("nonce", &self.nonce)
            .field("policy_oid", &oid_string(&self.policy_oid))
            .field("gen_time", &self.gen_time)
            .finish()
    }
}

/// Distinct outcomes of a [`TsaTokenModel`] validation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum TsaStatus {
    /// Every model-level check succeeded.
    Valid,
    /// The imprint did not match the supplied signed bytes.
    ImprintMismatch,
    /// The token nonce did not match the caller's expected nonce.
    NonceMismatch,
    /// The TSA certificate had expired at the caller-supplied time.
    CertificateExpired,
    /// The TSA certificate was not yet valid at the caller-supplied time.
    CertificateNotYetValid,
    /// The token's policy OID was not in the caller's accepted set.
    UnknownPolicy,
}

impl TsaStatus {
    /// Short, stable label for this status.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Valid => "valid",
            Self::ImprintMismatch => "imprint-mismatch",
            Self::NonceMismatch => "nonce-mismatch",
            Self::CertificateExpired => "certificate-expired",
            Self::CertificateNotYetValid => "certificate-not-yet-valid",
            Self::UnknownPolicy => "unknown-policy",
        }
    }

    /// Return `true` only for [`Self::Valid`].
    pub const fn is_valid(self) -> bool {
        matches!(self, Self::Valid)
    }
}

impl std::fmt::Display for TsaStatus {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.label())
    }
}

/// A deterministic, non-secret report produced by [`validate_tsa_token`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TsaValidationReport {
    status: TsaStatus,
    signer: String,
    policy_oid: String,
    gen_time: u64,
    imprint_algorithm: DigestAlgorithm,
    nonce_checked: bool,
    certificate_validity: ValidityStatus,
}

impl TsaValidationReport {
    /// Return the specific outcome.
    pub const fn status(&self) -> TsaStatus {
        self.status
    }

    /// Return `true` only when every check passed.
    pub const fn is_valid(&self) -> bool {
        self.status.is_valid()
    }

    /// Return the TSA certificate subject display string.
    pub fn signer(&self) -> &str {
        &self.signer
    }

    /// Return the recorded policy OID as a dotted string.
    pub fn policy_oid(&self) -> &str {
        &self.policy_oid
    }

    /// Return the token's generation time (Unix seconds).
    pub const fn gen_time(&self) -> u64 {
        self.gen_time
    }

    /// Return the imprint digest algorithm.
    pub const fn imprint_algorithm(&self) -> DigestAlgorithm {
        self.imprint_algorithm
    }

    /// Return whether a nonce check was requested.
    pub const fn nonce_checked(&self) -> bool {
        self.nonce_checked
    }

    /// Return the TSA certificate validity at the caller-supplied time.
    pub const fn certificate_validity(&self) -> ValidityStatus {
        self.certificate_validity
    }

    /// Map this report conservatively onto [`SignatureStatus`].
    ///
    /// Only [`TsaStatus::Valid`] becomes [`SignatureStatus::Valid`]. An imprint
    /// mismatch is tampering, a nonce failure is invalid, a certificate-window
    /// failure is expired, and an unknown policy is unknown trust.
    pub const fn to_signature_status(&self) -> SignatureStatus {
        match self.status {
            TsaStatus::Valid => SignatureStatus::Valid,
            TsaStatus::ImprintMismatch => SignatureStatus::Tampered,
            TsaStatus::NonceMismatch => SignatureStatus::Invalid,
            TsaStatus::CertificateExpired | TsaStatus::CertificateNotYetValid => {
                SignatureStatus::Expired
            }
            TsaStatus::UnknownPolicy => SignatureStatus::Unknown,
        }
    }
}

/// Validate a token model against supplied signed bytes, time and policy.
///
/// Checks run in a fixed order — imprint, nonce, certificate window, then
/// policy — and the first failure determines the status. Every observed
/// dimension is still recorded in the returned report so callers can audit a
/// failure without re-running the checks. Structural problems are typed errors;
/// semantic failures are reported through [`TsaValidationReport::status`].
pub fn validate_tsa_token(
    model: &TsaTokenModel,
    signed_bytes: &[u8],
    expected_nonce: Option<u64>,
    accepted_policies: Option<&[Vec<u32>]>,
    now: u64,
) -> Result<TsaValidationReport> {
    let computed = digest(model.imprint_algorithm, signed_bytes)?;

    let mut status = if constant_time_eq(&computed, &model.imprint) {
        TsaStatus::Valid
    } else {
        TsaStatus::ImprintMismatch
    };

    if status.is_valid() && expected_nonce.is_some_and(|expected| model.nonce != Some(expected)) {
        status = TsaStatus::NonceMismatch;
    }

    let certificate_validity = model.certificate.validity_status_at(now);
    if status.is_valid() {
        status = match certificate_validity {
            ValidityStatus::Valid => TsaStatus::Valid,
            ValidityStatus::Expired => TsaStatus::CertificateExpired,
            ValidityStatus::NotYetValid => TsaStatus::CertificateNotYetValid,
        };
    }

    if status.is_valid()
        && let Some(accepted) = accepted_policies
        && !accepted
            .iter()
            .any(|oid| oid.as_slice() == model.policy_oid.as_slice())
    {
        status = TsaStatus::UnknownPolicy;
    }

    Ok(TsaValidationReport {
        status,
        signer: model.certificate.subject().display_rfc4514(),
        policy_oid: oid_string(&model.policy_oid),
        gen_time: model.gen_time,
        imprint_algorithm: model.imprint_algorithm,
        nonce_checked: expected_nonce.is_some(),
        certificate_validity,
    })
}

/// A conservative report for a detached CMS signature checked against a
/// caller-supplied signer certificate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DetachedSignatureReport {
    signer: String,
    signed_at: u64,
    digest_algorithm: DigestAlgorithm,
    validity: SignatureStatus,
    certificate_validity: ValidityStatus,
}

impl DetachedSignatureReport {
    /// Return the signer display name reported by the provider.
    pub fn signer(&self) -> &str {
        &self.signer
    }

    /// Return the signature's self-asserted signing time (Unix seconds).
    pub const fn signed_at(&self) -> u64 {
        self.signed_at
    }

    /// Return the digest algorithm used by the signature.
    pub const fn digest_algorithm(&self) -> DigestAlgorithm {
        self.digest_algorithm
    }

    /// Return the conservative signature validity.
    pub const fn validity(&self) -> SignatureStatus {
        self.validity
    }

    /// Return the signer certificate validity at the caller-supplied time.
    pub const fn certificate_validity(&self) -> ValidityStatus {
        self.certificate_validity
    }

    /// Return `true` only when the signature is cryptographically valid.
    pub const fn is_valid(&self) -> bool {
        self.validity.is_valid()
    }
}

/// Check a detached CMS signature against caller-supplied content and a
/// caller-supplied signer certificate.
///
/// The provider's embedded certificate must equal `certificate_der` (otherwise
/// [`SecurityError::DetachedSignerMismatch`]); the certificate is parsed and its
/// window classified at `now`; then [`verify_signed_data_detached`] recomputes
/// the content digest and asks the provider to verify the signature. A
/// cryptographic failure is a typed error from the CMS layer. The resulting
/// report is conservative: the CMS layer's `Valid` status is never promoted
/// when the signer certificate is outside its validity window — in that case
/// [`DetachedSignatureReport::validity`] is [`SignatureStatus::Expired`].
pub fn validate_detached_signature_against_certificate(
    cms_der: &[u8],
    content: &[u8],
    certificate_der: &[u8],
    provider: &dyn SignatureProvider,
    now: u64,
) -> Result<DetachedSignatureReport> {
    if !constant_time_eq(provider.certificate_der(), certificate_der) {
        return Err(SecurityError::DetachedSignerMismatch);
    }
    let certificate = Certificate::parse(certificate_der)?;
    let certificate_validity = certificate.validity_status_at(now);
    let info = verify_signed_data_detached(cms_der, content, provider)?;
    let validity = if certificate_validity.is_valid() {
        info.validity()
    } else {
        SignatureStatus::Expired
    };
    Ok(DetachedSignatureReport {
        signer: info.signer().to_owned(),
        signed_at: info.signed_at(),
        digest_algorithm: info.digest_algorithm(),
        validity,
        certificate_validity,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::cms::{InMemorySignatureProvider, SignerIdentifier, build_signed_data};
    use crate::der::{
        Tag, encode_integer, encode_null, encode_oid, encode_sequence, encode_set, encode_time,
        write_tlv,
    };
    use crate::x509::OID_SHA256_WITH_RSA;

    const OID_CN: [u32; 4] = [2, 5, 4, 3];
    const OID_RSA_ENCRYPTION: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 1];
    const OID_POLICY: [u32; 5] = [1, 2, 3, 4, 5];
    const OID_OTHER_POLICY: [u32; 5] = [9, 8, 7, 6, 5];
    const NOW: u64 = 1_650_000_000;
    const DATA: &[u8] = b"document bytes covered by the timestamp";

    fn name(cn: &str) -> Vec<u8> {
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        encode_sequence(&encode_set(&encode_sequence(&ava)))
    }

    fn cert_der(cn: &str, not_before: u64, not_after: u64) -> (Vec<u8>, Vec<u8>) {
        let issuer = name(cn);
        let mut validity = encode_time(not_before).expect("notBefore");
        validity.extend_from_slice(&encode_time(not_after).expect("notAfter"));
        let mut spki_children = encode_sequence(&{
            let mut alg = encode_oid(&OID_RSA_ENCRYPTION).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        });
        let mut bit = vec![0u8];
        bit.extend_from_slice(&[7u8; 32]);
        spki_children.extend_from_slice(&crate::der::encode_tlv(Tag::BIT_STRING, &bit));
        let spki = encode_sequence(&spki_children);
        let mut tbs = Vec::new();
        tbs.extend_from_slice(&encode_integer(7));
        tbs.extend_from_slice(&encode_sequence(&{
            let mut alg = encode_oid(&OID_SHA256_WITH_RSA).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        }));
        tbs.extend_from_slice(&issuer);
        tbs.extend_from_slice(&encode_sequence(&validity));
        tbs.extend_from_slice(&issuer);
        tbs.extend_from_slice(&spki);
        let tbs = encode_sequence(&tbs);
        let signature = crate::crypto::sha::sha256(&tbs);
        let mut outer = tbs;
        outer.extend_from_slice(&encode_sequence(&{
            let mut alg = encode_oid(&OID_SHA256_WITH_RSA).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        }));
        outer.extend_from_slice(&crate::der::encode_tlv(Tag::BIT_STRING, &{
            let mut bits = vec![0u8];
            bits.extend_from_slice(&signature[..16]);
            bits
        }));
        (encode_sequence(&outer), issuer)
    }

    fn valid_model() -> (TsaTokenModel, Vec<u8>) {
        let (cert, _) = cert_der("TSA", NOW - 100, NOW + 100);
        let model = TsaTokenModel::from_signed_bytes(
            &cert,
            DATA,
            DigestAlgorithm::Sha256,
            Some(42),
            OID_POLICY.to_vec(),
            NOW,
        )
        .expect("model");
        (model, cert)
    }

    #[test]
    fn tsa_token_valid_report_is_deterministic() {
        let (model, _) = valid_model();
        let accepted = [OID_POLICY.to_vec()];
        let report =
            validate_tsa_token(&model, DATA, Some(42), Some(&accepted), NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::Valid);
        assert_eq!(
            report,
            validate_tsa_token(&model, DATA, Some(42), Some(&accepted), NOW).expect("report")
        );
        assert_eq!(report.to_signature_status(), SignatureStatus::Valid);
        assert_eq!(report.policy_oid(), "1.2.3.4.5");
        assert!(report.nonce_checked());
        assert_eq!(report.certificate_validity(), ValidityStatus::Valid);
    }

    #[test]
    fn imprint_mismatch_is_reported() {
        let (model, _) = valid_model();
        let report =
            validate_tsa_token(&model, b"different bytes", Some(42), None, NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::ImprintMismatch);
        assert_eq!(report.to_signature_status(), SignatureStatus::Tampered);
    }

    #[test]
    fn nonce_mismatch_is_reported() {
        let (model, _) = valid_model();
        let report = validate_tsa_token(&model, DATA, Some(99), None, NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::NonceMismatch);
        assert_eq!(report.to_signature_status(), SignatureStatus::Invalid);
    }

    #[test]
    fn expired_tsa_certificate_is_reported() {
        let (cert, _) = cert_der("TSA", NOW - 100, NOW - 1);
        let model = TsaTokenModel::from_signed_bytes(
            &cert,
            DATA,
            DigestAlgorithm::Sha256,
            None,
            OID_POLICY.to_vec(),
            NOW - 2,
        )
        .expect("model");
        let report = validate_tsa_token(&model, DATA, None, None, NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::CertificateExpired);
        assert_eq!(report.certificate_validity(), ValidityStatus::Expired);
        assert_eq!(report.to_signature_status(), SignatureStatus::Expired);
    }

    #[test]
    fn not_yet_valid_tsa_certificate_is_reported() {
        let (cert, _) = cert_der("TSA", NOW + 10, NOW + 100);
        let model = TsaTokenModel::from_signed_bytes(
            &cert,
            DATA,
            DigestAlgorithm::Sha256,
            None,
            OID_POLICY.to_vec(),
            NOW + 11,
        )
        .expect("model");
        let report = validate_tsa_token(&model, DATA, None, None, NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::CertificateNotYetValid);
    }

    #[test]
    fn unknown_policy_is_recorded() {
        let (model, _) = valid_model();
        let accepted = [OID_OTHER_POLICY.to_vec()];
        let report =
            validate_tsa_token(&model, DATA, Some(42), Some(&accepted), NOW).expect("report");
        assert_eq!(report.status(), TsaStatus::UnknownPolicy);
        assert_eq!(report.policy_oid(), "1.2.3.4.5");
        assert_eq!(report.to_signature_status(), SignatureStatus::Unknown);
    }

    #[test]
    fn tsa_token_invalid_fields_are_typed() {
        let (cert, _) = cert_der("TSA", NOW - 100, NOW + 100);
        assert_eq!(
            TsaTokenModel::new(
                &cert,
                DigestAlgorithm::Sha256,
                vec![0u8; 3],
                None,
                OID_POLICY.to_vec(),
                NOW,
            ),
            Err(SecurityError::TsaTokenInvalid("message_imprint"))
        );
        assert_eq!(
            TsaTokenModel::new(
                &cert,
                DigestAlgorithm::Sha256,
                vec![0u8; 32],
                None,
                Vec::new(),
                NOW,
            ),
            Err(SecurityError::TsaTokenInvalid("policy"))
        );
    }

    fn provider_for(cert: Vec<u8>, issuer: Vec<u8>) -> crate::InMemorySignatureProvider {
        let signer = SignerIdentifier::new(issuer, 7).expect("signer");
        InMemorySignatureProvider::new(signer, cert, "TSA signer").expect("provider")
    }

    #[test]
    fn detached_signature_verify_success_and_failure() {
        let (cert, issuer) = cert_der("TSA", NOW - 100, NOW + 100);
        let provider = provider_for(cert.clone(), issuer);
        let params = crate::cms::SignedDataParams {
            content: DATA.to_vec(),
            digest_algorithm: DigestAlgorithm::Sha256,
            signer: provider.signer_identifier().clone(),
            certificate_der: cert.clone(),
            signing_time: NOW,
        };
        let cms = build_signed_data(&params, &provider).expect("cms");
        let report =
            validate_detached_signature_against_certificate(&cms, DATA, &cert, &provider, NOW)
                .expect("report");
        assert!(report.is_valid());
        assert_eq!(report.validity(), SignatureStatus::Valid);
        assert_eq!(report.signer(), "TSA signer");

        // Wrong content fails the messageDigest check as a typed CMS error.
        assert_eq!(
            validate_detached_signature_against_certificate(
                &cms,
                b"tampered content",
                &cert,
                &provider,
                NOW,
            )
            .map(|_| ()),
            Err(SecurityError::TamperedContent)
        );

        // A different certificate than the provider's is a typed mismatch.
        let (other, _) = cert_der("Other", NOW - 100, NOW + 100);
        assert_eq!(
            validate_detached_signature_against_certificate(&cms, DATA, &other, &provider, NOW)
                .map(|_| ()),
            Err(SecurityError::DetachedSignerMismatch)
        );
    }

    #[test]
    fn detached_signature_report_respects_certificate_window() {
        let (cert, issuer) = cert_der("TSA", NOW - 100, NOW + 100);
        let provider = provider_for(cert.clone(), issuer);
        let params = crate::cms::SignedDataParams {
            content: DATA.to_vec(),
            digest_algorithm: DigestAlgorithm::Sha256,
            signer: provider.signer_identifier().clone(),
            certificate_der: cert.clone(),
            signing_time: NOW,
        };
        let cms = build_signed_data(&params, &provider).expect("cms");
        let report = validate_detached_signature_against_certificate(
            &cms,
            DATA,
            &cert,
            &provider,
            NOW + 1_000,
        )
        .expect("report");
        assert_eq!(report.certificate_validity(), ValidityStatus::Expired);
        assert_eq!(report.validity(), SignatureStatus::Expired);
        assert!(!report.is_valid());
    }
}
