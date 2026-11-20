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

use crate::crypto::sha::sha256;
use crate::error::{Result, SecurityError};
use crate::permissions::Permission;

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

#[cfg(test)]
mod tests {
    use super::*;

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
}
