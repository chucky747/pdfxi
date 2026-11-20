//! Protect and unprotect real documents: apply or remove password encryption.
//!
//! [`protect_document`] turns a plaintext [`pdfxi_core::Document`] into a PDF
//! encrypted with the PDF standard security handler and returns the new bytes.
//! [`remove_protection`] authenticates an encrypted PDF and writes a full,
//! unencrypted rewrite. Both entry points reuse
//! [`crate::encryption_writer::encrypt_document`] and
//! [`crate::encryption_writer::open_encrypted`]; this module adds the
//! document-facing policy layer (password rules, permission defaults, and the
//! full-rewrite normalization that real-world files need). No cipher, hash, or
//! key schedule is implemented here.
//!
//! # Permission mapping
//!
//! [`ProtectPermissions`] exposes the eight user-role grants as named booleans.
//! They map onto the `/P` bits of ISO 32000-1 table 22:
//!
//! | Field | `/P` bit | Value | [`crate::Permission`] |
//! |---|---|---|---|
//! | [`ProtectPermissions::print`] | 3 | `1 << 2` | [`crate::Permission::PRINT`] |
//! | [`ProtectPermissions::modify`] | 4 | `1 << 3` | [`crate::Permission::MODIFY`] |
//! | [`ProtectPermissions::copy`] | 5 | `1 << 4` | [`crate::Permission::COPY`] |
//! | [`ProtectPermissions::annotate`] | 6 | `1 << 5` | [`crate::Permission::ANNOTATE`] |
//! | [`ProtectPermissions::fill_forms`] | 9 | `1 << 8` | [`crate::Permission::FILL_FORMS`] |
//! | [`ProtectPermissions::accessibility`] | 10 | `1 << 9` | [`crate::Permission::ACCESSIBILITY`] |
//! | [`ProtectPermissions::assemble`] | 11 | `1 << 10` | [`crate::Permission::ASSEMBLE`] |
//! | [`ProtectPermissions::print_high_quality`] | 12 | `1 << 11` | [`crate::Permission::PRINT_HIGH_QUALITY`] |
//!
//! The permission mask is validated before writing: annotation and page
//! assembly require modification, and high-quality printing requires printing.
//! An inconsistent request is rejected with [`SecurityError::PermissionConflict`]
//! rather than written as a file that this crate would refuse to reopen.
//!
//! # Encryption revisions
//!
//! The default is AES-256 with security-handler revision 6. The writer also
//! supports AES-128 (revision 4), RC4-128 (revision 3), and RC4-40 (revision 2)
//! through [`ProtectOptions::with_encryption`]; AES-256 accepts revision 5 or 6
//! through [`ProtectOptions::with_revision`]. RC4 and AES-128 are offered only
//! for compatibility and are not recommended for new documents.
//!
//! # Application integration (ADR 0010 / ADR 0012)
//!
//! `pdfxi-document` exposes the decrypted plaintext as
//! `DocumentSession::bytes()`. The protect flow is therefore:
//!
//! 1. The user edits the open session normally.
//! 2. The shell calls [`protect_document`] on the session's document (or on a
//!    [`pdfxi_core::Document`] parsed from `session.bytes()`) with the chosen
//!    passwords and permissions.
//! 3. The shell writes the returned bytes atomically (or hands them to
//!    `DocumentSession::open_bytes_with_password`) and loads the result as a new
//!    session. The old session must not save over the protected file, because
//!    `pdfxi-document` still refuses to save an encrypted session.
//!
//! Removing protection follows the same shape: [`remove_protection`] takes the
//! session bytes and the open password, returns plaintext, and the shell reloads
//! it. Protection and protection removal are full rewrites, so they flatten
//! incremental revisions and invalidate any signature that covered earlier
//! bytes; the sessions report that boundary separately.
//!
//! # Honest limits
//!
//! - PDF permissions are advisory, not cryptographic. The `/P` mask and the
//!   AES-256 `/Perms` block are authenticated, but a reader that ignores them,
//!   or a holder of the owner password, has full access. This crate reports the
//!   flags through [`crate::DecryptionReport`]; it does not make `pdfxi-core`
//!   text extraction refuse to run.
//! - The owner password always bypasses the mask, per the PDF owner-password
//!   semantics modelled by [`crate::Permissions`].
//! - The AES-256 file key is derived deterministically from the document `/ID`
//!   and the two passwords by the existing writer; it is not a fresh random
//!   key. Reuse of the same document ID and passwords reproduces the same key.
//!   Callers may supply a fresh [`ProtectOptions::id_seed`] for a source without
//!   `/ID`.
//! - Linearized and signed inputs are refused by the shared writer with
//!   [`SecurityError::UnsupportedDocumentStructure`]. Protecting a signed file
//!   would invalidate the signature, so it is not attempted silently.
//! - Certificate (PFX) encryption is out of scope here; it is T-0249.
//!
//! No password, key, or document content is ever logged or carried in an error.

use pdfxi_core::Document;

use crate::encryption::EncryptionMethod;
use crate::encryption_writer::{EncryptOptions, encrypt_document, open_encrypted};
use crate::error::{Result, SecurityError};
use crate::permissions::Permission;

/// Longest password the revision 5/6 standard handler hashes.
///
/// ISO 32000-2 truncates AES-256 passwords to 127 bytes. Rejecting longer
/// input is clearer than silently truncating a secret.
pub const MAX_PASSWORD_BYTES: usize = 127;

/// Bytes inspected at the end of a document when looking for `/Encrypt`.
const ENCRYPT_SCAN_BYTES: usize = 64 * 1024;

/// The `/Encrypt` trailer key.
const ENCRYPT_KEY: &[u8] = b"/Encrypt";

/// User-role permission grants for a protected document.
///
/// Each field maps to one standard `/P` bit (see the [module
/// documentation](self) for the table). The default is [`Self::ALL`], matching
/// an owner who has not restricted anything but still wants a password.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct ProtectPermissions {
    /// Allow printing (bit 3).
    pub print: bool,
    /// Allow changing document contents (bit 4).
    pub modify: bool,
    /// Allow copying or extracting text and graphics (bit 5).
    pub copy: bool,
    /// Allow adding or editing annotations (bit 6).
    pub annotate: bool,
    /// Allow filling interactive form fields (bit 9).
    pub fill_forms: bool,
    /// Allow extraction for accessibility tools (bit 10).
    pub accessibility: bool,
    /// Allow assembling, inserting, or rotating pages (bit 11).
    pub assemble: bool,
    /// Allow printing at full quality (bit 12).
    pub print_high_quality: bool,
}

impl ProtectPermissions {
    /// Grant every permission.
    pub const ALL: Self = Self {
        print: true,
        modify: true,
        copy: true,
        annotate: true,
        fill_forms: true,
        accessibility: true,
        assemble: true,
        print_high_quality: true,
    };

    /// Deny every permission.
    pub const NONE: Self = Self {
        print: false,
        modify: false,
        copy: false,
        annotate: false,
        fill_forms: false,
        accessibility: false,
        assemble: false,
        print_high_quality: false,
    };

    /// The common "read and print, do not change" preset.
    ///
    /// Printing at full quality and accessibility extraction are granted;
    /// copying, modification, annotation, form filling, and page assembly are
    /// denied.
    pub const READ_ONLY: Self = Self {
        print: true,
        modify: false,
        copy: false,
        annotate: false,
        fill_forms: false,
        accessibility: true,
        assemble: false,
        print_high_quality: true,
    };

    /// Build a permission set from a validated `/P`-style mask.
    pub fn from_permission(permission: Permission) -> Self {
        Self {
            print: permission.contains(Permission::PRINT),
            modify: permission.contains(Permission::MODIFY),
            copy: permission.contains(Permission::COPY),
            annotate: permission.contains(Permission::ANNOTATE),
            fill_forms: permission.contains(Permission::FILL_FORMS),
            accessibility: permission.contains(Permission::ACCESSIBILITY),
            assemble: permission.contains(Permission::ASSEMBLE),
            print_high_quality: permission.contains(Permission::PRINT_HIGH_QUALITY),
        }
    }

    /// Convert these grants to a permission mask, rejecting contradictory ones.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::PermissionConflict`] when annotation or page
    /// assembly is granted without modification, or high-quality printing is
    /// granted without printing.
    pub fn to_permission(self) -> Result<Permission> {
        let mut mask = Permission::NONE;
        for (enabled, bit) in [
            (self.print, Permission::PRINT),
            (self.modify, Permission::MODIFY),
            (self.copy, Permission::COPY),
            (self.annotate, Permission::ANNOTATE),
            (self.fill_forms, Permission::FILL_FORMS),
            (self.accessibility, Permission::ACCESSIBILITY),
            (self.assemble, Permission::ASSEMBLE),
            (self.print_high_quality, Permission::PRINT_HIGH_QUALITY),
        ] {
            if enabled {
                mask = mask.union(bit);
            }
        }
        Permission::validate_consistent(mask)?;
        Ok(mask)
    }
}

impl Default for ProtectPermissions {
    fn default() -> Self {
        Self::ALL
    }
}

/// Options for [`protect_document`].
///
/// The default method is AES-256 revision 6. The owner password defaults to the
/// user password when [`ProtectOptions::owner_password`] is `None`, which is
/// the usual PDF convention when the caller only wants an open password.
#[derive(Clone)]
pub struct ProtectOptions {
    /// The user (open) password, as raw bytes. Empty means open without a prompt.
    pub user_password: Vec<u8>,
    /// The owner (permissions) password. `None` reuses the user password.
    pub owner_password: Option<Vec<u8>>,
    /// Rights granted to the user role.
    pub permissions: ProtectPermissions,
    /// Encryption method; defaults to AES-256.
    pub encryption: EncryptionMethod,
    /// Optional handler revision override. AES-256 accepts 5 or 6 (default 6).
    pub revision: Option<u8>,
    /// Whether metadata strings and streams are encrypted.
    pub encrypt_metadata: bool,
    /// Optional deterministic seed for sources that have no `/ID`.
    pub id_seed: Option<Vec<u8>>,
}

impl std::fmt::Debug for ProtectOptions {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProtectOptions")
            .field("user_password", &"<redacted>")
            .field(
                "owner_password",
                &self.owner_password.as_ref().map(|_| "<redacted>"),
            )
            .field("permissions", &self.permissions)
            .field("encryption", &self.encryption)
            .field("revision", &self.revision)
            .field("encrypt_metadata", &self.encrypt_metadata)
            .field("id_seed", &self.id_seed.as_ref().map(Vec::len))
            .finish()
    }
}

impl ProtectOptions {
    /// Start with AES-256 revision 6 and all permissions granted.
    pub fn new(user_password: impl AsRef<[u8]>) -> Self {
        Self {
            user_password: user_password.as_ref().to_vec(),
            owner_password: None,
            permissions: ProtectPermissions::ALL,
            encryption: EncryptionMethod::Aes256,
            revision: None,
            encrypt_metadata: true,
            id_seed: None,
        }
    }

    /// Set a distinct owner (permissions) password.
    pub fn with_owner_password(mut self, password: impl AsRef<[u8]>) -> Self {
        self.owner_password = Some(password.as_ref().to_vec());
        self
    }

    /// Set the user-role permission grants.
    pub fn with_permissions(mut self, permissions: ProtectPermissions) -> Self {
        self.permissions = permissions;
        self
    }

    /// Select a non-default encryption method.
    pub fn with_encryption(mut self, method: EncryptionMethod) -> Self {
        self.encryption = method;
        self
    }

    /// Override the security-handler revision written to `/R`.
    pub fn with_revision(mut self, revision: u8) -> Self {
        self.revision = Some(revision);
        self
    }

    /// Set the value written to `/EncryptMetadata`.
    pub fn with_encrypt_metadata(mut self, value: bool) -> Self {
        self.encrypt_metadata = value;
        self
    }

    /// Supply a deterministic seed for sources without a document `/ID`.
    pub fn with_id_seed(mut self, seed: Vec<u8>) -> Self {
        self.id_seed = Some(seed);
        self
    }

    /// The owner password actually written: the explicit one, or the user one.
    pub fn effective_owner_password(&self) -> &[u8] {
        self.owner_password
            .as_deref()
            .unwrap_or(self.user_password.as_slice())
    }

    /// The validated `/P` mask these options produce.
    ///
    /// # Errors
    ///
    /// See [`ProtectPermissions::to_permission`].
    pub fn permission_mask(&self) -> Result<Permission> {
        self.permissions.to_permission()
    }
}

/// Return `true` when the last [`ENCRYPT_SCAN_BYTES`] contain an `/Encrypt` key.
///
/// A name boundary prevents a false positive on `/EncryptMetadata`. The scan is
/// deliberately the same heuristic the document session uses: an `/Encrypt`
/// entry is written in the trailer, which lives at the end of a non-linearized
/// file, and no content stream is allowed to make this a security decision on
/// its own.
fn has_encrypt_entry(bytes: &[u8]) -> bool {
    let start = bytes.len().saturating_sub(ENCRYPT_SCAN_BYTES);
    let window = &bytes[start..];
    window
        .windows(ENCRYPT_KEY.len())
        .enumerate()
        .any(|(index, candidate)| {
            candidate == ENCRYPT_KEY
                && !window
                    .get(index + ENCRYPT_KEY.len())
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

/// Return a plaintext document with every live object materialized.
///
/// The shared writer serializes physical `N G obj` objects only. A real-world
/// source may keep objects in an `/ObjStm`, use a cross-reference stream, or
/// carry incremental revisions. [`pdfxi_core::Document::save`] expands object
/// streams and rewrites a single classic cross-reference table, which is
/// exactly the input [`encrypt_document`] expects. Protection is a full rewrite
/// regardless, so flattening revisions is intentional and documented.
fn normalize(source: &Document) -> Result<Document> {
    match source.save() {
        Ok(rewritten) => Document::from_bytes(rewritten)
            .map_err(|error| SecurityError::DocumentRewriteFailed(error.to_string())),
        Err(pdfxi_core::Error::UnsupportedState("encrypted documents cannot be saved")) => {
            Err(SecurityError::DocumentAlreadyEncrypted)
        }
        Err(error) => Err(SecurityError::DocumentRewriteFailed(error.to_string())),
    }
}

/// Encrypt a plaintext document and return the protected PDF bytes.
///
/// The output is a standalone, full-rewrite PDF: strings and streams of every
/// live object are encrypted, an `/Encrypt` dictionary is added, and the trailer
/// carries `/Encrypt` and a two-element `/ID`. The input document is not
/// modified.
///
/// # Errors
///
/// - [`SecurityError::DocumentAlreadyEncrypted`] when the source still carries
///   an `/Encrypt` entry. Decrypt it with [`crate::open_encrypted`] first.
/// - [`SecurityError::PermissionConflict`] for contradictory permission grants.
/// - [`SecurityError::Configuration`] for an empty user-and-owner password pair,
///   a password longer than [`MAX_PASSWORD_BYTES`], or an invalid revision.
/// - [`SecurityError::UnsupportedMethod`] for [`EncryptionMethod::None`].
/// - [`SecurityError::DocumentRewriteFailed`] when the full rewrite that
///   normalizes a cross-reference-stream or object-stream source fails.
/// - [`SecurityError::UnsupportedDocumentStructure`] for signed, linearized, or
///   otherwise unsupported inputs, as refused by the shared writer.
///
/// No password or document content is included in an error value.
pub fn protect_document(source: &Document, options: ProtectOptions) -> Result<Vec<u8>> {
    if !options.encryption.is_encrypted() {
        return Err(SecurityError::UnsupportedMethod(
            options.encryption.as_id().to_owned(),
        ));
    }

    let owner_password = options.effective_owner_password();
    if options.user_password.is_empty() && owner_password.is_empty() {
        return Err(SecurityError::Configuration(
            "an empty user and owner password would not protect the document".to_owned(),
        ));
    }
    if options.user_password.len() > MAX_PASSWORD_BYTES || owner_password.len() > MAX_PASSWORD_BYTES
    {
        return Err(SecurityError::Configuration(
            "PDF passwords are limited to 127 bytes".to_owned(),
        ));
    }

    let permissions = options.permission_mask()?;
    let mut encrypt = EncryptOptions::new(
        options.encryption,
        &options.user_password,
        owner_password,
        permissions,
    )?;
    encrypt = encrypt.with_encrypt_metadata(options.encrypt_metadata);
    if let Some(revision) = options.revision {
        encrypt = encrypt.with_revision(revision)?;
    }
    if let Some(seed) = options.id_seed.clone() {
        encrypt = encrypt.with_id_seed(seed);
    }

    let normalized = normalize(source)?;
    let (bytes, _report) = encrypt_document(&normalized, &encrypt)?;
    Ok(bytes)
}

/// Remove password protection from an encrypted PDF.
///
/// The password may be the user (open) password or the owner password. The
/// returned bytes are a full, deterministic [`pdfxi_core::Document::save`]
/// rewrite with no `/Encrypt` entry, so incremental revisions and earlier
/// signatures are flattened. The input slice is never modified.
///
/// # Errors
///
/// - [`SecurityError::DocumentNotEncrypted`] when the input has no `/Encrypt`
///   entry.
/// - [`SecurityError::WrongPassword`] when the password matches neither role.
///   An empty slice is a real password: it succeeds only when the document was
///   written with an empty user password.
/// - [`SecurityError::EncryptedDocumentInvalid`] for a malformed encryption
///   dictionary.
/// - [`SecurityError::DocumentRewriteFailed`] when the final full rewrite fails.
///
/// No password or document content is included in an error value.
pub fn remove_protection(bytes: &[u8], password: &[u8]) -> Result<Vec<u8>> {
    if !has_encrypt_entry(bytes) {
        return Err(SecurityError::DocumentNotEncrypted);
    }
    let (document, _report) = open_encrypted(bytes, Some(password))?;
    document
        .save()
        .map_err(|error| SecurityError::DocumentRewriteFailed(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn permission_names(permissions: ProtectPermissions) -> Vec<&'static str> {
        let mut names = Vec::new();
        for (enabled, name) in [
            (permissions.print, "print"),
            (permissions.modify, "modify"),
            (permissions.copy, "copy"),
            (permissions.annotate, "annotate"),
            (permissions.fill_forms, "fill_forms"),
            (permissions.accessibility, "accessibility"),
            (permissions.assemble, "assemble"),
            (permissions.print_high_quality, "print_high_quality"),
        ] {
            if enabled {
                names.push(name);
            }
        }
        names
    }

    #[test]
    fn permission_fields_map_to_the_documented_bits() {
        let mask = ProtectPermissions::READ_ONLY
            .to_permission()
            .expect("read-only grants are consistent");
        assert_eq!(
            mask,
            Permission::PRINT
                .union(Permission::ACCESSIBILITY)
                .union(Permission::PRINT_HIGH_QUALITY)
        );
        assert!(!mask.contains(Permission::COPY));
        assert!(!mask.contains(Permission::MODIFY));
        assert_eq!(
            ProtectPermissions::from_permission(mask),
            ProtectPermissions::READ_ONLY
        );
        assert_eq!(
            ProtectPermissions::ALL.to_permission().expect("ALL"),
            Permission::ALL
        );
        assert_eq!(
            ProtectPermissions::NONE.to_permission().expect("NONE"),
            Permission::NONE
        );
        assert_eq!(
            permission_names(ProtectPermissions::READ_ONLY),
            vec!["print", "accessibility", "print_high_quality"]
        );
    }

    #[test]
    fn contradictory_grants_are_rejected_before_writing() {
        let annotate_without_modify = ProtectPermissions {
            annotate: true,
            ..ProtectPermissions::READ_ONLY
        };
        assert_eq!(
            annotate_without_modify.to_permission(),
            Err(SecurityError::PermissionConflict(
                "annotation requires modification permission"
            ))
        );
        let assemble_without_modify = ProtectPermissions {
            assemble: true,
            ..ProtectPermissions::READ_ONLY
        };
        assert!(matches!(
            assemble_without_modify.to_permission(),
            Err(SecurityError::PermissionConflict(_))
        ));
        let high_quality_without_print = ProtectPermissions {
            print: false,
            print_high_quality: true,
            ..ProtectPermissions::NONE
        };
        assert!(matches!(
            high_quality_without_print.to_permission(),
            Err(SecurityError::PermissionConflict(_))
        ));
    }

    #[test]
    fn debug_redacts_both_passwords_and_the_seed() {
        let options = ProtectOptions::new(b"user-secret")
            .with_owner_password(b"owner-secret")
            .with_id_seed(b"seed-secret".to_vec());
        let rendered = format!("{options:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("user-secret"));
        assert!(!rendered.contains("owner-secret"));
        assert!(!rendered.contains("seed-secret"));
    }

    #[test]
    fn encrypt_entry_detection_ignores_metadata_and_boundaries() {
        assert!(!has_encrypt_entry(
            b"%PDF-1.7\ntrailer\n<< /EncryptMetadata false >>"
        ));
        assert!(!has_encrypt_entry(b"%PDF-1.7\nsome stream bytes"));
        assert!(has_encrypt_entry(
            b"%PDF-1.7\ntrailer\n<< /Size 5 /Encrypt 4 0 R >>"
        ));
        assert!(has_encrypt_entry(
            b"%PDF-1.7\n/Encrypt 4 0 R\nstartxref\n0\n"
        ));
    }
}
