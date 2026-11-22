//! `pdfxi trust verify`: offline X.509 chain and revocation evaluation.
//!
//! [`pdfxi_security::TrustManager`] builds a path from a leaf certificate to a
//! caller-supplied anchor, checks validity, CA constraints, key usage and every
//! signature with [`RsaPkcs1Verifier`], and evaluates caller-supplied CRLs for
//! revocation. This host loads the certificates from PEM or DER files, pins the
//! caller's clock, and reports the typed decision. Nothing is fetched over the
//! network.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use pdfxi_security::{
    Certificate, Crl, RevocationPolicy, RevocationStatus, RsaPkcs1Verifier, TrustFailureReason,
    TrustManager, TrustStore, certificates_from_pem,
};
use serde::Serialize;

/// JSON shape of one revocation row.
#[derive(Debug, Serialize)]
struct RevocationJson {
    /// Certificate serial.
    serial: u64,
    /// `good`, `revoked`, `unknown` or `stale`.
    status: String,
    /// Unix seconds at which the certificate was revoked.
    revoked_at: Option<u64>,
    /// Debug label of the recorded revocation reason.
    reason: Option<String>,
    /// RFC 4514 display of the matching CRL issuer.
    crl_issuer: Option<String>,
    /// Whether a matching CRL signature verified against the chain issuer.
    signature_verified: bool,
}

/// JSON shape of `pdfxi trust verify`.
#[derive(Debug, Serialize)]
struct TrustJson {
    /// Leaf certificate path.
    leaf: String,
    /// Leaf subject.
    subject: String,
    /// Leaf serial.
    serial: u64,
    /// Number of configured anchors.
    anchors: usize,
    /// Number of supplied intermediates.
    intermediates: usize,
    /// Number of supplied CRLs.
    crls: usize,
    /// Trust decision.
    trusted: bool,
    /// Stable failure code, when untrusted.
    reason: Option<String>,
    /// Validated serials in leaf-to-anchor order.
    chain_serials: Vec<u64>,
    /// Terminating anchor serial.
    anchor_serial: Option<u64>,
    /// Revocation evidence in leaf-to-anchor order.
    revocation: Vec<RevocationJson>,
    /// Pinned evaluation time.
    evaluated_at: u64,
    /// Whether the strict revocation policy was applied.
    strict: bool,
}

/// Stable code for a trust failure reason.
fn reason_code(reason: TrustFailureReason) -> String {
    match reason {
        TrustFailureReason::NoAnchor => "no_anchor".to_owned(),
        TrustFailureReason::IssuerUnknown => "issuer_unknown".to_owned(),
        TrustFailureReason::ChainTooLong => "chain_too_long".to_owned(),
        TrustFailureReason::SignatureInvalid => "signature_invalid".to_owned(),
        TrustFailureReason::Expired => "expired".to_owned(),
        TrustFailureReason::NotYetValid => "not_yet_valid".to_owned(),
        TrustFailureReason::NotCa => "not_ca".to_owned(),
        TrustFailureReason::KeyUsageViolation => "key_usage_violation".to_owned(),
        TrustFailureReason::EkuMissing => "eku_missing".to_owned(),
        TrustFailureReason::UnknownCriticalExtension => "unknown_critical_extension".to_owned(),
        TrustFailureReason::MalformedCertificate => "malformed_certificate".to_owned(),
        TrustFailureReason::Revoked { serial } => format!("revoked:{serial}"),
        TrustFailureReason::RevocationUnknown { serial } => {
            format!("revocation_unknown:{serial}")
        }
        TrustFailureReason::RevocationStale { serial } => format!("revocation_stale:{serial}"),
        TrustFailureReason::CrlSignatureInvalid { serial } => {
            format!("crl_signature_invalid:{serial}")
        }
    }
}

/// Load every certificate in a PEM bundle or one DER certificate.
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

/// Load one leaf certificate (exactly one certificate per file).
fn load_leaf(path: &Path) -> anyhow::Result<Certificate> {
    let mut certificates = load_certificates(path)?;
    match certificates.len() {
        1 => certificates
            .pop()
            .context("leaf certificate disappeared after load"),
        count => bail!(
            "{} contains {count} certificates; --leaf needs exactly one",
            path.display()
        ),
    }
}

/// Current Unix time.
fn now_unix() -> anyhow::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    Ok(duration.as_secs())
}

/// Verification report builder.
fn verify_chain(
    anchor: &Path,
    leaf: &Path,
    intermediates: &[PathBuf],
    crl_paths: &[PathBuf],
    time: Option<u64>,
    strict: bool,
) -> anyhow::Result<TrustJson> {
    let mut store = TrustStore::new();
    let anchor_certificates = load_certificates(anchor)?;
    for certificate in anchor_certificates {
        store.add_anchor(certificate);
    }
    if store.anchors().is_empty() {
        bail!("{} contains no trust anchors", anchor.display());
    }
    let leaf_certificate = load_leaf(leaf)?;
    let mut untrusted = Vec::new();
    for path in intermediates {
        untrusted.extend(load_certificates(path)?);
    }
    let mut crls = Vec::with_capacity(crl_paths.len());
    for path in crl_paths {
        let bytes = std::fs::read(path).with_context(|| format!("read {}", path.display()))?;
        let crl = Crl::from_der(&bytes)
            .map_err(|error| anyhow::anyhow!("parse CRL {}: {error}", path.display()))?;
        crls.push(crl);
    }
    let evaluated_at = time.unwrap_or(now_unix()?);
    let manager =
        TrustManager::new(store.clone(), evaluated_at).with_revocation_policy(if strict {
            RevocationPolicy::strict()
        } else {
            RevocationPolicy::default()
        });
    let report = manager
        .evaluate(&leaf_certificate, &untrusted, &crls, &RsaPkcs1Verifier)
        .map_err(|error| anyhow::anyhow!("trust evaluation: {error}"))?;
    let revocation = report
        .revocation()
        .iter()
        .map(|evidence| {
            let (status, revoked_at, reason) = match evidence.status() {
                RevocationStatus::Good => ("good".to_owned(), None, None),
                RevocationStatus::Revoked { date, reason } => (
                    "revoked".to_owned(),
                    Some(date),
                    Some(format!("{reason:?}")),
                ),
                RevocationStatus::Unknown => ("unknown".to_owned(), None, None),
                RevocationStatus::Stale => ("stale".to_owned(), None, None),
            };
            RevocationJson {
                serial: evidence.serial(),
                status,
                revoked_at,
                reason,
                crl_issuer: evidence.crl_issuer_display().map(str::to_owned),
                signature_verified: evidence.crl_signature_verified(),
            }
        })
        .collect();
    Ok(TrustJson {
        leaf: leaf.display().to_string(),
        subject: leaf_certificate.subject().display_rfc4514(),
        serial: leaf_certificate.serial_number(),
        anchors: manager.store().anchors().len(),
        intermediates: untrusted.len(),
        crls: crls.len(),
        trusted: report.is_trusted(),
        reason: match report.decision() {
            pdfxi_security::TrustDecision::Trusted => None,
            pdfxi_security::TrustDecision::Untrusted { reason } => Some(reason_code(reason)),
        },
        chain_serials: report.chain_serials().to_vec(),
        anchor_serial: report.anchor_serial(),
        revocation,
        evaluated_at: report.evaluated_at(),
        strict,
    })
}

/// `pdfxi trust verify`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn verify(
    anchor: &Path,
    leaf: &Path,
    intermediates: &[PathBuf],
    crls: &[PathBuf],
    time: Option<u64>,
    strict: bool,
    json: bool,
) -> anyhow::Result<()> {
    let report = verify_chain(anchor, leaf, intermediates, crls, time, strict)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "leaf: {}  serial: {}  anchors: {}  intermediates: {}  crls: {}",
            report.subject, report.serial, report.anchors, report.intermediates, report.crls
        );
        println!(
            "trusted: {}  reason: {}",
            report.trusted,
            report.reason.as_deref().unwrap_or("none")
        );
        println!(
            "chain: {:?}  anchor: {}",
            report.chain_serials,
            report
                .anchor_serial
                .map(|serial| serial.to_string())
                .unwrap_or_else(|| "none".to_owned())
        );
        if report.revocation.is_empty() {
            println!("revocation: none evaluated");
        } else {
            for row in &report.revocation {
                println!(
                    "revocation: serial {} {} (crl-issuer {}, signature-verified {})",
                    row.serial,
                    row.status,
                    row.crl_issuer.as_deref().unwrap_or("none"),
                    row.signature_verified
                );
            }
        }
        println!(
            "evaluated-at: {}  strict: {}",
            report.evaluated_at, report.strict
        );
    }
    if !report.trusted {
        bail!(
            "certificate chain is not trusted: {}",
            report.reason.as_deref().unwrap_or("unknown")
        );
    }
    Ok(())
}
