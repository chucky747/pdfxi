//! Injectable certificate-store abstraction for signing identities (T-0267).
//!
//! Acrobat can sign with an identity that lives in the operating-system
//! certificate store, where the private key never leaves the platform key
//! provider. Enumeration is inherently platform-specific, so this module keeps
//! it behind a narrow, injectable seam:
//!
//! - [`CertificateStore`] returns non-secret identity metadata only;
//! - [`StoreIdentity`] carries the subject, issuer, full serial bytes, SHA-256
//!   fingerprint, and a `has_private_key` flag — never a private key;
//! - [`platform_identity_store`] is the platform entry point.
//!
//! # This round's boundary
//!
//! The real Windows enumeration (`CertOpenStore` / `CertEnumCertificatesInStore`
//! and the CNG key handle) is **not** implemented in this round. The security
//! crate has no `windows` dependency and this round must not change the shared
//! workspace lock file, so [`UnavailableCertificateStore`] returns the typed
//! [`SecurityError::WindowsStoreUnavailable`] instead of pretending that the
//! store is empty. An empty result and an unavailable store are different
//! states and callers must display them differently.
//!
//! The FFI boundary, when added, belongs in this module alone, behind the same
//! trait, so callers and tests never link to the platform API. Tests inject a
//! fake store and never depend on the certificates installed on the host.

use crate::error::{Result, SecurityError};
use crate::x509::certificate::Certificate;

/// Non-secret metadata for one identity in a platform certificate store.
///
/// The value never holds private key bytes or a key handle. A
/// [`CertificateStore`] implementation records whether a usable private key is
/// present; obtaining and using that key remains the platform provider's job.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoreIdentity {
    subject_display: String,
    issuer_display: String,
    serial_number: Vec<u8>,
    certificate_fingerprint_sha256: [u8; 32],
    has_private_key: bool,
    provider_name: Option<String>,
}

impl StoreIdentity {
    /// Build identity metadata from a parsed certificate.
    ///
    /// `provider_name` names the platform key provider (for example a CNG
    /// provider label) when a private key is present. It must not contain key
    /// material.
    pub fn from_certificate(
        certificate: &Certificate,
        has_private_key: bool,
        provider_name: Option<String>,
    ) -> Self {
        Self {
            subject_display: certificate.subject().display_rfc4514(),
            issuer_display: certificate.issuer().display_rfc4514(),
            serial_number: certificate.serial_number_bytes().to_vec(),
            certificate_fingerprint_sha256: certificate.fingerprint_sha256(),
            has_private_key,
            provider_name,
        }
    }

    /// Deterministic RFC 4514 subject display.
    pub fn subject_display(&self) -> &str {
        &self.subject_display
    }

    /// Deterministic RFC 4514 issuer display.
    pub fn issuer_display(&self) -> &str {
        &self.issuer_display
    }

    /// Full canonical serial-number bytes.
    pub fn serial_number_bytes(&self) -> &[u8] {
        &self.serial_number
    }

    /// SHA-256 fingerprint of the certificate DER.
    pub const fn certificate_fingerprint_sha256(&self) -> [u8; 32] {
        self.certificate_fingerprint_sha256
    }

    /// Return `true` when a usable private key is present in the store.
    pub const fn has_private_key(&self) -> bool {
        self.has_private_key
    }

    /// Platform key-provider label, when one was reported.
    pub fn provider_name(&self) -> Option<&str> {
        self.provider_name.as_deref()
    }
}

/// A source of signing identities.
///
/// Implementations are read-only and must return only metadata. A store that
/// cannot be opened returns [`SecurityError::WindowsStoreUnavailable`] or
/// [`SecurityError::WindowsStoreFailed`], never an empty list, so a caller can
/// distinguish "no identities" from "the store could not be read".
pub trait CertificateStore {
    /// Enumerate every identity visible in the store.
    fn enumerate_identities(&self) -> Result<Vec<StoreIdentity>>;

    /// Enumerate only identities that report a usable private key.
    ///
    /// The default implementation filters [`Self::enumerate_identities`]; it
    /// preserves the store's insertion order.
    fn identities_with_private_key(&self) -> Result<Vec<StoreIdentity>> {
        Ok(self
            .enumerate_identities()?
            .into_iter()
            .filter(StoreIdentity::has_private_key)
            .collect())
    }
}

/// The platform certificate store.
///
/// On this round the returned value has no implementation behind it: every
/// enumeration returns [`SecurityError::WindowsStoreUnavailable`]. This is the
/// honest typed result, not a fake empty store. Replacing this type with a
/// Windows-backed implementation changes no caller code.
#[derive(Debug, Clone, Copy, Default)]
pub struct UnavailableCertificateStore;

impl CertificateStore for UnavailableCertificateStore {
    fn enumerate_identities(&self) -> Result<Vec<StoreIdentity>> {
        Err(SecurityError::WindowsStoreUnavailable)
    }
}

/// Return the platform identity store.
///
/// The current implementation is [`UnavailableCertificateStore`] on every
/// platform. Callers must handle the typed unavailable error and must not show
/// identity or trust as verified when it occurs.
pub const fn platform_identity_store() -> UnavailableCertificateStore {
    UnavailableCertificateStore
}

#[cfg(test)]
mod tests {
    use super::*;

    fn identity(name: &str, has_private_key: bool) -> StoreIdentity {
        let pem = std::fs::read(
            std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
                .join("tests/fixtures/pfx")
                .join(name),
        )
        .expect("read committed PEM fixture");
        let parsed = crate::trust_anchors::TrustAnchorBundle::from_pem(&pem).expect("parse PEM");
        let der = parsed.anchors()[0].der();
        let certificate = Certificate::parse(der).expect("parse fixture certificate");
        StoreIdentity::from_certificate(
            &certificate,
            has_private_key,
            has_private_key.then(|| "test-provider".to_owned()),
        )
    }

    struct FakeStore {
        identities: Vec<StoreIdentity>,
    }

    impl CertificateStore for FakeStore {
        fn enumerate_identities(&self) -> Result<Vec<StoreIdentity>> {
            Ok(self.identities.clone())
        }
    }

    struct FailingStore;

    impl CertificateStore for FailingStore {
        fn enumerate_identities(&self) -> Result<Vec<StoreIdentity>> {
            Err(SecurityError::WindowsStoreFailed(5))
        }
    }

    #[test]
    fn fake_store_enumerates_and_filters_private_keys() {
        let store = FakeStore {
            identities: vec![identity("leaf.pem", true), identity("root.pem", false)],
        };
        let all = store.enumerate_identities().expect("enumerate");
        assert_eq!(all.len(), 2);
        assert_eq!(all[0].serial_number_bytes().len(), 16);
        assert_eq!(
            all[0].certificate_fingerprint_sha256(),
            identity("leaf.pem", true).certificate_fingerprint_sha256()
        );
        assert!(
            all[0]
                .subject_display()
                .contains("PDFXI Test Signing Identity")
        );
        assert!(all[0].has_private_key());
        assert_eq!(all[0].provider_name(), Some("test-provider"));

        let with_keys = store.identities_with_private_key().expect("filter");
        assert_eq!(with_keys.len(), 1);
        assert!(with_keys[0].has_private_key());
        assert!(!all[1].has_private_key());
    }

    #[test]
    fn unavailable_store_is_typed_not_empty() {
        let store = platform_identity_store();
        assert_eq!(
            store.enumerate_identities(),
            Err(SecurityError::WindowsStoreUnavailable)
        );
        assert_eq!(
            store.identities_with_private_key(),
            Err(SecurityError::WindowsStoreUnavailable)
        );
    }

    #[test]
    fn store_failure_is_typed() {
        assert_eq!(
            FailingStore.enumerate_identities(),
            Err(SecurityError::WindowsStoreFailed(5))
        );
    }

    #[test]
    fn store_identity_debug_carries_no_key_material() {
        let identity = identity("leaf.pem", true);
        let text = format!("{identity:?}");
        assert!(!text.contains("PRIVATE"));
        assert!(text.contains("test-provider"));
    }
}
