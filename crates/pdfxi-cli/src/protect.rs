//! `pdfxi protect` and `pdfxi unprotect`: standard security handler output.
//!
//! [`pdfxi_security::protect`] owns the standard security handler (RC4-40,
//! RC4-128, AES-128, AES-256) and the encrypting save path. This host writes a
//! real protected PDF, reopens it with the password to verify the round trip,
//! and can remove protection. Passwords are never logged or included in errors.

use std::path::Path;

use anyhow::{Context, bail};
use pdfxi_core::Document;
use pdfxi_security::{
    EncryptionMethod, Permission, Permissions, ProtectOptions, ProtectPermissions, open_encrypted,
    protect_document, remove_protection,
};
use serde::Serialize;

/// JSON shape of `pdfxi protect`.
#[derive(Serialize)]
struct ProtectReport {
    /// Written file.
    out: String,
    /// Output bytes.
    bytes: usize,
    /// Encryption method.
    method: String,
    /// Security handler revision reported on reopen.
    revision: u8,
    /// Key length in bits.
    key_length_bits: u16,
    /// Granted permissions.
    permissions: Vec<String>,
    /// Whether an owner password was supplied.
    owner_password: bool,
    /// Pages in the reopened document.
    pages: usize,
    /// Characters of the first page's text after decryption.
    first_page_chars: usize,
}

/// JSON shape of `pdfxi unprotect`.
#[derive(Serialize)]
struct UnprotectReport {
    /// Written file.
    out: String,
    /// Output bytes.
    bytes: usize,
    /// Pages in the decrypted document.
    pages: usize,
    /// Characters of the first page's text.
    first_page_chars: usize,
    /// Whether the output still declares /Encrypt.
    encrypted: bool,
}

/// Parse an encryption method id.
fn parse_method(text: &str) -> anyhow::Result<EncryptionMethod> {
    EncryptionMethod::from_id(&text.trim().to_ascii_lowercase())
        .filter(|method| method.is_encrypted())
        .with_context(|| {
            format!("unknown method {text:?}; expected rc4-40, rc4-128, aes-128 or aes-256")
        })
}

/// Named permission fields.
fn permission_field(name: &str) -> Option<fn(&mut ProtectPermissions)> {
    match name {
        "print" => Some(|permissions| permissions.print = true),
        "modify" => Some(|permissions| permissions.modify = true),
        "copy" => Some(|permissions| permissions.copy = true),
        "annotate" => Some(|permissions| permissions.annotate = true),
        "fill" | "fill-forms" => Some(|permissions| permissions.fill_forms = true),
        "accessibility" => Some(|permissions| permissions.accessibility = true),
        "assemble" => Some(|permissions| permissions.assemble = true),
        "print-high" => Some(|permissions| permissions.print_high_quality = true),
        _ => None,
    }
}

/// Parse a permission specification.
fn parse_permissions(text: &str) -> anyhow::Result<ProtectPermissions> {
    let normalised = text.trim().to_ascii_lowercase();
    match normalised.as_str() {
        "all" => return Ok(ProtectPermissions::ALL),
        "none" => return Ok(ProtectPermissions::NONE),
        "read-only" | "readonly" => return Ok(ProtectPermissions::READ_ONLY),
        _ => {}
    }
    let mut permissions = ProtectPermissions::NONE;
    for name in normalised
        .split(',')
        .map(str::trim)
        .filter(|name| !name.is_empty())
    {
        let field = permission_field(name)
            .with_context(|| format!("unknown permission {name:?}; expected print, modify, copy, annotate, fill, accessibility, assemble or print-high"))?;
        field(&mut permissions);
    }
    Ok(permissions)
}

/// Enabled permission names.
fn permission_names(permissions: Permissions) -> Vec<String> {
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
        if permissions.allows(permission) {
            names.push(name.to_owned());
        }
    }
    names
}

/// Whether the bytes declare `/Encrypt`.
fn declares_encrypt(bytes: &[u8]) -> bool {
    bytes
        .windows(b"/Encrypt".len())
        .any(|window| window == b"/Encrypt")
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

/// `pdfxi protect`.
#[allow(clippy::too_many_arguments)]
pub(crate) fn protect(
    file: &Path,
    out: &Path,
    password: &str,
    owner: Option<&str>,
    method: &str,
    permissions: &str,
    revision: Option<u8>,
    encrypt_metadata: bool,
    json: bool,
) -> anyhow::Result<()> {
    let method = parse_method(method)?;
    let permissions = parse_permissions(permissions)?;
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    if declares_encrypt(&bytes) {
        bail!(
            "{} is already encrypted; run `pdfxi unprotect` first",
            file.display()
        );
    }
    let document = Document::from_bytes(bytes)
        .map_err(|error| anyhow::anyhow!("parse {}: {error}", file.display()))?;
    let (pages, first_page_chars) = page_summary(&document)?;
    let mut options = ProtectOptions::new(password.as_bytes())
        .with_permissions(permissions)
        .with_encryption(method)
        .with_encrypt_metadata(encrypt_metadata);
    if let Some(owner) = owner {
        options = options.with_owner_password(owner.as_bytes());
    }
    if let Some(revision) = revision {
        options = options.with_revision(revision);
    }
    let protected = protect_document(&document, options)
        .map_err(|error| anyhow::anyhow!("protect {}: {error}", file.display()))?;
    // Fail closed: reopen with the user password and confirm the content.
    let (reopened, report) = open_encrypted(&protected, Some(password.as_bytes()))
        .map_err(|error| anyhow::anyhow!("reopen protected document: {error}"))?;
    let (reopened_pages, reopened_chars) = page_summary(&reopened)?;
    if reopened_pages != pages || reopened_chars != first_page_chars {
        bail!(
            "protected document reopened with {reopened_pages} page(s)/{reopened_chars} chars, expected {pages}/{first_page_chars}"
        );
    }
    std::fs::write(out, &protected).with_context(|| format!("write {}", out.display()))?;
    let result = ProtectReport {
        out: out.display().to_string(),
        bytes: protected.len(),
        method: report.method.as_id().to_owned(),
        revision: report.revision,
        key_length_bits: report.method.key_length_bits(),
        permissions: permission_names(report.permissions),
        owner_password: owner.is_some(),
        pages: reopened_pages,
        first_page_chars: reopened_chars,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  method: {}  revision: {}  key: {} bits",
        result.out, result.bytes, result.method, result.revision, result.key_length_bits
    );
    println!(
        "permissions: {}  owner-password: {}  pages: {}  first-page chars: {}",
        if result.permissions.is_empty() {
            "none".to_owned()
        } else {
            result.permissions.join(",")
        },
        result.owner_password,
        result.pages,
        result.first_page_chars
    );
    Ok(())
}

/// `pdfxi unprotect`.
pub(crate) fn unprotect(file: &Path, out: &Path, password: &str, json: bool) -> anyhow::Result<()> {
    let bytes = std::fs::read(file).with_context(|| format!("read {}", file.display()))?;
    let decrypted = remove_protection(&bytes, password.as_bytes())
        .map_err(|error| anyhow::anyhow!("unprotect {}: {error}", file.display()))?;
    let document = Document::from_bytes(decrypted.clone())
        .map_err(|error| anyhow::anyhow!("parse decrypted document: {error}"))?;
    let (pages, first_page_chars) = page_summary(&document)?;
    let encrypted = declares_encrypt(&decrypted);
    if encrypted {
        bail!("decrypted output still declares /Encrypt");
    }
    std::fs::write(out, &decrypted).with_context(|| format!("write {}", out.display()))?;
    let result = UnprotectReport {
        out: out.display().to_string(),
        bytes: decrypted.len(),
        pages,
        first_page_chars,
        encrypted,
    };
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
        return Ok(());
    }
    println!(
        "out: {}  bytes: {}  pages: {}  first-page chars: {}  encrypted: {}",
        result.out, result.bytes, result.pages, result.first_page_chars, result.encrypted
    );
    Ok(())
}
