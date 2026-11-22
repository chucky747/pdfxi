//! `pdfxi recipient`: certificate-addressed encryption of real files.
//!
//! [`pdfxi_security::certificate_encryption`] models an enveloped content key:
//! AES-CBC content encryption, one wrapped content-encryption key per recipient,
//! a SHA-256 key commitment and sender-granted permissions. Its own boundary was
//! the recipient public-key unwrap and envelope persistence. This module supplies
//! both: `recipient seal` wraps the CEK with each recipient certificate's P-256
//! public key (ephemeral ECDH-ES + AES-KW) and writes a portable container;
//! `recipient open` unwraps it with the matching private key and checks a\n//! required permission.
//!
//! A fresh CEK and IV are generated with the OS random source unless
//! `--cek-hex`/`--iv-hex` pin them for reproducible tests. No key material,
//! plaintext or wrapped key is printed.

use std::path::{Path, PathBuf};

use anyhow::{Context, bail};
use p256::SecretKey;
use p256::pkcs8::DecodePrivateKey;
use pdfxi_security::{
    Certificate, ContentCipher, EcdhKeyWrapProvider, EnvelopedData, PasswordRole, Permission,
    Permissions, Recipient, RecipientEnvelopeKey, RecipientList, RecipientSelector, build_envelope,
    certificates_from_pem, open_envelope,
};
use serde::{Deserialize, Serialize};

/// Container format identifier.
const FORMAT: &str = "pdfxi-recipient-envelope";
/// Container version.
const VERSION: u32 = 1;

/// JSON container holding one persisted certificate envelope.
#[derive(Debug, Serialize, Deserialize)]
struct Container {
    /// Format identifier.
    format: String,
    /// Format version.
    version: u32,
    /// Cipher identifier.
    cipher: String,
    /// Content IV as lowercase hex.
    iv: String,
    /// One entry per recipient.
    recipients: Vec<ContainerRecipient>,
    /// Ciphertext as lowercase hex.
    ciphertext: String,
    /// SHA-256 key commitment as lowercase hex.
    key_commitment: String,
    /// Permission bits.
    permissions: u32,
}

/// One recipient entry in the container.
#[derive(Debug, Serialize, Deserialize)]
struct ContainerRecipient {
    /// Recipient label (certificate subject).
    label: String,
    /// Certificate SHA-256 fingerprint as lowercase hex.
    identifier: String,
    /// RSA-OAEP wrapped content-encryption key as lowercase hex.
    wrapped_key: String,
}

/// JSON shape of `recipient seal`.
#[derive(Debug, Serialize)]
struct SealReport {
    /// Written container.
    out: String,
    /// Container size in bytes.
    bytes: usize,
    /// Content cipher.
    cipher: &'static str,
    /// Plaintext size.
    plaintext_bytes: usize,
    /// Recipient labels.
    recipients: Vec<String>,
    /// Wrapped key sizes in bytes.
    wrapped_lens: Vec<usize>,
    /// Ciphertext size.
    ciphertext_bytes: usize,
    /// Permission bits.
    permissions: u32,
    /// Permission names.
    permission_names: Vec<&'static str>,
}

/// JSON shape of `recipient open`.
#[derive(Debug, Serialize)]
struct OpenReport {
    /// Written plaintext.
    out: String,
    /// Plaintext size.
    bytes: usize,
    /// Recipient label whose key opened the container.
    recipient: String,
    /// Permission bits.
    permissions: u32,
    /// Permission names.
    permission_names: Vec<&'static str>,
    /// Required permission, when one was demanded.
    required: Option<&'static str>,
}

/// Lowercase hex.
fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Parses one hex nibble.
fn hex_nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Parses lowercase or uppercase hex.
fn hex_decode(text: &str, what: &str) -> anyhow::Result<Vec<u8>> {
    let text = text.trim();
    let bytes = text.as_bytes();
    let (pairs, remainder) = bytes.as_chunks::<2>();
    if !remainder.is_empty() {
        bail!("{what} must have an even number of hex digits");
    }
    let mut out = Vec::with_capacity(pairs.len());
    for pair in pairs {
        let high = hex_nibble(pair[0]).with_context(|| format!("{what} is not a hex string"))?;
        let low = hex_nibble(pair[1]).with_context(|| format!("{what} is not a hex string"))?;
        out.push((high << 4) | low);
    }
    Ok(out)
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

/// Loads a P-256 private key from PKCS#8 PEM.
fn load_private_key(path: &Path) -> anyhow::Result<SecretKey> {
    let pem = std::fs::read_to_string(path)
        .with_context(|| format!("read private key {}", path.display()))?;
    SecretKey::from_pkcs8_pem(&pem).map_err(|_| {
        anyhow::anyhow!(
            "{} is not an unencrypted PKCS#8 P-256 private key",
            path.display()
        )
    })
}

/// Parses a cipher name.
fn parse_cipher(text: &str) -> anyhow::Result<ContentCipher> {
    match text.trim().to_ascii_lowercase().as_str() {
        "aes256-cbc" | "aes256" => Ok(ContentCipher::Aes256Cbc),
        "aes128-cbc" | "aes128" => Ok(ContentCipher::Aes128Cbc),
        other => bail!("unknown cipher {other:?}; expected aes128-cbc or aes256-cbc"),
    }
}

/// Parses one permission name.
fn permission_field(name: &str) -> Option<Permission> {
    match name {
        "print" => Some(Permission::PRINT),
        "modify" => Some(Permission::MODIFY),
        "copy" => Some(Permission::COPY),
        "annotate" => Some(Permission::ANNOTATE),
        "fill" | "fill-forms" => Some(Permission::FILL_FORMS),
        "accessibility" => Some(Permission::ACCESSIBILITY),
        "assemble" => Some(Permission::ASSEMBLE),
        "print-high" => Some(Permission::PRINT_HIGH_QUALITY),
        _ => None,
    }
}

/// Parses a permission specification.
fn parse_permissions(text: &str) -> anyhow::Result<Permission> {
    let normalised = text.trim().to_ascii_lowercase();
    match normalised.as_str() {
        "" | "all" => return Ok(Permission::ALL),
        "none" => return Ok(Permission::NONE),
        "read-only" | "readonly" => {
            return Ok(Permission::PRINT
                .union(Permission::ACCESSIBILITY)
                .union(Permission::PRINT_HIGH_QUALITY));
        }
        _ => {}
    }
    let mut permissions = Permission::NONE;
    for name in normalised
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let permission = permission_field(name).with_context(|| {
            format!(
                "unknown permission {name:?}; expected print, modify, copy, annotate, fill, \
                 accessibility, assemble or print-high"
            )
        })?;
        permissions = permissions.union(permission);
    }
    Ok(permissions)
}

/// Permission names in a stable order.
fn permission_names(bits: Permission) -> Vec<&'static str> {
    let mut names = Vec::new();
    for (name, permission) in [
        ("print", Permission::PRINT),
        ("modify", Permission::MODIFY),
        ("copy", Permission::COPY),
        ("annotate", Permission::ANNOTATE),
        ("fill", Permission::FILL_FORMS),
        ("accessibility", Permission::ACCESSIBILITY),
        ("assemble", Permission::ASSEMBLE),
        ("print-high", Permission::PRINT_HIGH_QUALITY),
    ] {
        if bits.contains(permission) {
            names.push(name);
        }
    }
    names
}

/// Fills bytes from the OS random source.
fn random_bytes(bytes: &mut [u8]) -> anyhow::Result<()> {
    getrandom::getrandom(bytes).map_err(|error| anyhow::anyhow!("random source: {error}"))
}

/// `pdfxi recipient seal`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn seal(
    input: &Path,
    out: &Path,
    certificates: &[PathBuf],
    cipher: &str,
    permissions: &str,
    cek_hex: Option<&str>,
    iv_hex: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    if certificates.is_empty() {
        bail!("pass at least one --cert recipient certificate");
    }
    let cipher = parse_cipher(cipher)?;
    let permission = parse_permissions(permissions)?;
    let plaintext = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let mut recipients = RecipientList::new();
    for path in certificates {
        let certificates = load_certificates(path)?;
        let certificate = certificates
            .first()
            .with_context(|| format!("{} contains no certificate", path.display()))?
            .clone();
        let label = certificate.subject().display_rfc4514();
        recipients.add(Recipient::new(label, certificate)?)?;
    }
    let mut cek = vec![0u8; cipher.key_len()];
    match cek_hex {
        Some(text) => {
            cek = hex_decode(text, "--cek-hex")?;
            if cek.len() != cipher.key_len() {
                bail!(
                    "--cek-hex has {} bytes; {} needs {}",
                    cek.len(),
                    cipher.as_str(),
                    cipher.key_len()
                );
            }
        }
        None => random_bytes(&mut cek)?,
    }
    let mut iv = [0u8; 16];
    match iv_hex {
        Some(text) => {
            let decoded = hex_decode(text, "--iv-hex")?;
            if decoded.len() != 16 {
                bail!("--iv-hex has {} bytes; expected 16", decoded.len());
            }
            iv.copy_from_slice(&decoded);
        }
        None => random_bytes(&mut iv)?,
    }
    let envelope = build_envelope(
        &plaintext,
        &recipients,
        Permissions::new(permission, PasswordRole::User),
        cipher,
        &cek,
        iv,
        &EcdhKeyWrapProvider::sealing(),
    )
    .map_err(|error| anyhow::anyhow!("seal {}: {error}", input.display()))?;
    let container = Container {
        format: FORMAT.to_owned(),
        version: VERSION,
        cipher: envelope.cipher().as_str().to_owned(),
        iv: hex_encode(&envelope.iv()),
        recipients: envelope
            .recipient_keys()
            .iter()
            .map(|entry| ContainerRecipient {
                label: entry.label().to_owned(),
                identifier: entry.identifier().to_owned(),
                wrapped_key: hex_encode(entry.wrapped_key()),
            })
            .collect(),
        ciphertext: hex_encode(envelope.ciphertext()),
        key_commitment: hex_encode(&envelope.key_commitment()),
        permissions: envelope.permissions().bits().bits(),
    };
    let text = serde_json::to_string_pretty(&container)?;
    std::fs::write(out, text.as_bytes()).with_context(|| format!("write {}", out.display()))?;
    let labels: Vec<String> = recipients.iter().map(|r| r.label().to_owned()).collect();
    let report = SealReport {
        out: out.display().to_string(),
        bytes: text.len(),
        cipher: cipher.as_str(),
        plaintext_bytes: plaintext.len(),
        recipients: labels,
        wrapped_lens: envelope
            .recipient_keys()
            .iter()
            .map(RecipientEnvelopeKey::wrapped_len)
            .collect(),
        ciphertext_bytes: envelope.ciphertext_len(),
        permissions: envelope.permissions().bits().bits(),
        permission_names: permission_names(permission),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("out: {}", report.out);
        println!("bytes: {}", report.bytes);
        println!("cipher: {}", report.cipher);
        println!("plaintext-bytes: {}", report.plaintext_bytes);
        println!("ciphertext-bytes: {}", report.ciphertext_bytes);
        println!("recipients: {}", report.recipients.join("; "));
        println!(
            "permissions: {} ({})",
            report.permissions,
            report.permission_names.join(",")
        );
    }
    Ok(())
}

/// `pdfxi recipient open`.
pub(crate) fn open(
    envelope_path: &Path,
    key: &Path,
    cert: &Path,
    out: &Path,
    require: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let text = std::fs::read_to_string(envelope_path)
        .with_context(|| format!("read {}", envelope_path.display()))?;
    let container: Container = serde_json::from_str(&text)
        .with_context(|| format!("parse {} as an envelope", envelope_path.display()))?;
    if container.format != FORMAT {
        bail!(
            "{} is not a {FORMAT} container (format {:?})",
            envelope_path.display(),
            container.format
        );
    }
    if container.version != VERSION {
        bail!(
            "{} has container version {}; expected {VERSION}",
            envelope_path.display(),
            container.version
        );
    }
    let cipher = parse_cipher(&container.cipher)?;
    let certificates = load_certificates(cert)?;
    let certificate = certificates
        .first()
        .with_context(|| format!("{} contains no certificate", cert.display()))?
        .clone();
    let identifier = hex_encode(&certificate.fingerprint_sha256());
    let entry = container
        .recipients
        .iter()
        .find(|entry| entry.identifier == identifier)
        .with_context(|| {
            format!(
                "{} is not addressed to this certificate ({identifier})",
                envelope_path.display()
            )
        })?;
    let iv = hex_decode(&container.iv, "--envelope iv")?;
    if iv.len() != 16 {
        bail!("the envelope iv has {} bytes; expected 16", iv.len());
    }
    let mut iv_bytes = [0u8; 16];
    iv_bytes.copy_from_slice(&iv);
    let commitment = hex_decode(&container.key_commitment, "--envelope key commitment")?;
    if commitment.len() != 32 {
        bail!(
            "the envelope key commitment has {} bytes; expected 32",
            commitment.len()
        );
    }
    let mut commitment_bytes = [0u8; 32];
    commitment_bytes.copy_from_slice(&commitment);
    let mut entries = Vec::with_capacity(container.recipients.len());
    for entry in &container.recipients {
        entries.push(RecipientEnvelopeKey::new(
            entry.label.clone(),
            entry.identifier.clone(),
            hex_decode(&entry.wrapped_key, "--envelope wrapped key")?,
        )?);
    }
    let permissions = Permissions::new(
        Permission::from_bits_checked(container.permissions)
            .map_err(|error| anyhow::anyhow!("envelope permissions: {error}"))?,
        PasswordRole::User,
    );
    let envelope = EnvelopedData::new_parts(
        cipher,
        iv_bytes,
        entries,
        hex_decode(&container.ciphertext, "--envelope ciphertext")?,
        commitment_bytes,
        permissions,
    )?;
    let label = entry.label.clone();
    let recipients = RecipientList::new().with(Recipient::new(label.clone(), certificate)?)?;
    let private = load_private_key(key)?;
    let opened = open_envelope(
        &envelope,
        RecipientSelector::Label(&label),
        &recipients,
        &EcdhKeyWrapProvider::unsealing(private),
    )
    .map_err(|error| anyhow::anyhow!("open {}: {error}", envelope_path.display()))?;
    let required = match require {
        Some(text) => {
            let permission = parse_permissions(text)?;
            if permission.bits() == 0 || permission.bits().count_ones() > 1 {
                bail!("--require needs exactly one permission name");
            }
            opened.require_permission(permission)?;
            permission_names(permission).first().copied()
        }
        None => None,
    };
    std::fs::write(out, &opened.plaintext).with_context(|| format!("write {}", out.display()))?;
    let report = OpenReport {
        out: out.display().to_string(),
        bytes: opened.plaintext.len(),
        recipient: opened.recipient_label.clone(),
        permissions: opened.permissions.bits().bits(),
        permission_names: permission_names(opened.permissions.bits()),
        required,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
    } else {
        println!("out: {}", report.out);
        println!("bytes: {}", report.bytes);
        println!("recipient: {}", report.recipient);
        println!(
            "permissions: {} ({})",
            report.permissions,
            report.permission_names.join(",")
        );
        if let Some(required) = report.required {
            println!("required: {required}");
        }
    }
    Ok(())
}
