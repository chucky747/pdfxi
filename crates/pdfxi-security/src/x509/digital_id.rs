//! Digital IDs: certificate plus an opaque private-key handle.
//!
//! A [`DigitalId`] never contains a private key. It holds a certificate and an
//! optional [`KeyHandle`] — a caller-owned object that can sign but cannot be
//! inspected for secret bytes by this crate. [`DigitalIdStore`] adds import,
//! export, duplicate detection, and lookup by subject or serial.

use std::fmt;
use std::sync::Arc;

use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;
use crate::x509::name::Name;

fn hex(bytes: &[u8]) -> String {
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            out.push_str(&format!("{byte:02x}"));
            out
        })
}

/// An opaque handle to a private key stored outside this crate.
///
/// Implementations may wrap a hardware token, OS key store, or HSM. Only
/// non-secret metadata is exposed; the key bytes are never returned, cloned
/// into this crate, or logged.
pub trait KeyHandle: fmt::Debug {
    /// Backend identifier, such as `windows-cng` or `pkcs11`.
    fn provider(&self) -> &str;

    /// Stable opaque identifier used for persistence and duplicate detection.
    ///
    /// This is a handle label, never the key itself.
    fn handle_id(&self) -> &str;

    /// Whether the underlying key is hardware-backed (metadata only).
    fn is_hardware_backed(&self) -> bool;
}

/// A metadata-only [`KeyHandle`] for callers that do not need a signing backend.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OpaqueKeyHandle {
    provider: String,
    handle_id: String,
    hardware_backed: bool,
}

impl OpaqueKeyHandle {
    /// Create a handle from non-secret metadata.
    ///
    /// Rejects empty fields and control characters so handle labels cannot
    /// corrupt exported metadata.
    pub fn new(
        provider: impl Into<String>,
        handle_id: impl Into<String>,
        hardware_backed: bool,
    ) -> Result<Self> {
        let provider = provider.into();
        let handle_id = handle_id.into();
        if provider.is_empty()
            || handle_id.is_empty()
            || provider.chars().any(char::is_control)
            || handle_id.chars().any(char::is_control)
        {
            return Err(SecurityError::Configuration(
                "key handle metadata must be non-empty and control-character free".to_owned(),
            ));
        }
        Ok(Self {
            provider,
            handle_id,
            hardware_backed,
        })
    }
}

impl KeyHandle for OpaqueKeyHandle {
    fn provider(&self) -> &str {
        &self.provider
    }

    fn handle_id(&self) -> &str {
        &self.handle_id
    }

    fn is_hardware_backed(&self) -> bool {
        self.hardware_backed
    }
}

fn key_identity(handle: Option<&Arc<dyn KeyHandle>>) -> Option<String> {
    handle.map(|key| format!("{}:{}", key.provider(), key.handle_id()))
}

/// A certificate paired with optional, opaque private-key metadata.
#[derive(Clone)]
pub struct DigitalId {
    label: String,
    certificate: Certificate,
    key_handle: Option<Arc<dyn KeyHandle>>,
    imported_at: u64,
}

impl DigitalId {
    /// Parse `certificate_der` and build a digital ID.
    ///
    /// `imported_at` is a caller-supplied Unix timestamp; this crate never
    /// reads the system clock.
    pub fn new(
        label: impl Into<String>,
        certificate_der: &[u8],
        key_handle: Option<Arc<dyn KeyHandle>>,
        imported_at: u64,
    ) -> Result<Self> {
        let certificate = Certificate::parse(certificate_der)?;
        Self::from_certificate(label, certificate, key_handle, imported_at)
    }

    /// Build a digital ID from an already-parsed certificate.
    ///
    /// Rejects an empty label or a label containing control characters.
    pub fn from_certificate(
        label: impl Into<String>,
        certificate: Certificate,
        key_handle: Option<Arc<dyn KeyHandle>>,
        imported_at: u64,
    ) -> Result<Self> {
        let label = label.into();
        if label.is_empty() || label.chars().any(char::is_control) {
            return Err(SecurityError::Configuration(
                "digital ID label must be non-empty and control-character free".to_owned(),
            ));
        }
        Ok(Self {
            label,
            certificate,
            key_handle,
            imported_at,
        })
    }

    /// Return the caller-assigned label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Return the certificate.
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// Return the opaque key handle, when one was supplied.
    pub fn key_handle(&self) -> Option<&Arc<dyn KeyHandle>> {
        self.key_handle.as_ref()
    }

    /// Return the caller-supplied import timestamp.
    pub const fn imported_at(&self) -> u64 {
        self.imported_at
    }

    /// Return the deterministic subject display string.
    pub fn subject_display(&self) -> String {
        self.certificate.subject().display_rfc4514()
    }

    /// Return the deterministic issuer display string.
    pub fn issuer_display(&self) -> String {
        self.certificate.issuer().display_rfc4514()
    }

    /// Return the certificate serial number.
    pub const fn serial_number(&self) -> u64 {
        self.certificate.serial_number()
    }

    /// Return the non-secret key identity (`provider:handle`), when present.
    pub fn key_identity(&self) -> Option<String> {
        key_identity(self.key_handle.as_ref())
    }

    fn same_identity(&self, other: &Self) -> bool {
        self.certificate.der() == other.certificate.der()
            && key_identity(self.key_handle.as_ref()) == key_identity(other.key_handle.as_ref())
    }

    /// Export non-secret metadata for this digital ID.
    pub fn metadata(&self) -> DigitalIdMetadata {
        DigitalIdMetadata {
            label: self.label.clone(),
            subject_display: self.subject_display(),
            issuer_display: self.issuer_display(),
            serial_number: self.serial_number(),
            certificate_fingerprint_sha256: hex(&self.certificate.fingerprint_sha256()),
            key_provider: self
                .key_handle
                .as_ref()
                .map(|key| key.provider().to_owned()),
            key_handle_id: self
                .key_handle
                .as_ref()
                .map(|key| key.handle_id().to_owned()),
            hardware_backed: self.key_handle.as_ref().map(|key| key.is_hardware_backed()),
            imported_at: self.imported_at,
        }
    }
}

impl PartialEq for DigitalId {
    fn eq(&self, other: &Self) -> bool {
        self.label == other.label
            && self.certificate == other.certificate
            && key_identity(self.key_handle.as_ref()) == key_identity(other.key_handle.as_ref())
            && self.imported_at == other.imported_at
    }
}

impl Eq for DigitalId {}

impl fmt::Debug for DigitalId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DigitalId")
            .field("label", &self.label)
            .field("certificate", &self.certificate)
            .field("key_identity", &self.key_identity())
            .field("imported_at", &self.imported_at)
            .finish()
    }
}

/// Serializable, non-secret metadata for one digital ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DigitalIdMetadata {
    /// Caller-assigned label.
    pub label: String,
    /// Deterministic subject display string.
    pub subject_display: String,
    /// Deterministic issuer display string.
    pub issuer_display: String,
    /// Certificate serial number.
    pub serial_number: u64,
    /// Lowercase hex SHA-256 fingerprint of the certificate DER.
    pub certificate_fingerprint_sha256: String,
    /// Opaque key backend name, if a key handle was supplied.
    pub key_provider: Option<String>,
    /// Opaque key handle label, if a key handle was supplied.
    pub key_handle_id: Option<String>,
    /// Hardware-backed flag, if a key handle was supplied.
    pub hardware_backed: Option<bool>,
    /// Caller-supplied import timestamp.
    pub imported_at: u64,
}

/// An in-memory collection of digital IDs with deterministic lookups.
#[derive(Debug, Default)]
pub struct DigitalIdStore {
    ids: Vec<DigitalId>,
}

impl DigitalIdStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Insert a digital ID, rejecting duplicates by label or identity.
    ///
    /// Identity is the pair of exact certificate DER and opaque key identity;
    /// two IDs with different labels but the same identity are duplicates.
    pub fn insert(&mut self, id: DigitalId) -> Result<()> {
        if self
            .ids
            .iter()
            .any(|existing| existing.label == id.label || existing.same_identity(&id))
        {
            return Err(SecurityError::DigitalIdDuplicate);
        }
        self.ids.push(id);
        Ok(())
    }

    /// Remove and return the digital ID with the given label.
    pub fn remove(&mut self, label: &str) -> Result<DigitalId> {
        let position = self
            .ids
            .iter()
            .position(|id| id.label == label)
            .ok_or(SecurityError::DigitalIdNotFound)?;
        Ok(self.ids.remove(position))
    }

    /// Return the digital ID with the given label, if present.
    pub fn get(&self, label: &str) -> Option<&DigitalId> {
        self.ids.iter().find(|id| id.label == label)
    }

    /// Return all digital IDs in insertion order.
    pub fn all(&self) -> &[DigitalId] {
        &self.ids
    }

    /// Return the number of stored IDs.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Return `true` when the store is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Return IDs whose subject has the given deterministic display string.
    pub fn find_by_subject_display(&self, subject: &str) -> Vec<&DigitalId> {
        self.ids
            .iter()
            .filter(|id| id.subject_display() == subject)
            .collect()
    }

    /// Return IDs whose subject `Name` matches by deterministic display.
    pub fn find_by_subject(&self, subject: &Name) -> Vec<&DigitalId> {
        self.find_by_subject_display(&subject.display_rfc4514())
    }

    /// Return IDs with the given certificate serial number.
    pub fn find_by_serial(&self, serial: u64) -> Vec<&DigitalId> {
        self.ids
            .iter()
            .filter(|id| id.serial_number() == serial)
            .collect()
    }

    /// Export metadata for every ID in insertion order.
    pub fn export_metadata(&self) -> Vec<DigitalIdMetadata> {
        self.ids.iter().map(DigitalId::metadata).collect()
    }

    /// Import a digital ID from metadata, certificate DER, and a key handle.
    ///
    /// Fails with [`SecurityError::DigitalIdMetadataMismatch`] if any metadata
    /// field disagrees with the certificate or key handle, then delegates to
    /// [`Self::insert`] for duplicate detection.
    pub fn import(
        &mut self,
        metadata: &DigitalIdMetadata,
        certificate_der: &[u8],
        key_handle: Option<Arc<dyn KeyHandle>>,
    ) -> Result<()> {
        let certificate = Certificate::parse(certificate_der)?;
        if hex(&certificate.fingerprint_sha256()) != metadata.certificate_fingerprint_sha256
            || certificate.subject().display_rfc4514() != metadata.subject_display
            || certificate.issuer().display_rfc4514() != metadata.issuer_display
            || certificate.serial_number() != metadata.serial_number
            || key_identity(key_handle.as_ref()) != metadata.key_identity()
            || key_handle.as_ref().map(|key| key.is_hardware_backed()) != metadata.hardware_backed
        {
            return Err(SecurityError::DigitalIdMetadataMismatch);
        }
        let id = DigitalId::from_certificate(
            metadata.label.clone(),
            certificate,
            key_handle,
            metadata.imported_at,
        )?;
        self.insert(id)
    }
}

impl DigitalIdMetadata {
    fn key_identity(&self) -> Option<String> {
        match (&self.key_provider, &self.key_handle_id) {
            (Some(provider), Some(handle)) => Some(format!("{provider}:{handle}")),
            (None, None) => None,
            _ => Some(String::new()),
        }
    }
}
