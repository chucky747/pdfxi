//! `pdfxi id`: a metadata catalog of local digital IDs.
//!
//! [`pdfxi_security::digital_id_lifecycle`] owns the validation: a PKCS#8
//! private key is paired with its certificate, the key algorithm must match the
//! certificate's `SubjectPublicKeyInfo`, and operational times come from the
//! caller. This host loads identities from PKCS#12 or PEM/DER files, keeps a
//! **metadata-only** catalog at a caller-supplied path, and lists, imports and
//! removes entries. Private keys are never copied into the catalog and never
//! printed.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use pdfxi_security::{
    Certificate, DigitalIdLifecycleStore, KeyCertificatePair, ValidityStatus,
    certificates_from_pem, pkcs12::import_pfx, private_key_der_from_pem,
};
use serde::{Deserialize, Serialize};

/// Catalog format version.
const CATALOG_VERSION: u32 = 1;
/// Maximum number of catalog entries.
const MAX_ENTRIES: usize = 64;
/// Maximum label length.
const MAX_LABEL_LEN: usize = 64;

/// One catalog entry. It carries no key material and no document content.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Entry {
    /// Caller-assigned label.
    label: String,
    /// Certificate subject.
    subject: String,
    /// Certificate issuer.
    issuer: String,
    /// Certificate serial.
    serial: u64,
    /// SHA-256 fingerprint of the certificate DER, lowercase hex.
    fingerprint_sha256: String,
    /// Dotted OID of the paired key algorithm.
    key_algorithm: String,
    /// Certificate `notBefore` in Unix seconds.
    not_before: u64,
    /// Certificate `notAfter` in Unix seconds.
    not_after: u64,
    /// Caller-supplied import time in Unix seconds.
    imported_at: u64,
    /// Identity file the entry refers to (path only, never its bytes).
    source: String,
}

/// The metadata-only catalog file.
#[derive(Debug, Clone, Serialize, Deserialize)]
struct Catalog {
    /// Format version (`1`).
    version: u32,
    /// Entries sorted by label.
    ids: Vec<Entry>,
}

impl Catalog {
    fn empty() -> Self {
        Self {
            version: CATALOG_VERSION,
            ids: Vec::new(),
        }
    }
}

/// JSON view of one entry with validity evaluated at the requested time.
#[derive(Debug, Serialize)]
struct Row {
    label: String,
    subject: String,
    issuer: String,
    serial: u64,
    fingerprint_sha256: String,
    key_algorithm: String,
    not_before: u64,
    not_after: u64,
    validity: String,
    imported_at: u64,
    source: String,
}

/// JSON shape of the catalog-wide reports.
#[derive(Debug, Serialize)]
struct CatalogReport {
    store: String,
    count: usize,
    ids: Vec<Row>,
}

/// Validate a label: non-empty, bounded, no control characters or separators.
fn validate_label(label: &str) -> anyhow::Result<String> {
    let label = label.trim();
    if label.is_empty() || label.len() > MAX_LABEL_LEN {
        bail!("--label must be 1..={MAX_LABEL_LEN} characters");
    }
    if label
        .chars()
        .any(|character| character.is_control() || "/\\:*?\"<>|".contains(character))
    {
        bail!("--label must not contain control characters or path separators");
    }
    Ok(label.to_owned())
}

/// Map a validity status to a stable label.
fn validity_label(status: ValidityStatus) -> &'static str {
    match status {
        ValidityStatus::Valid => "valid",
        ValidityStatus::NotYetValid => "not_yet_valid",
        ValidityStatus::Expired => "expired",
    }
}

/// Validity from stored bounds.
fn validity_at(entry: &Entry, now: u64) -> &'static str {
    if now < entry.not_before {
        "not_yet_valid"
    } else if now > entry.not_after {
        "expired"
    } else {
        "valid"
    }
}

/// Current Unix time.
fn now_unix() -> anyhow::Result<u64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    Ok(duration.as_secs())
}

/// Load the catalog; a missing file is an empty catalog.
fn load_catalog(path: &Path) -> anyhow::Result<Catalog> {
    if !path.exists() {
        return Ok(Catalog::empty());
    }
    let text = std::fs::read_to_string(path)
        .with_context(|| format!("read catalog {}", path.display()))?;
    let mut catalog: Catalog =
        serde_json::from_str(&text).with_context(|| format!("parse catalog {}", path.display()))?;
    if catalog.version != CATALOG_VERSION {
        bail!(
            "catalog {} has version {}; expected {CATALOG_VERSION}",
            path.display(),
            catalog.version
        );
    }
    if catalog.ids.len() > MAX_ENTRIES {
        bail!("catalog {} has too many entries", path.display());
    }
    let mut seen = std::collections::BTreeSet::new();
    for entry in &catalog.ids {
        if !seen.insert(entry.label.clone()) {
            bail!(
                "catalog {} has duplicate label {:?}",
                path.display(),
                entry.label
            );
        }
    }
    catalog.ids.sort_by(|a, b| a.label.cmp(&b.label));
    Ok(catalog)
}

/// Write the catalog atomically enough for a single-writer CLI.
fn save_catalog(path: &Path, catalog: &Catalog) -> anyhow::Result<()> {
    if let Some(parent) = path.parent()
        && !parent.as_os_str().is_empty()
    {
        std::fs::create_dir_all(parent).with_context(|| format!("create {}", parent.display()))?;
    }
    let mut text = serde_json::to_string_pretty(catalog)?;
    text.push('\n');
    let temporary = path.with_extension("tmp");
    std::fs::write(&temporary, text).with_context(|| format!("write {}", temporary.display()))?;
    std::fs::rename(&temporary, path).with_context(|| format!("replace {}", path.display()))?;
    Ok(())
}

/// Identity inputs shared by `inspect` and `import`.
#[derive(Debug, Clone)]
pub(crate) struct IdentityFiles {
    /// Password-protected PKCS#12 container.
    pub pfx: Option<PathBuf>,
    /// PKCS#12 password (empty when omitted).
    pub password: String,
    /// PEM or DER PKCS#8 private key.
    pub key: Option<PathBuf>,
    /// PEM or DER certificate.
    pub cert: Option<PathBuf>,
}

/// Load and validate an identity, returning its certificate and pair.
fn load_identity(files: &IdentityFiles) -> anyhow::Result<(Certificate, KeyCertificatePair)> {
    let (key_der, cert_der) = match (&files.pfx, &files.key, &files.cert) {
        (Some(pfx), None, None) => {
            let bytes =
                std::fs::read(pfx).with_context(|| format!("read PKCS#12 {}", pfx.display()))?;
            let contents = import_pfx(&bytes, &files.password)
                .map_err(|error| anyhow::anyhow!("load PKCS#12 {}: {error}", pfx.display()))?;
            let certificate = contents
                .certificates()
                .first()
                .context("PKCS#12 contains no certificate")?
                .clone();
            (contents.private_key_der().to_vec(), certificate)
        }
        (None, Some(key), Some(cert)) => {
            let key_pem = std::fs::read_to_string(key)
                .with_context(|| format!("read private key {}", key.display()))?;
            let key_der = private_key_der_from_pem(&key_pem)
                .map_err(|error| anyhow::anyhow!("parse private key {}: {error}", key.display()))?;
            let cert_bytes = std::fs::read(cert)
                .with_context(|| format!("read certificate {}", cert.display()))?;
            let is_pem = cert_bytes
                .windows(b"-----BEGIN".len())
                .any(|window| window == b"-----BEGIN");
            let certificate = if is_pem {
                certificates_from_pem(&cert_bytes)
                    .map_err(|error| {
                        anyhow::anyhow!("parse certificate {}: {error}", cert.display())
                    })?
                    .into_iter()
                    .next()
                    .context("certificate PEM contains no certificate")?
            } else {
                Certificate::parse(&cert_bytes).map_err(|error| {
                    anyhow::anyhow!("parse certificate {}: {error}", cert.display())
                })?
            };
            (key_der, certificate.der().to_vec())
        }
        (None, None, None) => bail!("supply --pfx, or --key with --cert"),
        _ => bail!("--pfx cannot be combined with --key/--cert"),
    };
    let pair = KeyCertificatePair::parse(&cert_der, &key_der)
        .map_err(|error| anyhow::anyhow!("validate identity: {error}"))?;
    let certificate = Certificate::parse(&cert_der)
        .map_err(|error| anyhow::anyhow!("parse identity certificate: {error}"))?;
    Ok((certificate, pair))
}

/// Build a catalog entry from a validated pair.
fn entry_from(
    label: &str,
    pair: &KeyCertificatePair,
    now: u64,
    source: &str,
) -> anyhow::Result<Entry> {
    let certificate = pair.certificate();
    let fingerprint = certificate.fingerprint_sha256();
    let mut fingerprint_sha256 = String::with_capacity(64);
    for byte in fingerprint {
        fingerprint_sha256.push_str(&format!("{byte:02x}"));
    }
    let key_algorithm = pair
        .private_key()
        .algorithm_oid()
        .iter()
        .map(u32::to_string)
        .collect::<Vec<_>>()
        .join(".");
    Ok(Entry {
        label: label.to_owned(),
        subject: certificate.subject().display_rfc4514(),
        issuer: certificate.issuer().display_rfc4514(),
        serial: certificate.serial_number(),
        fingerprint_sha256,
        key_algorithm,
        not_before: certificate.validity().not_before(),
        not_after: certificate.validity().not_after(),
        imported_at: now,
        source: source.to_owned(),
    })
}

/// Row from a catalog entry.
fn row(entry: &Entry, now: u64) -> Row {
    Row {
        label: entry.label.clone(),
        subject: entry.subject.clone(),
        issuer: entry.issuer.clone(),
        serial: entry.serial,
        fingerprint_sha256: entry.fingerprint_sha256.clone(),
        key_algorithm: entry.key_algorithm.clone(),
        not_before: entry.not_before,
        not_after: entry.not_after,
        validity: validity_at(entry, now).to_owned(),
        imported_at: entry.imported_at,
        source: entry.source.clone(),
    }
}

/// Print rows as JSON or text.
fn print_rows(report: &CatalogReport, json: bool) -> anyhow::Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(report)?);
        return Ok(());
    }
    println!("store: {}  ids: {}", report.store, report.count);
    for id in &report.ids {
        println!(
            "{}: {} (serial {}, {} {}-{}, fingerprint {})",
            id.label,
            id.subject,
            id.serial,
            id.validity,
            id.not_before,
            id.not_after,
            &id.fingerprint_sha256[..16.min(id.fingerprint_sha256.len())]
        );
    }
    Ok(())
}

/// `pdfxi id inspect`.
pub(crate) fn inspect(
    files: &IdentityFiles,
    label: &str,
    now: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    let label = validate_label(label)?;
    let now = now.unwrap_or(now_unix()?);
    let (certificate, pair) = load_identity(files)?;
    let source = files
        .pfx
        .as_deref()
        .or(files.key.as_deref())
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let entry = entry_from(&label, &pair, now, &source)?;
    if json {
        println!("{}", serde_json::to_string_pretty(&row(&entry, now))?);
        return Ok(());
    }
    println!(
        "label: {}  subject: {}  serial: {}",
        label,
        certificate.subject().display_rfc4514(),
        certificate.serial_number()
    );
    println!(
        "issuer: {}  validity: {}  not-after: {}",
        certificate.issuer().display_rfc4514(),
        validity_label(pair.validity_at(now)),
        entry.not_after
    );
    println!(
        "fingerprint: {}  key: {}  source: {}",
        entry.fingerprint_sha256, entry.key_algorithm, entry.source
    );
    Ok(())
}

/// `pdfxi id import`.
pub(crate) fn import(
    store: &Path,
    label: &str,
    files: &IdentityFiles,
    now: Option<u64>,
    json: bool,
) -> anyhow::Result<()> {
    let label = validate_label(label)?;
    let now = now.unwrap_or(now_unix()?);
    let (_, pair) = load_identity(files)?;
    // Re-validate through the lifecycle store so the CLI and the model agree.
    let mut validated = DigitalIdLifecycleStore::new();
    validated
        .import(
            label.clone(),
            pair.certificate_der(),
            pair.private_key().as_bytes(),
            now,
        )
        .map_err(|error| anyhow::anyhow!("import digital ID: {error}"))?;
    let source = files
        .pfx
        .as_deref()
        .or(files.key.as_deref())
        .map(|path| path.display().to_string())
        .unwrap_or_default();
    let entry = entry_from(&label, &pair, now, &source)?;
    let mut catalog = load_catalog(store)?;
    if let Some(other) = catalog
        .ids
        .iter()
        .find(|other| other.label != label && other.fingerprint_sha256 == entry.fingerprint_sha256)
    {
        bail!(
            "certificate is already catalogued under the label {:?}",
            other.label
        );
    }
    catalog.ids.retain(|existing| existing.label != label);
    catalog.ids.push(entry);
    if catalog.ids.len() > MAX_ENTRIES {
        bail!("catalog is limited to {MAX_ENTRIES} entries");
    }
    catalog.ids.sort_by(|a, b| a.label.cmp(&b.label));
    save_catalog(store, &catalog)?;
    let rows = catalog
        .ids
        .iter()
        .map(|entry| row(entry, now))
        .collect::<Vec<_>>();
    print_rows(
        &CatalogReport {
            store: store.display().to_string(),
            count: rows.len(),
            ids: rows,
        },
        json,
    )
}

/// `pdfxi id list`.
pub(crate) fn list(store: &Path, now: Option<u64>, json: bool) -> anyhow::Result<()> {
    let now = now.unwrap_or(now_unix()?);
    let catalog = load_catalog(store)?;
    let rows = catalog
        .ids
        .iter()
        .map(|entry| row(entry, now))
        .collect::<Vec<_>>();
    print_rows(
        &CatalogReport {
            store: store.display().to_string(),
            count: rows.len(),
            ids: rows,
        },
        json,
    )
}

/// `pdfxi id remove`.
pub(crate) fn remove(store: &Path, label: &str, json: bool) -> anyhow::Result<()> {
    let label = validate_label(label)?;
    let mut catalog = load_catalog(store)?;
    let before = catalog.ids.len();
    catalog.ids.retain(|entry| entry.label != label);
    if catalog.ids.len() == before {
        bail!("no digital ID with the label {label:?} is catalogued");
    }
    save_catalog(store, &catalog)?;
    let report = serde_json::json!({
        "store": store.display().to_string(),
        "removed": label,
        "count": catalog.ids.len(),
    });
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("removed: {}  remaining: {}", label, catalog.ids.len());
    }
    Ok(())
}
