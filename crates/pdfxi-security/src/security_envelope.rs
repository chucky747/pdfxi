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
//! The envelope provides **integrity** (a tamper-evident digest over the whole
//! container) and a **declarative access description** (who may open it and what
//! each recipient may do). It deliberately does **not** provide confidentiality:
//! the payload is stored verbatim. Confidentiality is the job of
//! [`crate::certificate_encryption`] and the standard security handler; callers
//! that need both should encrypt the payload first and then wrap the ciphertext.
//! Enforcement of the declared permissions is a local reader policy; no rights
//! management server is involved.
//!
//! No document contents or secrets are logged. Error messages carry only caller
//! supplied labels and structural reasons.
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
use crate::crypto::sha::sha256;
use crate::error::{Result, SecurityError};
use crate::permissions::{Permission, Permissions};

/// Magic bytes prefixing every serialized local security envelope.
const MAGIC: &[u8; 9] = b"PDFXI-ENV";
/// The only envelope format version this implementation understands.
const VERSION: u8 = 1;
/// Length of the SHA-256 integrity tag.
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SecurityEnvelope {
    version: u8,
    payload: Vec<u8>,
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
    integrity_tag: [u8; TAG_LEN],
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

    /// Look up a recipient by label.
    pub fn recipient(&self, label: &str) -> Option<&EnvelopeRecipient> {
        self.recipients
            .iter()
            .find(|recipient| recipient.label == label)
    }

    /// Verify that the integrity tag still matches the envelope contents.
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
    pub fn to_bytes(&self) -> Vec<u8> {
        let mut body = self.canonical_body();
        body.extend_from_slice(&self.integrity_tag);
        body
    }

    /// Parse an envelope from bytes without verifying its integrity tag.
    ///
    /// Call [`Self::verify_integrity`] or [`open_security_envelope`] to detect
    /// tampering.
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
        if version != VERSION {
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
    };
    envelope.integrity_tag = sha256(&envelope.canonical_body());
    Ok(envelope)
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
}

/// An envelope whose integrity tag has been verified.
#[derive(Debug, Clone)]
pub struct OpenedSecurityEnvelope {
    payload: Vec<u8>,
    permissions: Permission,
    recipients: Vec<EnvelopeRecipient>,
    report: EnvelopeReport,
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

/// Parse and verify a local security envelope.
///
/// # Errors
///
/// [`SecurityError::EnvelopeMalformed`] for a malformed container or
/// [`SecurityError::EnvelopeIntegrityFailed`] when the integrity tag does not
/// match (tampering).
pub fn open_security_envelope(bytes: &[u8]) -> Result<OpenedSecurityEnvelope> {
    let envelope = SecurityEnvelope::from_bytes(bytes)?;
    if !envelope.verify_integrity() {
        return Err(SecurityError::EnvelopeIntegrityFailed);
    }
    Ok(OpenedSecurityEnvelope {
        payload: envelope.payload.clone(),
        permissions: envelope.permissions,
        recipients: envelope.recipients.clone(),
        report: EnvelopeReport {
            version: envelope.version,
            recipient_count: envelope.recipients.len(),
            permission_bits: envelope.permissions.bits(),
            payload_len: envelope.payload.len(),
            integrity_ok: true,
        },
    })
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
