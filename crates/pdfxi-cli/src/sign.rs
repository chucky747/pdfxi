//! `pdfxi sign`: real CMS/RSA signatures over a PDF byte range.
//!
//! [`pdfxi_security::pdf_signature`] owns the cryptographic signature: a CMS
//! `SignedData` blob with a SHA-256 digest and a PKCS#1 v1.5 RSA signature,
//! written into an incremental revision as `adbe.pkcs7.detached`. This host
//! loads a signer identity from PEM or PKCS#12, signs a real file, verifies its
//! own output before writing, and exposes the verifier separately. Key material
//! and passwords are never printed or included in an error.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_security::{
    SignOptions, SignatureVerification, SigningIdentity, sign_pdf, verify_signature,
};
use serde::Serialize;

/// JSON shape of `pdfxi sign create`.
#[derive(Serialize)]
struct SignReport {
    /// Written file.
    out: String,
    /// Output bytes.
    bytes: usize,
    /// Signature field name.
    field: String,
    /// Signer certificate subject.
    signer: String,
    /// Certificates in the supplied chain.
    chain_len: usize,
    /// One-based page that received the invisible widget.
    page: usize,
    /// Digest algorithm.
    digest: String,
    /// Exact DER CMS length.
    cms_bytes: usize,
    /// Reserved `/Contents` size.
    contents_reservation: usize,
    /// `/ByteRange` array.
    byte_range: [usize; 4],
    /// Bytes appended after the original document.
    appended_bytes: usize,
    /// Self-asserted signing time.
    signing_time: u64,
    /// Whether the fresh output passes the full verification badge.
    fully_valid: bool,
}

/// JSON shape of `pdfxi sign verify`.
#[derive(Serialize)]
struct VerifyReport {
    /// Verified file.
    file: String,
    /// Cryptographic status label.
    status: String,
    /// Full validity badge: signature, covered bytes and whole-file coverage.
    fully_valid: bool,
    /// RSA signature verifies.
    signature_valid: bool,
    /// Covered bytes match the signed digest.
    bytes_match: bool,
    /// The signature covers the whole file.
    covers_entire_file: bool,
    /// Bytes follow the signature.
    appended_after_signature: bool,
    /// Signer certificate subject.
    signer: String,
    /// SHA-256 fingerprint of the signer certificate.
    fingerprint_sha256: String,
    /// Digest algorithm.
    digest: String,
    /// `/SubFilter`.
    sub_filter: String,
    /// `/ByteRange` array.
    byte_range: [usize; 4],
    /// Self-asserted signing time.
    signing_time: Option<u64>,
    /// Trust-store decision: `None` means not evaluated.
    chain_trusted: Option<bool>,
}

/// Load an identity from `--key`/`--cert`/`--chain` or from `--pfx`.
fn load_identity(
    key: Option<&Path>,
    cert: Option<&Path>,
    chain: Option<&Path>,
    pfx: Option<&Path>,
    password: &str,
) -> anyhow::Result<SigningIdentity> {
    match (pfx, key, cert) {
        (Some(pfx), None, None) => {
            let bytes =
                std::fs::read(pfx).with_context(|| format!("read PKCS#12 {}", pfx.display()))?;
            SigningIdentity::from_pfx(&bytes, password)
                .map_err(|error| anyhow::anyhow!("load PKCS#12 {}: {error}", pfx.display()))
        }
        (None, Some(key), Some(cert)) => {
            let key_pem = std::fs::read_to_string(key)
                .with_context(|| format!("read private key {}", key.display()))?;
            let mut chain_pem = std::fs::read_to_string(cert)
                .with_context(|| format!("read certificate {}", cert.display()))?;
            if let Some(chain) = chain {
                let extra = std::fs::read_to_string(chain)
                    .with_context(|| format!("read certificate chain {}", chain.display()))?;
                chain_pem.push('\n');
                chain_pem.push_str(&extra);
            }
            SigningIdentity::from_pem(&key_pem, &chain_pem)
                .map_err(|error| anyhow::anyhow!("load PEM identity: {error}"))
        }
        (None, None, None) => {
            bail!("supply an identity with --key/--cert or with --pfx")
        }
        _ => bail!("--pfx cannot be combined with --key/--cert/--chain"),
    }
}

/// Build the JSON verification report.
fn verification_report(file: &Path, verification: &SignatureVerification) -> VerifyReport {
    VerifyReport {
        file: file.display().to_string(),
        status: verification.status().label().to_owned(),
        fully_valid: verification.is_fully_valid(),
        signature_valid: verification.signature_valid(),
        bytes_match: verification.bytes_match(),
        covers_entire_file: verification.covers_entire_file(),
        appended_after_signature: verification.appended_after_signature(),
        signer: verification.signer_subject().to_owned(),
        fingerprint_sha256: verification.signer_fingerprint_hex(),
        digest: verification.digest_algorithm().as_id().to_owned(),
        sub_filter: verification.sub_filter().to_owned(),
        byte_range: verification.byte_range(),
        signing_time: verification.signing_time_unix(),
        chain_trusted: verification.certificate_chain_trusted(),
    }
}

/// `pdfxi sign create`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn create(
    file: &Path,
    out: &Path,
    key: Option<&Path>,
    cert: Option<&Path>,
    chain: Option<&Path>,
    pfx: Option<&Path>,
    password: &str,
    page: usize,
    field: &str,
    reason: Option<&str>,
    location: Option<&str>,
    time: Option<u64>,
    reserve: usize,
    json: bool,
) -> anyhow::Result<()> {
    if page == 0 {
        bail!("--page is one-based and must be at least 1");
    }
    let identity = load_identity(key, cert, chain, pfx, password)?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let mut document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages in {}: {error}", file.display()))?;
    if page > pages {
        bail!(
            "--page {page} is outside {} (the document has {pages} page(s))",
            file.display()
        );
    }
    let options = SignOptions {
        page_index: page - 1,
        field_name: field.to_owned(),
        reason: reason.map(str::to_owned),
        location: location.map(str::to_owned),
        signing_time_unix: time,
        contents_reservation: reserve,
    };
    let signed = sign_pdf(&mut document, &identity, &options)
        .map_err(|error| anyhow::anyhow!("sign {}: {error}", file.display()))?;
    // Fail closed: the bytes we are about to write must pass the verifier.
    let verification = verify_signature(signed.bytes())
        .map_err(|error| anyhow::anyhow!("verify fresh signature: {error}"))?;
    if !verification.is_fully_valid() {
        bail!(
            "fresh signature did not verify: status {} (signature {}, digest {}, whole-file {})",
            verification.status().label(),
            verification.signature_valid(),
            verification.bytes_match(),
            verification.covers_entire_file()
        );
    }
    let report_data = signed.report();
    std::fs::write(out, signed.bytes()).with_context(|| format!("write {}", out.display()))?;
    let report = SignReport {
        out: out.display().to_string(),
        bytes: signed.bytes().len(),
        field: report_data.field_name().to_owned(),
        signer: report_data.signer_subject().to_owned(),
        chain_len: identity.chain_len(),
        page,
        digest: report_data.digest_algorithm().as_id().to_owned(),
        cms_bytes: report_data.cms_len(),
        contents_reservation: report_data.contents_reservation(),
        byte_range: report_data.byte_range(),
        appended_bytes: report_data.appended_bytes(),
        signing_time: report_data.signing_time_unix(),
        fully_valid: verification.is_fully_valid(),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  field: {}  page: {}",
        report.out, report.bytes, report.field, report.page
    );
    println!(
        "signer: {}  chain: {}  digest: {}  cms: {} bytes",
        report.signer, report.chain_len, report.digest, report.cms_bytes
    );
    println!(
        "byte-range: [{}, {}, {}, {}]  appended: {}  fully-valid: {}",
        report.byte_range[0],
        report.byte_range[1],
        report.byte_range[2],
        report.byte_range[3],
        report.appended_bytes,
        report.fully_valid
    );
    Ok(())
}

/// `pdfxi sign verify`.
pub(crate) fn verify(file: &Path, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let verification = verify_signature(&bytes)
        .map_err(|error| anyhow::anyhow!("verify {}: {error}", file.display()))?;
    let report = verification_report(file, &verification);
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!(
            "file: {}  status: {}  fully-valid: {}",
            report.file, report.status, report.fully_valid
        );
        println!(
            "signer: {}  fingerprint: {}",
            report.signer, report.fingerprint_sha256
        );
        println!(
            "digest: {}  sub-filter: {}  chain-trusted: {}",
            report.digest,
            report.sub_filter,
            report
                .chain_trusted
                .map(|trusted| trusted.to_string())
                .unwrap_or_else(|| "not-evaluated".to_owned())
        );
        println!(
            "byte-range: [{}, {}, {}, {}]  appended: {}",
            report.byte_range[0],
            report.byte_range[1],
            report.byte_range[2],
            report.byte_range[3],
            report.appended_after_signature
        );
    }
    if !report.fully_valid {
        bail!(
            "signature is not fully valid: status {} (signature {}, digest {}, whole-file {})",
            report.status,
            report.signature_valid,
            report.bytes_match,
            report.covers_entire_file
        );
    }
    Ok(())
}
