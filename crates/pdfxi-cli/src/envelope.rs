//! `pdfxi envelope`: authenticated local security envelopes.
//!
//! [`pdfxi_security::security_envelope`] wraps any payload with a recipient
//! list, a permission mask and a SHA-256 integrity tag; with a caller-supplied
//! key it adds an HMAC-SHA256 authentication tag. This host seals and opens real
//! files. It provides integrity and authentication, **not** confidentiality:
//! the payload is stored verbatim. Keys are never printed or logged.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_security::Permission;
use pdfxi_security::security_envelope::{
    EnvelopeRecipient, Integrity, build_authenticated_security_envelope, build_security_envelope,
    open_security_envelope, open_security_envelope_with_key,
};
use serde::Serialize;

/// JSON shape of a sealed envelope.
#[derive(Debug, Serialize)]
struct SealReport {
    /// Input file.
    input: String,
    /// Written envelope.
    out: String,
    /// Envelope version.
    version: u8,
    /// `authenticated` or `unkeyed`.
    integrity: String,
    /// Envelope bytes.
    bytes: usize,
    /// Payload bytes.
    payload_len: usize,
    /// Recipient count.
    recipients: usize,
    /// Recipient labels.
    labels: Vec<String>,
    /// Envelope-wide permissions.
    permissions: Vec<String>,
}

/// JSON shape of an opened envelope.
#[derive(Debug, Serialize)]
struct OpenReport {
    /// Envelope file.
    envelope: String,
    /// Written payload, when extracting.
    out: Option<String>,
    /// Envelope version.
    version: u8,
    /// `authenticated` or `unkeyed`.
    integrity: String,
    /// Whether the integrity tag matched.
    integrity_ok: bool,
    /// Payload bytes.
    payload_len: usize,
    /// Recipients with their permissions.
    recipients: Vec<RecipientRow>,
    /// Envelope-wide permissions.
    permissions: Vec<String>,
    /// Requested recipient, when authorization was checked.
    authorized: Option<String>,
    /// Requested permission, when authorization was checked.
    required: Option<String>,
}

/// One recipient row.
#[derive(Debug, Serialize)]
struct RecipientRow {
    /// Label.
    label: String,
    /// Granted permissions.
    permissions: Vec<String>,
}

/// Named permission fields.
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

/// Parse a permission specification.
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
                "unknown permission {name:?}; expected print, modify, copy, annotate, fill, accessibility, assemble or print-high"
            )
        })?;
        permissions = permissions.union(permission);
    }
    Ok(permissions)
}

/// Permission names in a stable order.
fn permission_names(permissions: Permission) -> Vec<String> {
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
        if permissions.contains(permission) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Parse one `LABEL[:PERM,PERM]` recipient specification.
fn parse_recipient(
    text: &str,
    envelope_permissions: Permission,
) -> anyhow::Result<EnvelopeRecipient> {
    let (label, permissions) = match text.split_once(':') {
        Some((label, permissions)) => (label.trim(), parse_permissions(permissions)?),
        None => (text.trim(), envelope_permissions),
    };
    if label.is_empty() {
        bail!("a recipient needs a non-empty label");
    }
    Ok(EnvelopeRecipient::new(label, permissions))
}

/// Parse a hex key.
fn parse_hex(text: &str) -> anyhow::Result<Vec<u8>> {
    let cleaned: String = text
        .chars()
        .filter(|ch| !ch.is_ascii_whitespace())
        .collect();
    if cleaned.is_empty() || !cleaned.chars().all(|ch| ch.is_ascii_hexdigit()) {
        bail!("--key-hex must be a non-empty hex string");
    }
    if !cleaned.len().is_multiple_of(2) {
        bail!("--key-hex must have an even number of hex digits");
    }
    let mut bytes = Vec::with_capacity(cleaned.len() / 2);
    for offset in (0..cleaned.len()).step_by(2) {
        let pair = &cleaned[offset..offset + 2];
        bytes.push(u8::from_str_radix(pair, 16).with_context(|| format!("hex {pair:?}"))?);
    }
    Ok(bytes)
}

/// Load the authentication key, if one was supplied.
fn load_key(key_file: Option<&Path>, key_hex: Option<&str>) -> anyhow::Result<Option<Vec<u8>>> {
    match (key_file, key_hex) {
        (Some(_), Some(_)) => bail!("--key-file and --key-hex cannot be combined"),
        (Some(path), None) => {
            let key = std::fs::read(path)
                .with_context(|| format!("read authentication key {}", path.display()))?;
            if key.is_empty() {
                bail!("authentication key {} is empty", path.display());
            }
            Ok(Some(key))
        }
        (None, Some(text)) => {
            let key = parse_hex(text)?;
            if key.is_empty() {
                bail!("authentication key is empty");
            }
            Ok(Some(key))
        }
        (None, None) => Ok(None),
    }
}

/// `integrity` label for a report.
fn integrity_label(integrity: Integrity) -> &'static str {
    match integrity {
        Integrity::Authenticated => "authenticated",
        Integrity::Unkeyed => "unkeyed",
    }
}

/// `pdfxi envelope seal`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn seal(
    input: &Path,
    out: &Path,
    recipients: &[String],
    permissions: &str,
    key_file: Option<&Path>,
    key_hex: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    if recipients.is_empty() {
        bail!("supply at least one --recipient LABEL");
    }
    let permissions = parse_permissions(permissions)?;
    let key = load_key(key_file, key_hex)?;
    let mut parsed = Vec::with_capacity(recipients.len());
    for recipient in recipients {
        parsed.push(parse_recipient(recipient, permissions)?);
    }
    let payload = std::fs::read(input).with_context(|| format!("read {}", input.display()))?;
    let envelope = match &key {
        Some(key) => build_authenticated_security_envelope(&payload, permissions, parsed, key)
            .map_err(|error| anyhow::anyhow!("seal {}: {error}", input.display()))?,
        None => build_security_envelope(&payload, permissions, parsed)
            .map_err(|error| anyhow::anyhow!("seal {}: {error}", input.display()))?,
    };
    let bytes = envelope.to_bytes();
    std::fs::write(out, &bytes).with_context(|| format!("write {}", out.display()))?;
    let report = SealReport {
        input: input.display().to_string(),
        out: out.display().to_string(),
        version: envelope.version(),
        integrity: if envelope.is_authenticated() {
            "authenticated".to_owned()
        } else {
            "unkeyed".to_owned()
        },
        bytes: bytes.len(),
        payload_len: payload.len(),
        recipients: envelope.recipients().len(),
        labels: envelope
            .recipients()
            .iter()
            .map(|recipient| recipient.label().to_owned())
            .collect(),
        permissions: permission_names(permissions),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  version: {}  integrity: {}  payload: {} bytes",
        report.out, report.bytes, report.version, report.integrity, report.payload_len
    );
    println!(
        "recipients: {} [{}]  permissions: {}",
        report.recipients,
        report.labels.join(", "),
        if report.permissions.is_empty() {
            "none".to_owned()
        } else {
            report.permissions.join(",")
        }
    );
    if report.integrity == "unkeyed" {
        println!("warning: no key supplied; the payload and permissions are not authenticated");
    }
    Ok(())
}

/// Open or verify an envelope.
#[allow(clippy::too_many_arguments)]
fn open_common(
    envelope_path: &Path,
    out: Option<&Path>,
    key_file: Option<&Path>,
    key_hex: Option<&str>,
    recipient: Option<&str>,
    require: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    let key = load_key(key_file, key_hex)?;
    let bytes = std::fs::read(envelope_path)
        .with_context(|| format!("read {}", envelope_path.display()))?;
    let opened = match &key {
        Some(key) => open_security_envelope_with_key(&bytes, key),
        None => open_security_envelope(&bytes),
    }
    .map_err(|error| anyhow::anyhow!("open {}: {error}", envelope_path.display()))?;
    if let (Some(label), Some(permission_text)) = (recipient, require) {
        let permission = permission_field(&permission_text.trim().to_ascii_lowercase())
            .with_context(|| format!("unknown permission {permission_text:?}"))?;
        let entry = opened
            .recipients()
            .iter()
            .find(|candidate| candidate.label() == label)
            .with_context(|| format!("recipient {label:?} is not addressed by this envelope"))?;
        if !entry.allows(permission) {
            bail!("recipient {label:?} is not granted the {permission_text} permission");
        }
    } else if recipient.is_some() || require.is_some() {
        bail!("--recipient and --require must be supplied together");
    }
    if let Some(out) = out {
        std::fs::write(out, opened.payload())
            .with_context(|| format!("write {}", out.display()))?;
    }
    let report = OpenReport {
        envelope: envelope_path.display().to_string(),
        out: out.map(|path| path.display().to_string()),
        version: opened.report().version,
        integrity: integrity_label(opened.report().integrity).to_owned(),
        integrity_ok: opened.report().integrity_ok,
        payload_len: opened.report().payload_len,
        recipients: opened
            .recipients()
            .iter()
            .map(|entry| RecipientRow {
                label: entry.label().to_owned(),
                permissions: permission_names(entry.permissions()),
            })
            .collect(),
        permissions: permission_names(opened.permissions()),
        authorized: recipient.map(str::to_owned),
        required: require.map(str::to_owned),
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&report)?);
        return Ok(());
    }
    println!(
        "envelope: {}  version: {}  integrity: {}  payload: {} bytes",
        report.envelope, report.version, report.integrity, report.payload_len
    );
    if let Some(out) = &report.out {
        println!("out: {out}");
    }
    println!("permissions: {}", report.permissions.join(","));
    for row in &report.recipients {
        println!(
            "recipient: {} [{}]",
            row.label,
            if row.permissions.is_empty() {
                "none".to_owned()
            } else {
                row.permissions.join(",")
            }
        );
    }
    if let Some(label) = &report.authorized {
        println!(
            "authorized: {label} for {}",
            report.required.as_deref().unwrap_or("")
        );
    }
    Ok(())
}

/// `pdfxi envelope open`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open(
    envelope: &Path,
    out: &Path,
    key_file: Option<&Path>,
    key_hex: Option<&str>,
    recipient: Option<&str>,
    require: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    open_common(
        envelope,
        Some(out),
        key_file,
        key_hex,
        recipient,
        require,
        json,
    )
}

/// `pdfxi envelope verify`.
pub(crate) fn verify(
    envelope: &Path,
    key_file: Option<&Path>,
    key_hex: Option<&str>,
    recipient: Option<&str>,
    require: Option<&str>,
    json: bool,
) -> anyhow::Result<()> {
    open_common(envelope, None, key_file, key_hex, recipient, require, json)
}
