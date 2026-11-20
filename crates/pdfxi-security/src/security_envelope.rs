//! Local security envelopes for document payloads (SEC-010).
//!
//! A [`SecurityEnvelope`] wraps a document payload together with a recipient
//! list, a set of permissions, and a SHA-256 integrity tag. It is a **local file
//! format**: a deterministic, length-prefixed binary container that can be
//! written to disk and read back with [`open_security_envelope`]. It is *not*
//! Adobe's server-backed Rights Management format and performs no network
//! access, licensing calls, or host-clock reads.
//!
//! # What this provides — and what it does not
//!
//! Every envelope carries an unkeyed SHA-256 integrity tag over the whole
//! container. An envelope built with a caller-supplied authentication key also
//! carries a keyed HMAC-SHA256 tag; [`open_security_envelope_with_key`] verifies
//! it and reports [`Integrity::Authenticated`]. An unkeyed envelope reports
//! [`Integrity::Unkeyed`]: its digest is reproducible by anyone, so both the
//! payload **and the declared permissions must be treated as untrusted** until
//! the caller authenticates them by other means. The `Debug` implementations of
//! [`SecurityEnvelope`] and [`OpenedSecurityEnvelope`] render the payload as a
//! length only.
//!
//! The envelope provides a **declarative access description** (who may open it
//! and what each recipient may do). It deliberately does **not** provide
//! confidentiality: the payload is stored verbatim. Confidentiality is the job
//! of [`crate::certificate_encryption`] and the standard security handler;
//! callers that need both should encrypt the payload first and then wrap the
//! ciphertext. Enforcement of the declared permissions is a local reader
//! policy; no rights management server is involved.
//!
//! No document contents or secrets are logged. Error messages and `Debug`
//! output carry only caller supplied labels, lengths, and structural reasons.
//!
//! # Certificate envelopes (SEC-010 completion)
//!
//! In addition to the integrity-only [`SecurityEnvelope`] above, this module
//! provides [`CertificateEnvelope`], which *does* provide confidentiality. It
//! composes [`crate::certificate_encryption`]: a caller-supplied
//! [`crate::certificate_encryption::RecipientList`] identifies the recipients, a
//! single content-encryption key is wrapped once per recipient through the
//! caller-supplied [`crate::certificate_encryption::KeyWrapProvider`] boundary,
//! and an [`EncryptionScope`] records whether the whole document or only named
//! attachments are encrypted. Both the scope decision and the
//! [`EnvelopeDeliveryReport`] are deterministic and contain no key material.

use std::fmt;

use crate::certificate_encryption::{
    ContentCipher, EnvelopedData, KeyWrapProvider, RecipientList,
    build_envelope as build_enveloped_data,
};
use crate::crypto::constant_time_eq;
use crate::crypto::sha::{hmac_sha256, sha256};
use crate::error::{Result, SecurityError};
use crate::permissions::{Permission, Permissions};

/// Magic bytes prefixing every serialized local security envelope.
const MAGIC: &[u8; 9] = b"PDFXI-ENV";
/// Legacy, unkeyed envelope format version (still read for backward compatibility).
const VERSION: u8 = 1;
/// Authenticated envelope format version, carrying an HMAC-SHA256 tag.
const VERSION_AUTHENTICATED: u8 = 2;
/// Length of the SHA-256 integrity and authentication tags.
const TAG_LEN: usize = 32;

/// One recipient of a [`SecurityEnvelope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeRecipient {
    label: String,
    permissions: Permission,
}

impl EnvelopeRecipient {
    /// Create a recipient with a label and the permissions granted to them.
    pub fn new(label: impl Into<String>, permissions: Permission) -> Self {
        Self {
            label: label.into(),
            permissions,
        }
    }

    /// The recipient's label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The permissions granted to this recipient.
    pub fn permissions(&self) -> Permission {
        self.permissions
    }

    /// Return `true` when this recipient may perform `operation`.
    pub fn allows(&self, operation: Permission) -> bool {
        self.permissions.contains(operation)
    }
}

/// A local security envelope wrapping a payload and its access description.
///
/// `Debug` is implemented manually: the payload is rendered as a byte length,
/// never as document contents.
#[derive(Clone, PartialEq, Eq)]
pub struct SecurityEnvelope {
    version: u8,
    payload: Vec<u8>,
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
    integrity_tag: [u8; TAG_LEN],
    /// Keyed authentication tag; present only for authenticated (version 2)
    /// envelopes and never rendered in `Debug`.
    mac_tag: Option<[u8; TAG_LEN]>,
}

impl fmt::Debug for SecurityEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SecurityEnvelope")
            .field("version", &self.version)
            .field("payload", &format_args!("<{} bytes>", self.payload.len()))
            .field(
                "permissions",
                &format_args!("{:#x}", self.permissions.bits()),
            )
            .field("recipient_count", &self.recipients.len())
            .field("integrity_tag", &"<redacted>")
            .field("authenticated", &self.mac_tag.is_some())
            .finish()
    }
}

impl SecurityEnvelope {
    /// Format version of this envelope.
    pub fn version(&self) -> u8 {
        self.version
    }

    /// The wrapped payload bytes (stored verbatim; not encrypted).
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// Envelope-wide permissions.
    pub fn permissions(&self) -> Permission {
        self.permissions
    }

    /// Declared recipients in insertion order.
    pub fn recipients(&self) -> &[EnvelopeRecipient] {
        &self.recipients
    }

    /// The SHA-256 integrity tag over the canonical envelope body.
    pub fn integrity_tag(&self) -> &[u8; TAG_LEN] {
        &self.integrity_tag
    }

    /// Whether this envelope carries a keyed HMAC-SHA256 authentication tag.
    ///
    /// Unauthenticated envelopes report [`Integrity::Unkeyed`] when opened and
    /// their permissions must be treated as untrusted.
    pub fn is_authenticated(&self) -> bool {
        self.mac_tag.is_some()
    }

    /// Verify the keyed authentication tag with `key`, if this envelope has one.
    ///
    /// Returns `false` for unkeyed envelopes or an empty key. The comparison is
    /// constant-time with respect to the tag bytes.
    pub fn verify_authentication(&self, key: &[u8]) -> bool {
        match &self.mac_tag {
            Some(tag) if !key.is_empty() => {
                constant_time_eq(tag, &hmac_sha256(key, &self.canonical_body()))
            }
            _ => false,
        }
    }

    /// Look up a recipient by label.
    pub fn recipient(&self, label: &str) -> Option<&EnvelopeRecipient> {
        self.recipients
            .iter()
            .find(|recipient| recipient.label == label)
    }

    /// Verify the unkeyed SHA-256 digest over the canonical envelope body.
    ///
    /// This is a tamper-evidence check only: anyone can recompute the digest,
    /// so it does not authenticate the payload or permissions. Call
    /// [`Self::verify_authentication`] with a trusted key to authenticate them.
    pub fn verify_integrity(&self) -> bool {
        sha256(&self.canonical_body()) == self.integrity_tag
    }

    /// Ensure `label` exists and grants `operation`, otherwise return a typed
    /// error.
    ///
    /// # Errors
    ///
    /// [`SecurityError::EnvelopeRecipientNotFound`] for an unknown label or
    /// [`SecurityError::EnvelopePermissionDenied`] when the permission is absent.
    pub fn authorize(&self, label: &str, operation: Permission) -> Result<()> {
        let recipient = self
            .recipient(label)
            .ok_or_else(|| SecurityError::EnvelopeRecipientNotFound(label.to_owned()))?;
        if !recipient.allows(operation) || !self.permissions.contains(operation) {
            return Err(SecurityError::EnvelopePermissionDenied("operation"));
        }
        Ok(())
    }

    /// Serialize the envelope to the deterministic local file format.
    ///
    /// Authenticated envelopes append the HMAC-SHA256 tag after the unkeyed
    /// integrity tag.
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = self.canonical_body();
        body.extend_from_slice(&self.integrity_tag);
        if let Some(tag) = &self.mac_tag {
            body.extend_from_slice(tag);
        }
        body
    }

    /// Parse an envelope from bytes without verifying its integrity tags.
    ///
    /// Call [`Self::verify_integrity`] / [`Self::verify_authentication`], or
    /// [`open_security_envelope`] / [`open_security_envelope_with_key`], to
    /// detect tampering.
    ///
    /// # Errors
    ///
    /// Returns [`SecurityError::EnvelopeMalformed`] for an unknown version or a
    /// truncated/internally inconsistent container.
    pub fn from_bytes(bytes: &[u8]) -> Result<Self> {
        let mut cursor = Cursor::new(bytes);
        if cursor.take(MAGIC.len())? != MAGIC {
            return Err(SecurityError::EnvelopeMalformed("bad magic".to_owned()));
        }
        let version = cursor.u8()?;
        if version != VERSION && version != VERSION_AUTHENTICATED {
            return Err(SecurityError::EnvelopeMalformed(format!(
                "unsupported version {version}"
            )));
        }
        let payload_len = cursor.u32()? as usize;
        let payload = cursor.take(payload_len)?.to_vec();
        let permissions = Permission::from_bits_checked(cursor.u32()?)
            .map_err(|_| SecurityError::EnvelopeMalformed("invalid permission bits".to_owned()))?;
        let recipient_count = cursor.u16()? as usize;
        let mut recipients = Vec::with_capacity(recipient_count);
        for _ in 0..recipient_count {
            let label_len = cursor.u16()? as usize;
            let label_bytes = cursor.take(label_len)?;
            let label = String::from_utf8(label_bytes.to_vec()).map_err(|_| {
                SecurityError::EnvelopeMalformed("invalid recipient label".to_owned())
            })?;
            let recipient_permissions =
                Permission::from_bits_checked(cursor.u32()?).map_err(|_| {
                    SecurityError::EnvelopeMalformed("invalid recipient permission bits".to_owned())
                })?;
            recipients.push(EnvelopeRecipient::new(label, recipient_permissions));
        }
        let integrity_tag: [u8; TAG_LEN] = cursor.take(TAG_LEN)?.try_into().map_err(|_| {
            SecurityError::EnvelopeMalformed("invalid integrity tag length".to_owned())
        })?;
        let mac_tag = if version == VERSION_AUTHENTICATED {
            Some(cursor.take(TAG_LEN)?.try_into().map_err(|_| {
                SecurityError::EnvelopeMalformed("invalid authentication tag length".to_owned())
            })?)
        } else {
            None
        };
        if !cursor.is_empty() {
            return Err(SecurityError::EnvelopeMalformed(
                "trailing bytes".to_owned(),
            ));
        }
        Ok(Self {
            version,
            payload,
            permissions,
            recipients,
            integrity_tag,
            mac_tag,
        })
    }

    fn canonical_body(&self) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(MAGIC);
        out.push(self.version);
        out.extend_from_slice(&(self.payload.len() as u32).to_le_bytes());
        out.extend_from_slice(&self.payload);
        out.extend_from_slice(&self.permissions.bits().to_le_bytes());
        out.extend_from_slice(&(self.recipients.len() as u16).to_le_bytes());
        for recipient in &self.recipients {
            let label = recipient.label.as_bytes();
            out.extend_from_slice(&(label.len() as u16).to_le_bytes());
            out.extend_from_slice(label);
            out.extend_from_slice(&recipient.permissions.bits().to_le_bytes());
        }
        out
    }
}

/// Build a local security envelope around a non-empty payload.
///
/// # Errors
///
/// [`SecurityError::EnvelopeEmptyPayload`] for an empty payload,
/// [`SecurityError::EnvelopeNoRecipients`] when no recipients are given,
/// [`SecurityError::EnvelopeMalformed`] for an empty label, and
/// [`SecurityError::EnvelopeDuplicateRecipient`] for duplicate labels.
pub fn build_security_envelope(
    payload: &[u8],
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
) -> Result<SecurityEnvelope> {
    if payload.is_empty() {
        return Err(SecurityError::EnvelopeEmptyPayload);
    }
    if recipients.is_empty() {
        return Err(SecurityError::EnvelopeNoRecipients);
    }
    let mut seen = std::collections::BTreeSet::new();
    for recipient in &recipients {
        if recipient.label.is_empty() {
            return Err(SecurityError::EnvelopeMalformed(
                "recipient label must not be empty".to_owned(),
            ));
        }
        if !seen.insert(recipient.label.as_str()) {
            return Err(SecurityError::EnvelopeDuplicateRecipient(
                recipient.label.clone(),
            ));
        }
    }
    let mut envelope = SecurityEnvelope {
        version: VERSION,
        payload: payload.to_vec(),
        permissions,
        recipients,
        integrity_tag: [0u8; TAG_LEN],
        mac_tag: None,
    };
    envelope.integrity_tag = sha256(&envelope.canonical_body());
    Ok(envelope)
}

/// Build an authenticated local security envelope around a non-empty payload.
///
/// The caller supplies `auth_key`, which is used only to compute an HMAC-SHA256
/// tag over the canonical envelope body; it is never stored in the envelope or
/// written to diagnostics. The key must be non-empty. Empty payloads, empty or
/// duplicate recipient lists, and empty labels are rejected exactly as in
/// [`build_security_envelope`].
///
/// An authenticated envelope must be opened with
/// [`open_security_envelope_with_key`] and the same key. Its permissions and
/// payload are authenticated; an unkeyed envelope's are not.
///
/// # Errors
///
/// [`SecurityError::EnvelopeAuthenticationRequired`] for an empty key, plus the
/// same typed errors as [`build_security_envelope`] for invalid payload or
/// recipients.
pub fn build_authenticated_security_envelope(
    payload: &[u8],
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
    auth_key: &[u8],
) -> Result<SecurityEnvelope> {
    if auth_key.is_empty() {
        return Err(SecurityError::EnvelopeAuthenticationRequired);
    }
    let mut envelope = build_security_envelope(payload, permissions, recipients)?;
    // The version byte is part of the canonical body, so switch it before
    // recomputing both tags.
    envelope.version = VERSION_AUTHENTICATED;
    envelope.integrity_tag = sha256(&envelope.canonical_body());
    envelope.mac_tag = Some(hmac_sha256(auth_key, &envelope.canonical_body()));
    Ok(envelope)
}

/// How an opened envelope's integrity was established.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Integrity {
    /// Only the unkeyed SHA-256 digest was checked.
    ///
    /// Anyone can recompute that digest, so the payload **and the declared
    /// permissions are untrusted**. Callers must not rely on the permissions
    /// for an authorization decision without authenticating them separately.
    Unkeyed,
    /// A keyed HMAC-SHA256 tag was verified with a caller-supplied key.
    ///
    /// The payload and permissions are authenticated to a party holding that
    /// key. This does not add confidentiality; the payload is still verbatim.
    Authenticated,
}

/// A deterministic report describing an opened envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EnvelopeReport {
    /// Envelope format version.
    pub version: u8,
    /// Number of declared recipients.
    pub recipient_count: usize,
    /// Envelope-wide permission bits.
    pub permission_bits: u32,
    /// Length of the wrapped payload in bytes.
    pub payload_len: usize,
    /// Whether the integrity tag matched on open.
    pub integrity_ok: bool,
    /// How integrity was established.
    ///
    /// With [`Integrity::Unkeyed`] the declared permissions are **untrusted**:
    /// the unkeyed digest is reproducible by an attacker.
    pub integrity: Integrity,
}

/// An envelope whose integrity tag has been verified.
///
/// `Debug` is implemented manually: the payload is rendered as a byte length,
/// never as document contents.
#[derive(Clone)]
pub struct OpenedSecurityEnvelope {
    payload: Vec<u8>,
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
    report: EnvelopeReport,
}

impl fmt::Debug for OpenedSecurityEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedSecurityEnvelope")
            .field("payload", &format_args!("<{} bytes>", self.payload.len()))
            .field(
                "permissions",
                &format_args!("{:#x}", self.permissions.bits()),
            )
            .field("recipient_count", &self.recipients.len())
            .field("report", &self.report)
            .finish()
    }
}

impl OpenedSecurityEnvelope {
    /// The recovered payload bytes.
    pub fn payload(&self) -> &[u8] {
        &self.payload
    }

    /// The recovered envelope-wide permissions.
    pub fn permissions(&self) -> Permission {
        self.permissions
    }

    /// The recovered recipient list.
    pub fn recipients(&self) -> &[EnvelopeRecipient] {
        &self.recipients
    }

    /// The deterministic open report.
    pub fn report(&self) -> &EnvelopeReport {
        &self.report
    }
}

/// Build the opened view of an envelope whose tags have already been checked.
fn finalize_open(envelope: SecurityEnvelope, integrity: Integrity) -> OpenedSecurityEnvelope {
    let report = EnvelopeReport {
        version: envelope.version,
        recipient_count: envelope.recipients.len(),
        permission_bits: envelope.permissions.bits(),
        payload_len: envelope.payload.len(),
        integrity_ok: true,
        integrity,
    };
    OpenedSecurityEnvelope {
        payload: envelope.payload,
        permissions: envelope.permissions,
        recipients: envelope.recipients,
        report,
    }
}

/// Parse and verify an unkeyed local security envelope.
///
/// This opens legacy, integration-only envelopes (version 1) and reports
/// [`Integrity::Unkeyed`]. Because the digest is unkeyed, the payload and the
/// declared permissions are **untrusted**. An authenticated envelope is
/// rejected here with [`SecurityError::EnvelopeAuthenticationRequired`]; use
/// [`open_security_envelope_with_key`] for it.
///
/// # Errors
///
/// [`SecurityError::EnvelopeMalformed`] for a malformed container,
/// [`SecurityError::EnvelopeAuthenticationRequired`] when a key is needed, or
/// [`SecurityError::EnvelopeIntegrityFailed`] on tampering.
pub fn open_security_envelope(bytes: &[u8]) -> Result<OpenedSecurityEnvelope> {
    let envelope = SecurityEnvelope::from_bytes(bytes)?;
    if envelope.is_authenticated() {
        return Err(SecurityError::EnvelopeAuthenticationRequired);
    }
    if !envelope.verify_integrity() {
        return Err(SecurityError::EnvelopeIntegrityFailed);
    }
    Ok(finalize_open(envelope, Integrity::Unkeyed))
}

/// Parse and verify an authenticated local security envelope with `auth_key`.
///
/// The key must be non-empty and the envelope must carry an HMAC-SHA256 tag.
/// Both the unkeyed digest and the keyed tag must match; a caller-supplied key
/// is therefore required and an unkeyed envelope is never accepted here.
/// Permissions in the resulting report are authenticated to a key holder.
///
/// # Errors
///
/// [`SecurityError::EnvelopeAuthenticationRequired`] for an empty key, an
/// unkeyed envelope, or an authenticated envelope opened without a key;
/// [`SecurityError::EnvelopeMalformed`] for a malformed container;
/// [`SecurityError::EnvelopeIntegrityFailed`] when the unkeyed digest fails;
/// [`SecurityError::EnvelopeAuthenticationFailed`] for a wrong key or a
/// recomputed unkeyed tag over modified contents.
pub fn open_security_envelope_with_key(
    bytes: &[u8],
    auth_key: &[u8],
) -> Result<OpenedSecurityEnvelope> {
    if auth_key.is_empty() {
        return Err(SecurityError::EnvelopeAuthenticationRequired);
    }
    let envelope = SecurityEnvelope::from_bytes(bytes)?;
    if !envelope.is_authenticated() {
        return Err(SecurityError::EnvelopeAuthenticationRequired);
    }
    if !envelope.verify_integrity() {
        return Err(SecurityError::EnvelopeIntegrityFailed);
    }
    if !envelope.verify_authentication(auth_key) {
        return Err(SecurityError::EnvelopeAuthenticationFailed);
    }
    Ok(finalize_open(envelope, Integrity::Authenticated))
}

/// A small bounds-checked byte cursor used by [`SecurityEnvelope::from_bytes`].
struct Cursor<'a> {
    data: &'a [u8],
    position: usize,
}

impl<'a> Cursor<'a> {
    fn new(data: &'a [u8]) -> Self {
        Self { data, position: 0 }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        let end = self
            .position
            .checked_add(length)
            .filter(|end| *end <= self.data.len())
            .ok_or_else(|| SecurityError::EnvelopeMalformed("truncated envelope".to_owned()))?;
        let slice = &self.data[self.position..end];
        self.position = end;
        Ok(slice)
    }

    fn u8(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }

    fn u16(&mut self) -> Result<u16> {
        let bytes: [u8; 2] = self
            .take(2)?
            .try_into()
            .map_err(|_| SecurityError::EnvelopeMalformed("truncated envelope".to_owned()))?;
        Ok(u16::from_le_bytes(bytes))
    }

    fn u32(&mut self) -> Result<u32> {
        let bytes: [u8; 4] = self
            .take(4)?
            .try_into()
            .map_err(|_| SecurityError::EnvelopeMalformed("truncated envelope".to_owned()))?;
        Ok(u32::from_le_bytes(bytes))
    }

    fn is_empty(&self) -> bool {
        self.position == self.data.len()
    }
}

/// The part of a document an envelope is asked to protect.
///
/// This is a declarative decision recorded in the envelope report; the actual
/// byte selection is performed by the caller that applies the envelope
/// (this crate never reads a document on disk).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EncryptionScope {
    /// Encrypt every object and stream of the document.
    WholeDocument,
    /// Encrypt only the named embedded attachments; all other content remains
    /// plaintext. Names are caller-supplied attachment identifiers.
    SelectedAttachments(Vec<String>),
}

impl EncryptionScope {
    /// Stable identifier used in reports (`whole_document` or
    /// `selected_attachments`).
    pub fn as_id(&self) -> &'static str {
        match self {
            Self::WholeDocument => "whole_document",
            Self::SelectedAttachments(_) => "selected_attachments",
        }
    }

    /// Return `true` when this scope protects the whole document.
    pub fn is_whole_document(&self) -> bool {
        matches!(self, Self::WholeDocument)
    }

    /// Selected attachment names, or an empty slice for a whole-document scope.
    pub fn attachment_names(&self) -> &[String] {
        match self {
            Self::WholeDocument => &[],
            Self::SelectedAttachments(names) => names,
        }
    }
}

/// Validate an [`EncryptionScope`], rejecting empty or duplicate attachment
/// names with typed errors.
fn validate_scope(scope: &EncryptionScope) -> Result<()> {
    if let EncryptionScope::SelectedAttachments(names) = scope {
        if names.is_empty() {
            return Err(SecurityError::EnvelopeEmptyAttachmentSelection);
        }
        let mut seen = std::collections::BTreeSet::new();
        for name in names {
            if name.is_empty() {
                return Err(SecurityError::EnvelopeMalformed(
                    "attachment name must not be empty".to_owned(),
                ));
            }
            if !seen.insert(name.as_str()) {
                return Err(SecurityError::EnvelopeDuplicateAttachment(name.clone()));
            }
        }
    }
    Ok(())
}

/// One recipient's wrapped-key entry in an [`EnvelopeDeliveryReport`].
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct RecipientDelivery {
    /// Recipient label supplied by the caller.
    pub label: String,
    /// Hex-encoded certificate fingerprint identifying the recipient.
    pub identifier: String,
    /// Length of this recipient's wrapped content-encryption key in bytes.
    pub wrapped_len: usize,
}

/// A deterministic report describing a built [`CertificateEnvelope`].
///
/// No field carries key material: only labels, lengths, and stable identifiers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnvelopeDeliveryReport {
    /// Scope identifier (`whole_document` or `selected_attachments`).
    pub scope: &'static str,
    /// Number of selected attachments (zero for a whole-document scope).
    pub attachment_count: usize,
    /// Selected attachment names in canonical (lexicographic) order.
    pub attachments: Vec<String>,
    /// Symmetric content-cipher identifier.
    pub cipher: &'static str,
    /// Encrypted payload length in bytes.
    pub ciphertext_len: usize,
    /// Per-recipient wrapped-key entries in canonical (label) order.
    pub recipients: Vec<RecipientDelivery>,
}

/// A certificate-addressed security envelope with confidentiality.
///
/// The encrypted content and per-recipient wrapped keys are produced by
/// [`crate::certificate_encryption`]. This crate holds no private key: wrapping
/// happens entirely through the caller-supplied [`KeyWrapProvider`]. The
/// [`EncryptionScope`] and [`EnvelopeDeliveryReport`] make the attachment
/// decision and the delivery shape explicit and deterministic.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CertificateEnvelope {
    scope: EncryptionScope,
    enveloped: EnvelopedData,
    report: EnvelopeDeliveryReport,
}

impl CertificateEnvelope {
    /// The attachment-encryption decision recorded in this envelope.
    pub fn scope(&self) -> &EncryptionScope {
        &self.scope
    }

    /// The deterministic delivery report.
    pub fn report(&self) -> &EnvelopeDeliveryReport {
        &self.report
    }

    /// The underlying certificate-encryption envelope (encrypted content plus
    /// wrapped keys). Its `Debug` implementation redacts all byte material.
    pub fn enveloped_data(&self) -> &EnvelopedData {
        &self.enveloped
    }

    /// Number of recipients addressed by this envelope.
    pub fn recipient_count(&self) -> usize {
        self.report.recipients.len()
    }

    /// Return `true` when a recipient with the given label was addressed.
    pub fn has_recipient(&self, label: &str) -> bool {
        self.report.recipients.iter().any(|row| row.label == label)
    }

    /// Deterministic CMS-style DER encoding of the encrypted content and the
    /// per-recipient wrapped keys.
    pub fn to_der(&self) -> Vec<u8> {
        self.enveloped.to_der()
    }
}

/// Bundled inputs for [`build_certificate_envelope`].
///
/// Grouping these fields keeps the builder signature small. The `Debug`
/// implementation redacts the plaintext, key, and IV so no secret or document
/// content is ever written to diagnostics.
pub struct CertificateEnvelopeInput<'a> {
    /// Plaintext payload to encrypt.
    pub plaintext: &'a [u8],
    /// Permissions granted to recipients.
    pub permissions: Permissions,
    /// Symmetric content cipher.
    pub cipher: ContentCipher,
    /// Content-encryption key matching `cipher.key_len()`.
    pub cek: &'a [u8],
    /// Caller-supplied IV, making encryption deterministic.
    pub iv: [u8; 16],
    /// Attachment-encryption decision recorded in the report.
    pub scope: EncryptionScope,
}

impl fmt::Debug for CertificateEnvelopeInput<'_> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("CertificateEnvelopeInput")
            .field(
                "plaintext",
                &format_args!("<{} bytes>", self.plaintext.len()),
            )
            .field(
                "permissions",
                &format_args!("{:#x}", self.permissions.bits().bits()),
            )
            .field("cipher", &self.cipher.as_str())
            .field("cek", &format_args!("<{} bytes>", self.cek.len()))
            .field("iv", &"<redacted>")
            .field("scope", &self.scope)
            .finish()
    }
}

/// Build a [`CertificateEnvelope`].
///
/// The input's `plaintext` must be non-empty, `recipients` non-empty and unique,
/// and `cek` must match `input.cipher.key_len()`; these are validated by the
/// underlying [`crate::certificate_encryption::build_envelope`]. The scope must
/// name at least one attachment when selecting attachments, without duplicates.
/// The provider's `wrap_key` is called exactly once per recipient, in recipient
/// order.
///
/// # Errors
///
/// - [`SecurityError::EnvelopeEmptyAttachmentSelection`] or
///   [`SecurityError::EnvelopeDuplicateAttachment`] for an invalid scope.
/// - Typed certificate-encryption errors for empty input, no/duplicate
///   recipients, wrong key length, or a provider wrap failure.
pub fn build_certificate_envelope(
    input: CertificateEnvelopeInput<'_>,
    recipients: &RecipientList,
    provider: &dyn KeyWrapProvider,
) -> Result<CertificateEnvelope> {
    let CertificateEnvelopeInput {
        plaintext,
        permissions,
        cipher,
        cek,
        iv,
        scope,
    } = input;
    validate_scope(&scope)?;
    let enveloped = build_enveloped_data(
        plaintext,
        recipients,
        permissions,
        cipher,
        cek,
        iv,
        provider,
    )?;

    let mut attachments = scope.attachment_names().to_vec();
    attachments.sort();
    let mut recipient_rows: Vec<RecipientDelivery> = enveloped
        .recipient_keys()
        .iter()
        .map(|entry| RecipientDelivery {
            label: entry.label().to_owned(),
            identifier: entry.identifier().to_owned(),
            wrapped_len: entry.wrapped_len(),
        })
        .collect();
    recipient_rows.sort();

    let report = EnvelopeDeliveryReport {
        scope: scope.as_id(),
        attachment_count: attachments.len(),
        attachments,
        cipher: cipher.as_str(),
        ciphertext_len: enveloped.ciphertext_len(),
        recipients: recipient_rows,
    };

    Ok(CertificateEnvelope {
        scope,
        enveloped,
        report,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::certificate_encryption::{InMemoryKeyWrapProvider, Recipient};
    use crate::der::{
        Tag, encode_integer, encode_null, encode_oid, encode_sequence, encode_tlv, write_tlv,
    };
    use crate::permissions::PasswordRole;
    use crate::x509::Certificate;
    use std::cell::RefCell;

    const OID_CN: [u32; 4] = [2, 5, 4, 3];

    fn der_name(cn: &str) -> Vec<u8> {
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        encode_sequence(&der_set_bytes(&encode_sequence(&ava)))
    }

    fn der_set_bytes(children: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_tlv(&mut out, Tag::SET, children);
        out
    }

    fn der_alg_id() -> Vec<u8> {
        let mut children = encode_oid(&[1, 2, 840, 113_549, 1, 1, 1]).expect("oid");
        children.extend_from_slice(&encode_null());
        encode_sequence(&children)
    }

    fn der_bit_string(payload: &[u8]) -> Vec<u8> {
        let mut content = vec![0u8];
        content.extend_from_slice(payload);
        encode_tlv(Tag::BIT_STRING, &content)
    }

    fn der_validity() -> Vec<u8> {
        let mut children = crate::der::encode_time(1_600_000_000).expect("time");
        children.extend_from_slice(&crate::der::encode_time(1_700_000_000).expect("time"));
        encode_sequence(&children)
    }

    fn test_certificate(seed: u8) -> Certificate {
        let subject = der_name(&format!("Recipient-{seed}"));
        let mut tbs = Vec::new();
        tbs.extend_from_slice(&encode_integer(seed as u64));
        tbs.extend_from_slice(&der_alg_id());
        tbs.extend_from_slice(&subject);
        tbs.extend_from_slice(&der_validity());
        tbs.extend_from_slice(&subject);
        let mut spki = der_alg_id();
        spki.extend_from_slice(&der_bit_string(&[seed; 32]));
        tbs.extend_from_slice(&encode_sequence(&spki));
        let tbs = encode_sequence(&tbs);
        let mut cert = tbs.clone();
        cert.extend_from_slice(&der_alg_id());
        cert.extend_from_slice(&der_bit_string(&crate::crypto::sha::sha256(&tbs)[..16]));
        Certificate::parse(&encode_sequence(&cert)).expect("valid test certificate")
    }

    fn cert_recipients(specs: &[(&str, u8)]) -> RecipientList {
        let mut list = RecipientList::new();
        for (label, seed) in specs {
            list.add(Recipient::new(*label, test_certificate(*seed)).expect("recipient"))
                .expect("add");
        }
        list
    }

    fn cert_envelope(
        recipients: &RecipientList,
        cipher: ContentCipher,
        cek: &[u8],
        scope: EncryptionScope,
        provider: &dyn KeyWrapProvider,
    ) -> crate::error::Result<CertificateEnvelope> {
        build_certificate_envelope(
            CertificateEnvelopeInput {
                plaintext: b"confidential-payload",
                permissions: Permissions::new(Permission::PRINT, PasswordRole::User),
                cipher,
                cek,
                iv: [3u8; 16],
                scope,
            },
            recipients,
            provider,
        )
    }

    #[derive(Default)]
    struct CountingProvider {
        wrap_calls: RefCell<usize>,
        unwrap_calls: RefCell<usize>,
    }

    impl KeyWrapProvider for CountingProvider {
        fn wrap_key(&self, cek: &[u8], recipient: &Recipient) -> crate::error::Result<Vec<u8>> {
            *self.wrap_calls.borrow_mut() += 1;
            InMemoryKeyWrapProvider.wrap_key(cek, recipient)
        }

        fn unwrap_key(
            &self,
            wrapped: &[u8],
            recipient: &Recipient,
        ) -> crate::error::Result<Vec<u8>> {
            *self.unwrap_calls.borrow_mut() += 1;
            InMemoryKeyWrapProvider.unwrap_key(wrapped, recipient)
        }
    }

    fn recipients() -> Vec<EnvelopeRecipient> {
        vec![
            EnvelopeRecipient::new("alice", Permission::PRINT.union(Permission::COPY)),
            EnvelopeRecipient::new("bob", Permission::PRINT),
        ]
    }

    fn envelope() -> SecurityEnvelope {
        build_security_envelope(
            b"%PDF-1.7\npayload-bytes",
            Permission::PRINT.union(Permission::COPY),
            recipients(),
        )
        .expect("valid envelope")
    }

    #[test]
    fn round_trip_preserves_payload_recipients_and_permissions() {
        let built = envelope();
        let bytes = built.to_bytes();
        let opened = open_security_envelope(&bytes).expect("open");
        assert_eq!(opened.payload(), b"%PDF-1.7\npayload-bytes");
        assert_eq!(opened.recipients(), built.recipients());
        assert_eq!(opened.permissions(), built.permissions());
        assert_eq!(opened.report().version, 1);
        assert_eq!(opened.report().recipient_count, 2);
        assert!(opened.report().integrity_ok);
        assert_eq!(opened.report().integrity, Integrity::Unkeyed);
        assert!(!built.is_authenticated());
    }

    #[test]
    fn envelope_debug_redacts_payload_bytes() {
        let mut secret = b"TOP-SECRET-PAYLOAD-BYTES".to_vec();
        secret.extend_from_slice(&[0xDE, 0xAD, 0xBE, 0xEF]);
        let key = b"diagnostic-test-key";
        let unkeyed =
            build_security_envelope(&secret, Permission::PRINT, recipients()).expect("unkeyed");
        let authenticated =
            build_authenticated_security_envelope(&secret, Permission::PRINT, recipients(), key)
                .expect("authenticated");
        let opened = open_security_envelope(&unkeyed.to_bytes()).expect("open unkeyed");
        let opened_auth = open_security_envelope_with_key(&authenticated.to_bytes(), key)
            .expect("open authenticated");
        let length_marker = format!("<{} bytes>", secret.len());

        for rendered in [
            format!("{unkeyed:?}"),
            format!("{authenticated:?}"),
            format!("{opened:?}"),
            format!("{opened_auth:?}"),
        ] {
            assert!(
                !rendered.contains("TOP-SECRET-PAYLOAD-BYTES"),
                "payload text leaked into Debug: {rendered}"
            );
            assert!(
                !rendered.contains("222, 173, 190, 239"),
                "raw payload bytes leaked into Debug: {rendered}"
            );
            assert!(
                rendered.contains(&length_marker),
                "Debug must show payload length only: {rendered}"
            );
        }
    }

    #[test]
    fn authenticated_envelope_round_trip_is_reported_as_authenticated() {
        let key = b"round-trip-key";
        let payload = b"%PDF-1.7\nauthenticated-payload";
        let built = build_authenticated_security_envelope(
            payload,
            Permission::PRINT.union(Permission::COPY),
            recipients(),
            key,
        )
        .expect("build");
        assert!(built.is_authenticated());
        assert!(built.verify_integrity());
        assert!(built.verify_authentication(key));
        assert!(!built.verify_authentication(b"wrong-key"));
        assert!(!built.verify_authentication(b""));

        let bytes = built.to_bytes();
        let reparsed = SecurityEnvelope::from_bytes(&bytes).expect("reparse");
        assert_eq!(reparsed.version(), 2);
        assert!(reparsed.is_authenticated());
        assert_eq!(reparsed.integrity_tag(), built.integrity_tag());

        let opened = open_security_envelope_with_key(&bytes, key).expect("open");
        assert_eq!(opened.payload(), payload);
        assert_eq!(opened.recipients(), built.recipients());
        assert_eq!(opened.permissions(), built.permissions());
        assert!(opened.report().integrity_ok);
        assert_eq!(opened.report().integrity, Integrity::Authenticated);
    }

    #[test]
    fn tampered_payload_with_recomputed_unkeyed_tag_is_rejected_when_key_is_required() {
        let key = b"required-auth-key";
        let built = build_authenticated_security_envelope(
            b"%PDF-1.7\nprotect-me",
            Permission::PRINT,
            recipients(),
            key,
        )
        .expect("build");
        let mut tampered = SecurityEnvelope::from_bytes(&built.to_bytes()).expect("parse");
        tampered.payload[0] ^= 0x01;
        // An attacker freely recomputes the unkeyed SHA-256 tag over the edited
        // body, but cannot produce a valid HMAC without the key.
        tampered.integrity_tag = sha256(&tampered.canonical_body());
        assert!(tampered.verify_integrity(), "unkeyed tag was recomputed");
        assert!(!tampered.verify_authentication(key));
        let bytes = tampered.to_bytes();
        assert!(matches!(
            open_security_envelope_with_key(&bytes, key),
            Err(SecurityError::EnvelopeAuthenticationFailed)
        ));
        // An authenticated envelope is never accepted by the unkeyed opener.
        assert!(matches!(
            open_security_envelope(&bytes),
            Err(SecurityError::EnvelopeAuthenticationRequired)
        ));

        // The same attack against the declared permissions is rejected too.
        let mut tampered_perms = SecurityEnvelope::from_bytes(&built.to_bytes()).expect("parse");
        tampered_perms.permissions = Permission::COPY;
        tampered_perms.integrity_tag = sha256(&tampered_perms.canonical_body());
        assert!(tampered_perms.verify_integrity());
        assert!(matches!(
            open_security_envelope_with_key(&tampered_perms.to_bytes(), key),
            Err(SecurityError::EnvelopeAuthenticationFailed)
        ));
    }

    #[test]
    fn wrong_key_is_rejected_and_empty_key_is_a_typed_error() {
        let key = b"correct-key";
        let bytes =
            build_authenticated_security_envelope(b"payload", Permission::PRINT, recipients(), key)
                .expect("build")
                .to_bytes();
        assert!(matches!(
            open_security_envelope_with_key(&bytes, b"incorrect-key"),
            Err(SecurityError::EnvelopeAuthenticationFailed)
        ));
        assert!(matches!(
            open_security_envelope_with_key(&bytes, b""),
            Err(SecurityError::EnvelopeAuthenticationRequired)
        ));
        assert!(matches!(
            build_authenticated_security_envelope(b"payload", Permission::PRINT, recipients(), b""),
            Err(SecurityError::EnvelopeAuthenticationRequired)
        ));
    }

    #[test]
    fn unkeyed_envelope_is_never_accepted_by_the_keyed_opener() {
        let bytes = build_security_envelope(b"legacy payload", Permission::PRINT, recipients())
            .expect("build")
            .to_bytes();
        assert!(matches!(
            open_security_envelope_with_key(&bytes, b"any-key"),
            Err(SecurityError::EnvelopeAuthenticationRequired)
        ));
        let opened = open_security_envelope(&bytes).expect("legacy open");
        assert_eq!(opened.report().integrity, Integrity::Unkeyed);
    }

    #[test]
    fn authenticated_envelope_without_a_mac_tag_is_malformed() {
        let bytes = build_authenticated_security_envelope(
            b"payload",
            Permission::PRINT,
            recipients(),
            b"key",
        )
        .expect("build")
        .to_bytes();
        let missing_mac = &bytes[..bytes.len() - TAG_LEN];
        assert!(matches!(
            SecurityEnvelope::from_bytes(missing_mac),
            Err(SecurityError::EnvelopeMalformed(_))
        ));
        assert!(matches!(
            open_security_envelope_with_key(missing_mac, b"key"),
            Err(SecurityError::EnvelopeMalformed(_))
        ));
    }

    #[test]
    fn serialization_is_deterministic() {
        let bytes_a = envelope().to_bytes();
        let bytes_b = envelope().to_bytes();
        assert_eq!(bytes_a, bytes_b);
        assert!(bytes_a.starts_with(MAGIC));
    }

    #[test]
    fn tampered_payload_fails_with_typed_integrity_error() {
        let mut bytes = envelope().to_bytes();
        let index = MAGIC.len() + 1 + 4 + 5; // inside the payload
        bytes[index] ^= 0x01;
        assert!(matches!(
            open_security_envelope(&bytes),
            Err(SecurityError::EnvelopeIntegrityFailed)
        ));
    }

    #[test]
    fn tampered_recipient_label_fails_integrity() {
        let mut bytes = envelope().to_bytes();
        // Flip one ASCII byte of the first recipient label ('a' -> '`'); the
        // container stays structurally valid but the digest no longer matches.
        let label = b"alice";
        let index = bytes
            .windows(label.len())
            .position(|window| window == label)
            .expect("label is present");
        bytes[index] ^= 0x01;
        assert!(matches!(
            open_security_envelope(&bytes),
            Err(SecurityError::EnvelopeIntegrityFailed)
        ));
    }

    #[test]
    fn empty_payload_and_no_recipients_are_typed_errors() {
        assert_eq!(
            build_security_envelope(b"", Permission::PRINT, recipients()),
            Err(SecurityError::EnvelopeEmptyPayload)
        );
        assert_eq!(
            build_security_envelope(b"payload", Permission::PRINT, Vec::new()),
            Err(SecurityError::EnvelopeNoRecipients)
        );
    }

    #[test]
    fn duplicate_and_empty_labels_are_typed_errors() {
        let dup = vec![
            EnvelopeRecipient::new("alice", Permission::PRINT),
            EnvelopeRecipient::new("alice", Permission::COPY),
        ];
        assert_eq!(
            build_security_envelope(b"payload", Permission::PRINT, dup),
            Err(SecurityError::EnvelopeDuplicateRecipient(
                "alice".to_owned()
            ))
        );
        let empty = vec![EnvelopeRecipient::new("", Permission::PRINT)];
        assert!(matches!(
            build_security_envelope(b"payload", Permission::PRINT, empty),
            Err(SecurityError::EnvelopeMalformed(_))
        ));
    }

    #[test]
    fn authorization_honors_recipient_and_envelope_permissions() {
        let built = envelope();
        assert!(built.authorize("alice", Permission::COPY).is_ok());
        assert_eq!(
            built.authorize("bob", Permission::COPY),
            Err(SecurityError::EnvelopePermissionDenied("operation"))
        );
        assert_eq!(
            built.authorize("carol", Permission::PRINT),
            Err(SecurityError::EnvelopeRecipientNotFound("carol".to_owned()))
        );
    }

    #[test]
    fn malformed_bytes_are_typed_errors_not_panics() {
        assert!(matches!(
            open_security_envelope(b"short"),
            Err(SecurityError::EnvelopeMalformed(_))
        ));
        let mut bad_version = envelope().to_bytes();
        bad_version[MAGIC.len()] = 99;
        assert!(matches!(
            open_security_envelope(&bad_version),
            Err(SecurityError::EnvelopeMalformed(_))
        ));
    }

    // ---- SEC-010 completion: certificate envelopes -----------------------

    #[test]
    fn certificate_envelope_recipients_are_ordered_and_deduplicated() {
        let list = cert_recipients(&[("carol", 3), ("alice", 1), ("bob", 2)]);
        let env = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            EncryptionScope::WholeDocument,
            &InMemoryKeyWrapProvider,
        )
        .expect("build");
        let labels: Vec<&str> = env
            .report()
            .recipients
            .iter()
            .map(|row| row.label.as_str())
            .collect();
        assert_eq!(labels, vec!["alice", "bob", "carol"]);
        assert_eq!(env.recipient_count(), 3);
        assert!(env.has_recipient("bob"));
        assert!(!env.has_recipient("nobody"));

        // Duplicate labels are rejected at the recipient-list boundary.
        let mut dup = cert_recipients(&[("alice", 1)]);
        assert_eq!(
            dup.add(Recipient::new("alice", test_certificate(9)).expect("recipient")),
            Err(SecurityError::CertificateEncryptionDuplicateRecipient)
        );
    }

    #[test]
    fn wrap_provider_is_invoked_once_per_recipient() {
        let list = cert_recipients(&[("alice", 1), ("bob", 2)]);
        let provider = CountingProvider::default();
        let env = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            EncryptionScope::WholeDocument,
            &provider,
        )
        .expect("build");
        assert_eq!(*provider.wrap_calls.borrow(), 2);
        assert_eq!(*provider.unwrap_calls.borrow(), 0);
        assert_eq!(env.report().recipients.len(), 2);
        // Wrapped keys are one 8-byte semiblock longer than a 16-byte CEK.
        assert!(
            env.report()
                .recipients
                .iter()
                .all(|row| row.wrapped_len == 24)
        );
    }

    #[test]
    fn attachment_scope_selects_only_named_attachments() {
        let list = cert_recipients(&[("alice", 1)]);
        let selected = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            EncryptionScope::SelectedAttachments(vec!["report.pdf".into(), "chart.xlsx".into()]),
            &InMemoryKeyWrapProvider,
        )
        .expect("selected");
        assert_eq!(selected.report().scope, "selected_attachments");
        assert_eq!(selected.report().attachment_count, 2);
        assert_eq!(
            selected.report().attachments,
            vec!["chart.xlsx", "report.pdf"]
        );
        assert!(!selected.scope().is_whole_document());
        assert_eq!(EncryptionScope::WholeDocument.as_id(), "whole_document");

        let whole = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            EncryptionScope::WholeDocument,
            &InMemoryKeyWrapProvider,
        )
        .expect("whole");
        assert_eq!(whole.report().scope, "whole_document");
        assert_eq!(whole.report().attachment_count, 0);
        assert!(whole.report().attachments.is_empty());
        assert!(whole.scope().is_whole_document());
        assert!(whole.scope().attachment_names().is_empty());
    }

    #[test]
    fn empty_recipients_is_a_typed_error() {
        let empty = RecipientList::new();
        assert_eq!(
            cert_envelope(
                &empty,
                ContentCipher::Aes128Cbc,
                &[7u8; 16],
                EncryptionScope::WholeDocument,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionNoRecipients)
        );
    }

    #[test]
    fn empty_and_duplicate_attachment_selection_are_typed_errors() {
        let list = cert_recipients(&[("alice", 1)]);
        assert_eq!(
            cert_envelope(
                &list,
                ContentCipher::Aes128Cbc,
                &[7u8; 16],
                EncryptionScope::SelectedAttachments(Vec::new()),
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::EnvelopeEmptyAttachmentSelection)
        );
        assert_eq!(
            cert_envelope(
                &list,
                ContentCipher::Aes128Cbc,
                &[7u8; 16],
                EncryptionScope::SelectedAttachments(vec!["a.pdf".into(), "a.pdf".into()]),
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::EnvelopeDuplicateAttachment(
                "a.pdf".to_owned()
            ))
        );
    }

    #[test]
    fn invalid_cek_length_is_a_typed_error() {
        let list = cert_recipients(&[("alice", 1)]);
        // A 16-byte CEK cannot drive AES-256-CBC (which needs 32 bytes).
        assert_eq!(
            cert_envelope(
                &list,
                ContentCipher::Aes256Cbc,
                &[7u8; 16],
                EncryptionScope::WholeDocument,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionInvalidKeyLength)
        );
    }

    #[test]
    fn certificate_envelope_is_deterministic_and_redacts_secrets() {
        let list = cert_recipients(&[("alice", 1), ("bob", 2)]);
        let scope =
            || EncryptionScope::SelectedAttachments(vec!["doc.pdf".into(), "img.png".into()]);
        let a = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            scope(),
            &InMemoryKeyWrapProvider,
        )
        .expect("a");
        let b = cert_envelope(
            &list,
            ContentCipher::Aes128Cbc,
            &[7u8; 16],
            scope(),
            &InMemoryKeyWrapProvider,
        )
        .expect("b");
        assert_eq!(a.report(), b.report());
        assert_eq!(a.to_der(), b.to_der());
        let rendered = format!("{a:?}");
        assert!(!rendered.contains("confidential-payload"));
        // EnvelopedData's Debug redacts the key commitment.
        assert!(rendered.contains("<redacted>"));
        assert_eq!(a.enveloped_data().cipher(), ContentCipher::Aes128Cbc);
    }

    #[test]
    fn envelope_input_debug_redacts_plaintext_and_key() {
        let input = CertificateEnvelopeInput {
            plaintext: b"super-secret-document",
            permissions: Permissions::new(Permission::PRINT, PasswordRole::User),
            cipher: ContentCipher::Aes128Cbc,
            cek: &[0xABu8; 16],
            iv: [0xCDu8; 16],
            scope: EncryptionScope::WholeDocument,
        };
        let rendered = format!("{input:?}");
        assert!(!rendered.contains("super-secret-document"));
        assert!(!rendered.contains("abab"));
        assert!(!rendered.contains("cdcd"));
        assert!(rendered.contains("<redacted>"));
    }
}
