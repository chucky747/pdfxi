//! `pdfxi protection apply`: policy-driven document protection.
//!
//! [`pdfxi_security::policy`] owns [`ProtectionPolicy`](pdfxi_security::ProtectionPolicy) validation, the ordered
//! protection plan, and caller-clocked expiry enforcement. This host applies a
//! policy to a real PDF: it validates the policy, refuses an expired one,
//! optionally stamps a visible watermark, writes standard-handler encryption
//! with the policy's permission mask, reopens the output with the password, and
//! reports the applied plan. Recipient labels are recorded; wrapping content
//! keys for certificate recipients remains a caller-supplied backend boundary.

use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_security::{
    EncryptionMethod, Permission, ProtectOptions, ProtectPermissions, ProtectionPolicy,
    apply_protection_policy, open_encrypted, protect_document,
};
use serde::Serialize;

/// JSON shape of `pdfxi protection apply`.
#[derive(Debug, Serialize)]
struct ProtectionReport {
    /// Written file.
    out: String,
    /// Policy name.
    policy: String,
    /// Stable handler identifier.
    handler: String,
    /// Security handler revision.
    revision: u8,
    /// Key length in bits.
    key_length: u16,
    /// Granted permissions.
    permissions: Vec<String>,
    /// Declared recipient labels.
    recipients: Vec<String>,
    /// Applied plan actions.
    actions: Vec<String>,
    /// Evaluation time.
    evaluated_at: i64,
    /// Whether the policy was expired at the evaluation time.
    expired: bool,
    /// Whether a watermark was stamped.
    watermark: bool,
    /// Output bytes.
    bytes: usize,
    /// Output pages.
    pages: usize,
    /// First-page text characters after reopening.
    first_page_chars: usize,
    /// Whether the output reopened with the password.
    reparsed: bool,
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

/// Map a policy handler identifier to its canonical id and encryption method.
fn handler_method(handler: &str) -> anyhow::Result<(&'static str, EncryptionMethod)> {
    let mapped = match handler.trim().to_ascii_lowercase().as_str() {
        "standard-r2" | "r2" | "rc4-40" => ("standard-r2", EncryptionMethod::Rc4_40),
        "standard-r3" | "r3" | "rc4-128" => ("standard-r3", EncryptionMethod::Rc4_128),
        "standard-r4" | "r4" | "aes-128" => ("standard-r4", EncryptionMethod::Aes128),
        "standard-r6" | "r6" | "aes-256" => ("standard-r6", EncryptionMethod::Aes256),
        other => bail!("unknown handler {other:?}; expected standard-r2, r3, r4 or r6"),
    };
    Ok(mapped)
}

/// Current Unix time.
fn now_unix() -> anyhow::Result<i64> {
    let duration = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .context("system clock is before the Unix epoch")?;
    i64::try_from(duration.as_secs()).context("system clock is out of range")
}

/// Page count and first-page text length.
fn page_summary(document: &Document) -> anyhow::Result<(usize, usize)> {
    let pages = document
        .page_count()
        .map_err(|error| anyhow::anyhow!("count pages: {error}"))?;
    let characters = if pages == 0 {
        0
    } else {
        document
            .page_text(0)
            .map_err(|error| anyhow::anyhow!("read page 1 text: {error}"))?
            .chars()
            .count()
    };
    Ok((pages, characters))
}

/// `pdfxi protection apply`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn apply(
    file: &Path,
    out: &Path,
    name: &str,
    password: &str,
    owner: Option<&str>,
    handler: &str,
    permissions: &str,
    recipients: &[String],
    expiry: Option<i64>,
    now: Option<i64>,
    watermark: bool,
    watermark_text: &str,
    json: bool,
) -> anyhow::Result<()> {
    if password.is_empty() {
        bail!("--password must not be empty");
    }
    if recipients.is_empty() {
        bail!("supply at least one --recipient LABEL");
    }
    let (handler_id, method) = handler_method(handler)?;
    let permission = parse_permissions(permissions)?;
    // Reject contradictory grants before any bytes are written.
    let grant = ProtectPermissions::from_permission(permission)
        .to_permission()
        .map_err(|error| anyhow::anyhow!("permission mask: {error}"))?;
    let policy = ProtectionPolicy::with_protection_id(
        name,
        handler_id,
        method.key_length_bits(),
        grant,
        recipients.to_vec(),
    )
    .map_err(|error| anyhow::anyhow!("protection policy: {error}"))?
    .with_watermark(watermark);
    let policy = match expiry {
        Some(expiry) => policy.with_expiry(expiry),
        None => policy,
    };
    let now = now.unwrap_or(now_unix()?);
    let applied = apply_protection_policy(&policy, now)
        .map_err(|error| anyhow::anyhow!("apply policy {name:?}: {error}"))?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let source = if watermark {
        let text = watermark_text.trim();
        if text.is_empty() {
            bail!("--watermark-text must not be empty");
        }
        crate::decorate::apply_default_watermark(&document, text)
            .map_err(|error| anyhow::anyhow!("watermark {}: {error}", file.display()))?
    } else {
        document
            .save()
            .map_err(|error| anyhow::anyhow!("save {}: {error}", file.display()))?
    };
    let source_document = Document::from_bytes(source.clone())
        .map_err(|error| anyhow::anyhow!("parse prepared document: {error}"))?;
    let (source_pages, source_chars) = page_summary(&source_document)?;
    let mut options = ProtectOptions::new(password.as_bytes())
        .with_permissions(ProtectPermissions::from_permission(grant))
        .with_encryption(method);
    if let Some(owner) = owner {
        options = options.with_owner_password(owner.as_bytes());
    }
    let protected = protect_document(&source_document, options)
        .map_err(|error| anyhow::anyhow!("protect {}: {error}", file.display()))?;
    let (reopened, report) = open_encrypted(&protected, Some(password.as_bytes()))
        .map_err(|error| anyhow::anyhow!("reopen protected output: {error}"))?;
    let (reopened_pages, reopened_chars) = page_summary(&reopened)?;
    if reopened_pages != source_pages || reopened_chars != source_chars {
        bail!(
            "protected output reopened with {reopened_pages} page(s)/{reopened_chars} chars, expected {source_pages}/{source_chars}"
        );
    }
    std::fs::write(out, &protected).with_context(|| format!("write {}", out.display()))?;
    let result = ProtectionReport {
        out: out.display().to_string(),
        policy: applied.name.clone(),
        handler: applied.handler_id.to_owned(),
        revision: report.revision,
        key_length: report.method.key_length_bits(),
        permissions: permission_names(grant),
        recipients: applied.recipients.clone(),
        actions: applied
            .actions
            .iter()
            .map(|action| action.as_id().to_owned())
            .collect(),
        evaluated_at: applied.evaluated_at,
        expired: applied.expired,
        watermark,
        bytes: protected.len(),
        pages: reopened_pages,
        first_page_chars: reopened_chars,
        reparsed: true,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  policy: {}  handler: {}  revision: {}  key: {} bits",
        result.out, result.bytes, result.policy, result.handler, result.revision, result.key_length
    );
    println!(
        "permissions: {}  recipients: {}  watermark: {}",
        if result.permissions.is_empty() {
            "none".to_owned()
        } else {
            result.permissions.join(",")
        },
        result.recipients.join(","),
        result.watermark
    );
    println!(
        "actions: {}  evaluated-at: {}  expired: {}",
        result.actions.join(","),
        result.evaluated_at,
        result.expired
    );
    println!(
        "pages: {}  first-page chars: {}  reparsed: {}",
        result.pages, result.first_page_chars, result.reparsed
    );
    Ok(())
}
