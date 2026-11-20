//! Certificate-based (public-key) encryption model (SEC-003).
//!
//! This module models a CMS/PKCS#7-style *enveloped* content encryption without
//! ever holding a private key. A caller supplies a list of recipient
//! [`Certificate`]s and a content-encryption key (CEK). The CEK encrypts the
//! content with the AES CBC primitives in [`crate::crypto::aes`], and a separate
//! per-recipient *wrapped key* is produced by the caller-provided
//! [`KeyWrapProvider`]. Opening the envelope unwraps the CEK for one recipient,
//! checks a SHA-256 key commitment (which distinguishes a wrong recipient from
//! tampered ciphertext), and decrypts the content.
//!
//! The CEK is wrapped with **RFC 3394 AES Key Wrap** ([`crate::crypto::keywrap`])
//! under a key-encryption key (KEK). AES-KW is authenticated: a wrong KEK or a
//! modified wrapped key is rejected by its integrity check instead of yielding
//! a corrupt CEK.
//!
//! # Trust boundary
//!
//! This is a clean-room *model*, not an implementation of Adobe's public-key
//! handlers. The caller-supplied [`KeyWrapProvider`] is the host boundary: a
//! real host derives the KEK from the recipient certificate using RSA/ECDH or a
//! KMS and supplies it through [`AesKwKeyWrapProvider`]. This crate never holds
//! a private key. [`InMemoryKeyWrapProvider`] is retained for API/test
//! compatibility and still performs real AES-KW, but it derives its KEK from
//! the public certificate fingerprint, so it offers no confidentiality and is
//! suitable only for deterministic tests. No private key, password, plaintext,
//! ciphertext, wrapped key, KEK, or key commitment is ever written to
//! diagnostics; [`Debug`] redacts every byte field.
//!
//! Envelopes carry the effective [`Permissions`] granted to recipients, so a
//! decryption result can be checked against the rights the sender intended.
//! A deterministic DER encoding of the envelope is available through
//! [`EnvelopedData::to_der`] using the [`crate::der`] primitives.

use std::fmt;

use crate::crypto::aes::{decrypt_cbc, encrypt_cbc};
use crate::crypto::keywrap::{aes_key_unwrap, aes_key_wrap};
use crate::crypto::sha::sha256;
use crate::der::{
    Tag, encode_integer, encode_null, encode_octet_string, encode_oid, encode_sequence, encode_tlv,
    write_tlv,
};
use crate::error::{Result, SecurityError};
use crate::permissions::{Permission, Permissions};
use crate::x509::certificate::Certificate;

/// OID `1.2.840.113549.1.7.3` — `envelopedData`.
const OID_ENVELOPED_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 3];
/// OID `1.2.840.113549.1.7.1` — `data`.
const OID_DATA: [u32; 7] = [1, 2, 840, 113_549, 1, 7, 1];
/// OID `2.16.840.1.101.3.4.1.6` — `aes128-CBC`.
const OID_AES128_CBC: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 1, 6];
/// OID `2.16.840.1.101.3.4.1.42` — `aes256-CBC`.
const OID_AES256_CBC: [u32; 9] = [2, 16, 840, 1, 101, 3, 4, 1, 42];

/// The symmetric cipher used to encrypt envelope content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ContentCipher {
    /// AES-128 in CBC mode with PKCS#7 padding.
    Aes128Cbc,
    /// AES-256 in CBC mode with PKCS#7 padding.
    Aes256Cbc,
}

impl ContentCipher {
    /// Required content-encryption key length in bytes.
    pub const fn key_len(self) -> usize {
        match self {
            Self::Aes128Cbc => 16,
            Self::Aes256Cbc => 32,
        }
    }

    /// Stable identifier including the mode.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Aes128Cbc => "aes128-cbc",
            Self::Aes256Cbc => "aes256-cbc",
        }
    }

    fn oid(self) -> &'static [u32] {
        match self {
            Self::Aes128Cbc => &OID_AES128_CBC,
            Self::Aes256Cbc => &OID_AES256_CBC,
        }
    }
}

/// A certificate recipient of an envelope.
///
/// The certificate is used only as public material (its DER SHA-256 fingerprint
/// identifies the recipient); no private key is stored here.
#[derive(Clone, PartialEq, Eq)]
pub struct Recipient {
    label: String,
    certificate: Certificate,
}

impl Recipient {
    /// Create a recipient from a label and certificate.
    ///
    /// Returns [`SecurityError::CertificateEncryptionInvalidCertificate`] when
    /// `label` is empty or the certificate carries no public key bits.
    pub fn new(label: impl Into<String>, certificate: Certificate) -> Result<Self> {
        let label = label.into();
        if label.is_empty() || certificate.subject_public_key_info().key_bits().is_empty() {
            return Err(SecurityError::CertificateEncryptionInvalidCertificate);
        }
        Ok(Self { label, certificate })
    }

    /// The caller-supplied recipient label.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// The recipient certificate (public material only).
    pub const fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// Hex-encoded SHA-256 fingerprint of the recipient certificate.
    pub fn identifier(&self) -> String {
        hex(&self.certificate.fingerprint_sha256())
    }
}

impl fmt::Debug for Recipient {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Recipient")
            .field("label", &self.label)
            .field("identifier", &self.identifier())
            .field("certificate", &self.certificate)
            .finish()
    }
}

/// An ordered, unique collection of envelope recipients.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RecipientList {
    recipients: Vec<Recipient>,
}

impl RecipientList {
    /// Create an empty recipient list.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a recipient, rejecting duplicate labels or certificate identities.
    pub fn add(&mut self, recipient: Recipient) -> Result<()> {
        if self.recipients.iter().any(|existing| {
            existing.label == recipient.label || existing.identifier() == recipient.identifier()
        }) {
            return Err(SecurityError::CertificateEncryptionDuplicateRecipient);
        }
        self.recipients.push(recipient);
        Ok(())
    }

    /// Builder form of [`Self::add`].
    pub fn with(mut self, recipient: Recipient) -> Result<Self> {
        self.add(recipient)?;
        Ok(self)
    }

    /// Number of recipients.
    pub fn len(&self) -> usize {
        self.recipients.len()
    }

    /// Return `true` when the list is empty.
    pub fn is_empty(&self) -> bool {
        self.recipients.is_empty()
    }

    /// Return the recipient at `index`.
    pub fn get(&self, index: usize) -> Option<&Recipient> {
        self.recipients.get(index)
    }

    /// Find a recipient by label.
    pub fn find(&self, label: &str) -> Option<&Recipient> {
        self.recipients
            .iter()
            .find(|recipient| recipient.label == label)
    }

    /// Iterate recipients in insertion order.
    pub fn iter(&self) -> impl Iterator<Item = &Recipient> {
        self.recipients.iter()
    }
}

/// A caller-supplied boundary for wrapping and unwrapping content-encryption
/// keys to and from recipient public keys.
///
/// Implementations own all key material; this crate never stores or logs a
/// private key. Returning [`SecurityError::CertificateEncryptionKeyWrapFailed`]
/// or [`SecurityError::CertificateEncryptionKeyUnwrapFailed`] maps directly to
/// the corresponding typed error.
pub trait KeyWrapProvider {
    /// Wrap (encrypt) `cek` for `recipient`, returning the wrapped key bytes.
    fn wrap_key(&self, cek: &[u8], recipient: &Recipient) -> Result<Vec<u8>>;

    /// Unwrap (decrypt) `wrapped` using the key belonging to `recipient`.
    fn unwrap_key(&self, wrapped: &[u8], recipient: &Recipient) -> Result<Vec<u8>>;
}

/// Derive a deterministic 32-byte KEK for a recipient from its public
/// certificate fingerprint.
///
/// This is used only by [`InMemoryKeyWrapProvider`] for deterministic tests.
/// Because the fingerprint is public, the KEK is **not secret**; real
/// confidentiality requires a caller-supplied KEK via [`AesKwKeyWrapProvider`].
fn test_double_kek(recipient: &Recipient) -> [u8; 32] {
    let fingerprint = recipient.certificate.fingerprint_sha256();
    let mut seed = Vec::with_capacity(fingerprint.len() + 22);
    seed.extend_from_slice(&fingerprint);
    seed.extend_from_slice(b"pdfxi-aeskw-kek-v2");
    sha256(&seed)
}

/// An RFC 3394 AES-KW key-wrap provider holding a caller-supplied KEK.
///
/// This is the recommended production provider: the host establishes a
/// key-encryption key for the recipient (for example by unwrapping an RSA/ECDH
/// ephemeral key or by querying a KMS) and passes it to [`Self::new`]. This
/// crate never persists or logs the KEK; `Debug` reports only its length.
#[derive(Clone)]
pub struct AesKwKeyWrapProvider {
    kek: Vec<u8>,
}

impl AesKwKeyWrapProvider {
    /// Create a provider from a 16-, 24-, or 32-byte key-encryption key.
    ///
    /// Returns [`SecurityError::AesKwInvalidKekLength`] for any other length.
    pub fn new(kek: &[u8]) -> Result<Self> {
        if !matches!(kek.len(), 16 | 24 | 32) {
            return Err(SecurityError::AesKwInvalidKekLength(kek.len()));
        }
        Ok(Self { kek: kek.to_vec() })
    }

    /// Length of the key-encryption key in bytes.
    pub fn kek_len(&self) -> usize {
        self.kek.len()
    }
}

impl fmt::Debug for AesKwKeyWrapProvider {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("AesKwKeyWrapProvider")
            .field("kek", &format_args!("<{} bytes>", self.kek.len()))
            .finish()
    }
}

impl KeyWrapProvider for AesKwKeyWrapProvider {
    fn wrap_key(&self, cek: &[u8], _recipient: &Recipient) -> Result<Vec<u8>> {
        aes_key_wrap(&self.kek, cek).map_err(|_| SecurityError::CertificateEncryptionKeyWrapFailed)
    }

    fn unwrap_key(&self, wrapped: &[u8], _recipient: &Recipient) -> Result<Vec<u8>> {
        aes_key_unwrap(&self.kek, wrapped)
            .map_err(|_| SecurityError::CertificateEncryptionKeyUnwrapFailed)
    }
}

/// A deterministic AES-KW provider for tests, keyed by the recipient fingerprint.
///
/// This type is retained for API compatibility with earlier releases. It uses
/// the real RFC 3394 AES-KW algorithm (wrapped keys are authenticated and eight
/// bytes longer than the CEK), but derives its KEK from *public* certificate
/// material, so it provides no confidentiality. Use [`AesKwKeyWrapProvider`]
/// with a caller-held secret KEK for real envelopes.
#[derive(Debug, Clone, Copy, Default)]
pub struct InMemoryKeyWrapProvider;

impl KeyWrapProvider for InMemoryKeyWrapProvider {
    fn wrap_key(&self, cek: &[u8], recipient: &Recipient) -> Result<Vec<u8>> {
        let kek = test_double_kek(recipient);
        aes_key_wrap(&kek, cek).map_err(|_| SecurityError::CertificateEncryptionKeyWrapFailed)
    }

    fn unwrap_key(&self, wrapped: &[u8], recipient: &Recipient) -> Result<Vec<u8>> {
        let kek = test_double_kek(recipient);
        aes_key_unwrap(&kek, wrapped)
            .map_err(|_| SecurityError::CertificateEncryptionKeyUnwrapFailed)
    }
}

/// One recipient's wrapped key in an envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct RecipientEnvelopeKey {
    label: String,
    identifier: String,
    wrapped_key: Vec<u8>,
}

impl RecipientEnvelopeKey {
    /// Recipient label this wrapped key belongs to.
    pub fn label(&self) -> &str {
        &self.label
    }

    /// Recipient certificate fingerprint (hex).
    pub fn identifier(&self) -> &str {
        &self.identifier
    }

    /// Length of the wrapped key in bytes (value is redacted from `Debug`).
    pub fn wrapped_len(&self) -> usize {
        self.wrapped_key.len()
    }
}

impl fmt::Debug for RecipientEnvelopeKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("RecipientEnvelopeKey")
            .field("label", &self.label)
            .field("identifier", &self.identifier)
            .field(
                "wrapped_key",
                &format_args!("<{} bytes>", self.wrapped_key.len()),
            )
            .finish()
    }
}

/// A CMS-style enveloped-content value: encrypted content plus one wrapped key
/// per recipient and the sender-granted permissions.
#[derive(Clone, PartialEq, Eq)]
pub struct EnvelopedData {
    cipher: ContentCipher,
    iv: [u8; 16],
    recipients: Vec<RecipientEnvelopeKey>,
    ciphertext: Vec<u8>,
    key_commitment: [u8; 32],
    permissions: Permissions,
}

impl EnvelopedData {
    /// Symmetric content cipher.
    pub const fn cipher(&self) -> ContentCipher {
        self.cipher
    }

    /// Recipient wrapped-key entries in insertion order.
    pub fn recipient_keys(&self) -> &[RecipientEnvelopeKey] {
        &self.recipients
    }

    /// Ciphertext length in bytes (value is redacted from `Debug`).
    pub fn ciphertext_len(&self) -> usize {
        self.ciphertext.len()
    }

    /// Effective permissions granted by the sender to recipients.
    pub const fn permissions(&self) -> &Permissions {
        &self.permissions
    }

    /// Return `true` when the granted permissions allow `permission`.
    pub fn allows(&self, permission: Permission) -> bool {
        self.permissions.allows(permission)
    }

    /// Encode the envelope as a deterministic CMS/PKCS#7-style DER structure.
    ///
    /// The layout is `ContentInfo { contentType = envelopedData, content [0] {
    /// EnvelopedData { version, recipientInfos SET OF, encryptedContentInfo } }
    /// }`. Values are encoded with [`crate::der`] primitives; no key material or
    /// plaintext is logged.
    pub fn to_der(&self) -> Vec<u8> {
        let mut recipient_infos = Vec::new();
        for entry in &self.recipients {
            let mut info = encode_integer(0);
            let mut issuer_and_serial = encode_tlv(Tag::UTF8_STRING, entry.label.as_bytes());
            issuer_and_serial.extend_from_slice(&encode_integer(0));
            info.extend_from_slice(&encode_sequence(&issuer_and_serial));
            let mut wrap_alg = oid_bytes(&[1, 2, 840, 113_549, 1, 1, 1]);
            wrap_alg.extend_from_slice(&encode_null());
            info.extend_from_slice(&encode_sequence(&wrap_alg));
            info.extend_from_slice(&encode_octet_string(&entry.wrapped_key));
            recipient_infos.extend_from_slice(&encode_sequence(&info));
        }

        let mut encrypted_content = oid_bytes(&OID_DATA);
        let mut content_alg = oid_bytes(self.cipher.oid());
        content_alg.extend_from_slice(&encode_null());
        encrypted_content.extend_from_slice(&encode_sequence(&content_alg));
        encrypted_content.extend_from_slice(&encode_octet_string(&self.ciphertext));

        let mut enveloped = encode_integer(0);
        enveloped.extend_from_slice(&der_set(&recipient_infos));
        enveloped.extend_from_slice(&encode_sequence(&encrypted_content));

        let mut content = oid_bytes(&OID_ENVELOPED_DATA);
        content.extend_from_slice(
            &crate::der::encode_context(0, true, &encode_sequence(&enveloped)).unwrap_or_default(),
        );
        encode_sequence(&content)
    }
}

impl fmt::Debug for EnvelopedData {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("EnvelopedData")
            .field("cipher", &self.cipher.as_str())
            .field("recipients", &self.recipients)
            .field(
                "ciphertext",
                &format_args!("<{} bytes>", self.ciphertext.len()),
            )
            .field("key_commitment", &"<redacted>")
            .field(
                "permissions",
                &format_args!("{:#x}", self.permissions.bits().bits()),
            )
            .finish()
    }
}

fn der_set(children: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(children.len() + 4);
    write_tlv(&mut out, Tag::SET, children);
    out
}

/// Encode one of this module's fixed, well-formed OID constants.
///
/// Invariant: every caller passes a hardcoded ARC slice that `encode_oid`
/// accepts (short arcs, no leading zero sub-identifier), so the `Result` cannot
/// be `Err` here. `unwrap_or_default` is used only to avoid a non-test `expect`;
/// on the impossible error path the empty bytes produce malformed DER rather
/// than a panic, which callers reject at parse time.
fn oid_bytes(oid: &[u32]) -> Vec<u8> {
    encode_oid(oid).unwrap_or_default()
}

fn hex(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push_str(&format!("{byte:02x}"));
    }
    out
}

/// Build a certificate-encryption envelope.
///
/// `plaintext` must be non-empty, `cek` must match `cipher.key_len()`, and
/// `recipients` must be non-empty and unique. The IV is supplied by the caller
/// so encryption is deterministic and never reads a random source.
///
/// # Errors
///
/// Typed errors cover empty plaintext, no/duplicate recipients, wrong key
/// length, provider wrap failure, and structure violations.
pub fn build_envelope(
    plaintext: &[u8],
    recipients: &RecipientList,
    permissions: Permissions,
    cipher: ContentCipher,
    cek: &[u8],
    iv: [u8; 16],
    provider: &dyn KeyWrapProvider,
) -> Result<EnvelopedData> {
    if plaintext.is_empty() {
        return Err(SecurityError::CertificateEncryptionEmptyPlaintext);
    }
    if recipients.is_empty() {
        return Err(SecurityError::CertificateEncryptionNoRecipients);
    }
    if cek.len() != cipher.key_len() {
        return Err(SecurityError::CertificateEncryptionInvalidKeyLength);
    }

    let ciphertext = encrypt_cbc(cek, &iv, plaintext)?;
    let key_commitment = sha256(cek);
    let mut entries = Vec::with_capacity(recipients.len());
    for recipient in recipients.iter() {
        let wrapped = provider.wrap_key(cek, recipient)?;
        if wrapped.is_empty() {
            return Err(SecurityError::CertificateEncryptionKeyWrapFailed);
        }
        entries.push(RecipientEnvelopeKey {
            label: recipient.label().to_owned(),
            identifier: recipient.identifier(),
            wrapped_key: wrapped,
        });
    }

    Ok(EnvelopedData {
        cipher,
        iv,
        recipients: entries,
        ciphertext,
        key_commitment,
        permissions,
    })
}

/// The result of successfully opening an envelope.
#[derive(Clone, PartialEq, Eq)]
pub struct OpenedEnvelope {
    /// Recovered plaintext bytes.
    pub plaintext: Vec<u8>,
    /// Label of the recipient whose key opened the envelope.
    pub recipient_label: String,
    /// Permissions granted by the sender.
    pub permissions: Permissions,
}

impl fmt::Debug for OpenedEnvelope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("OpenedEnvelope")
            .field(
                "plaintext",
                &format_args!("<{} bytes>", self.plaintext.len()),
            )
            .field("recipient_label", &self.recipient_label)
            .field(
                "permissions",
                &format_args!("{:#x}", self.permissions.bits().bits()),
            )
            .finish()
    }
}

impl OpenedEnvelope {
    /// Verify that the sender granted `permission`, or return a typed error.
    pub fn require_permission(&self, permission: Permission) -> Result<()> {
        if self.permissions.allows(permission) {
            Ok(())
        } else {
            Err(SecurityError::CertificateEncryptionPermissionDenied(
                permission_label(permission),
            ))
        }
    }
}

fn permission_label(permission: Permission) -> &'static str {
    if permission == Permission::PRINT {
        "print"
    } else if permission == Permission::MODIFY {
        "modify"
    } else if permission == Permission::COPY {
        "copy"
    } else if permission == Permission::ANNOTATE {
        "annotate"
    } else if permission == Permission::FILL_FORMS {
        "fill-forms"
    } else if permission == Permission::ACCESSIBILITY {
        "accessibility"
    } else if permission == Permission::ASSEMBLE {
        "assemble"
    } else if permission == Permission::PRINT_HIGH_QUALITY {
        "print-high-quality"
    } else {
        "operation"
    }
}

/// Select a recipient entry from an envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecipientSelector<'a> {
    /// Select by the zero-based recipient index.
    Index(usize),
    /// Select by recipient label.
    Label(&'a str),
}

/// Open an envelope for one recipient using the caller's key-unwrap provider.
///
/// The key commitment distinguishes a wrong recipient (commitment mismatch)
/// from tampered ciphertext (decryption/integrity failure).
///
/// # Errors
///
/// - [`SecurityError::CertificateEncryptionRecipientNotFound`] /
///   [`SecurityError::CertificateEncryptionRecipientIndexOutOfRange`] for a bad
///   selector.
/// - [`SecurityError::CertificateEncryptionWrongRecipient`] when the supplied
///   recipient identity does not match the envelope entry, or the unwrapped key
///   does not match the envelope commitment.
/// - [`SecurityError::CertificateEncryptionTamperedContent`] when content
///   decryption or its integrity check fails.
pub fn open_envelope(
    envelope: &EnvelopedData,
    selector: RecipientSelector<'_>,
    recipients: &RecipientList,
    provider: &dyn KeyWrapProvider,
) -> Result<OpenedEnvelope> {
    let entry = match selector {
        RecipientSelector::Index(index) => envelope
            .recipients
            .get(index)
            .ok_or(SecurityError::CertificateEncryptionRecipientIndexOutOfRange { index })?,
        RecipientSelector::Label(label) => envelope
            .recipients
            .iter()
            .find(|candidate| candidate.label == label)
            .ok_or(SecurityError::CertificateEncryptionRecipientNotFound)?,
    };
    let recipient = recipients
        .find(&entry.label)
        .ok_or(SecurityError::CertificateEncryptionRecipientNotFound)?;
    // Defense in depth: the certificate presented at open time must be the one
    // the envelope was addressed to. The key commitment below already rejects a
    // wrong unwrap, but checking the identity first yields the same typed result
    // before any key material is processed.
    if recipient.identifier() != entry.identifier {
        return Err(SecurityError::CertificateEncryptionWrongRecipient);
    }

    let cek = provider.unwrap_key(&entry.wrapped_key, recipient)?;
    if cek.is_empty() || sha256(&cek) != envelope.key_commitment {
        return Err(SecurityError::CertificateEncryptionWrongRecipient);
    }
    let plaintext = decrypt_cbc(&cek, &envelope.iv, &envelope.ciphertext)
        .map_err(|_| SecurityError::CertificateEncryptionTamperedContent)?;

    Ok(OpenedEnvelope {
        plaintext,
        recipient_label: entry.label.clone(),
        permissions: envelope.permissions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::{DerReader, encode_oid};
    use crate::permissions::PasswordRole;
    use crate::x509::Certificate;

    const OID_CN: [u32; 4] = [2, 5, 4, 3];

    fn name(cn: &str) -> Vec<u8> {
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        encode_sequence(&encode_set_bytes(&encode_sequence(&ava)))
    }

    fn encode_set_bytes(children: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        write_tlv(&mut out, Tag::SET, children);
        out
    }

    fn bit_string(payload: &[u8]) -> Vec<u8> {
        let mut content = vec![0u8];
        content.extend_from_slice(payload);
        encode_tlv(Tag::BIT_STRING, &content)
    }

    fn alg_id() -> Vec<u8> {
        let mut children = encode_oid(&[1, 2, 840, 113_549, 1, 1, 1]).expect("oid");
        children.extend_from_slice(&encode_null());
        encode_sequence(&children)
    }

    fn validity() -> Vec<u8> {
        let mut children = crate::der::encode_time(1_600_000_000).expect("time");
        children.extend_from_slice(&crate::der::encode_time(1_700_000_000).expect("time"));
        encode_sequence(&children)
    }

    fn test_cert(cn: &str, serial: u64, seed: u8) -> Certificate {
        let subject = name(cn);
        let mut tbs = Vec::new();
        tbs.extend_from_slice(&encode_integer(serial));
        tbs.extend_from_slice(&alg_id());
        tbs.extend_from_slice(&subject);
        tbs.extend_from_slice(&validity());
        tbs.extend_from_slice(&subject);
        let mut spki = alg_id();
        spki.extend_from_slice(&bit_string(&[seed; 32]));
        tbs.extend_from_slice(&encode_sequence(&spki));
        let tbs = encode_sequence(&tbs);
        let mut cert = tbs.clone();
        cert.extend_from_slice(&alg_id());
        cert.extend_from_slice(&bit_string(&crate::crypto::sha::sha256(&tbs)[..16]));
        Certificate::parse(&encode_sequence(&cert)).expect("valid test certificate")
    }

    fn recipient(label: &str, seed: u8) -> Recipient {
        // The certificate identity is derived from the seed, not the label, so
        // two labels with the same seed produce the same certificate.
        Recipient::new(
            label,
            test_cert(&format!("Recipient-{seed}"), seed as u64, seed),
        )
        .expect("recipient")
    }

    fn list(labels: &[(&str, u8)]) -> RecipientList {
        let mut list = RecipientList::new();
        for (label, seed) in labels {
            list.add(recipient(label, *seed)).expect("add");
        }
        list
    }

    fn envelope(recipients: &RecipientList) -> EnvelopedData {
        let cek = [7u8; 16];
        build_envelope(
            b"confidential pdf bytes",
            recipients,
            Permissions::new(Permission::PRINT, PasswordRole::User),
            ContentCipher::Aes128Cbc,
            &cek,
            [3u8; 16],
            &InMemoryKeyWrapProvider,
        )
        .expect("envelope")
    }

    #[test]
    fn recipient_list_round_trips_by_label_and_index() {
        let list = list(&[("alice", 1), ("bob", 2)]);
        assert_eq!(list.len(), 2);
        assert!(!list.is_empty());
        assert_eq!(list.get(0).map(Recipient::label), Some("alice"));
        assert_eq!(list.find("bob").map(Recipient::label), Some("bob"));
        assert!(list.find("carol").is_none());
        assert_eq!(list.iter().count(), 2);
        assert_eq!(
            list.get(0).map(Recipient::identifier).map(|s| s.len()),
            Some(64)
        );
    }

    #[test]
    fn duplicate_recipient_is_a_typed_error() {
        assert_eq!(
            list(&[("alice", 1)]).add(recipient("alice", 2)),
            Err(SecurityError::CertificateEncryptionDuplicateRecipient)
        );
        // Same certificate, different label also rejected by fingerprint.
        let mut list = list(&[("alice", 1)]);
        assert_eq!(
            list.add(recipient("alias", 1)),
            Err(SecurityError::CertificateEncryptionDuplicateRecipient)
        );
        assert_eq!(
            Recipient::new("", test_cert("x", 1, 1)),
            Err(SecurityError::CertificateEncryptionInvalidCertificate)
        );
    }

    #[test]
    fn envelope_round_trip_for_intended_recipient() {
        let recipients = list(&[("alice", 1), ("bob", 2)]);
        let env = envelope(&recipients);
        let opened = open_envelope(
            &env,
            RecipientSelector::Label("bob"),
            &recipients,
            &InMemoryKeyWrapProvider,
        )
        .expect("open as bob");
        assert_eq!(opened.plaintext, b"confidential pdf bytes");
        assert_eq!(opened.recipient_label, "bob");
        let by_index = open_envelope(
            &env,
            RecipientSelector::Index(0),
            &recipients,
            &InMemoryKeyWrapProvider,
        )
        .expect("open as alice");
        assert_eq!(by_index.plaintext, b"confidential pdf bytes");
    }

    #[test]
    fn wrong_recipient_is_rejected() {
        let only_alice = list(&[("alice", 1)]);
        let env_alice = envelope(&only_alice);

        // A recipient label absent from the supplied set is a typed error.
        let bob_only = list(&[("bob", 2)]);
        assert_eq!(
            open_envelope(
                &env_alice,
                RecipientSelector::Label("alice"),
                &bob_only,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionRecipientNotFound)
        );

        // Genuine wrong-recipient: keep the label but present a different
        // certificate, so unwrapping with the wrong public key yields a CEK that
        // fails the envelope's key commitment.
        let impostor = recipient("alice", 9);
        let mut impostor_list = RecipientList::new();
        impostor_list.add(impostor).expect("add impostor");
        assert_eq!(
            open_envelope(
                &env_alice,
                RecipientSelector::Label("alice"),
                &impostor_list,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionWrongRecipient)
        );
    }

    #[test]
    fn permissions_are_enforced_and_typed() {
        let recipients = list(&[("alice", 1)]);
        let env = build_envelope(
            b"payload",
            &recipients,
            Permissions::new(Permission::PRINT, PasswordRole::User),
            ContentCipher::Aes128Cbc,
            &[1u8; 16],
            [0u8; 16],
            &InMemoryKeyWrapProvider,
        )
        .expect("envelope");
        assert!(env.allows(Permission::PRINT));
        assert!(!env.allows(Permission::MODIFY));
        let opened = open_envelope(
            &env,
            RecipientSelector::Index(0),
            &recipients,
            &InMemoryKeyWrapProvider,
        )
        .expect("open");
        assert!(opened.require_permission(Permission::PRINT).is_ok());
        assert_eq!(
            opened.require_permission(Permission::MODIFY),
            Err(SecurityError::CertificateEncryptionPermissionDenied(
                "modify"
            ))
        );
    }

    #[test]
    fn malformed_certificate_is_rejected() {
        assert_eq!(
            Certificate::parse(&[0x30, 0x05, 0x02, 0x01]).map(|_| ()),
            Err(SecurityError::DerTruncated)
        );
    }

    #[test]
    fn no_recipients_and_bad_inputs_are_typed_errors() {
        let empty = RecipientList::new();
        let cek = [7u8; 16];
        assert_eq!(
            build_envelope(
                b"x",
                &empty,
                Permissions::new(Permission::NONE, PasswordRole::User),
                ContentCipher::Aes128Cbc,
                &cek,
                [0u8; 16],
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionNoRecipients)
        );
        let recipients = list(&[("alice", 1)]);
        assert_eq!(
            build_envelope(
                b"",
                &recipients,
                Permissions::new(Permission::NONE, PasswordRole::User),
                ContentCipher::Aes128Cbc,
                &cek,
                [0u8; 16],
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionEmptyPlaintext)
        );
        assert_eq!(
            build_envelope(
                b"x",
                &recipients,
                Permissions::new(Permission::NONE, PasswordRole::User),
                ContentCipher::Aes256Cbc,
                &cek,
                [0u8; 16],
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionInvalidKeyLength)
        );
    }

    #[test]
    fn bad_recipient_selectors_are_typed_errors() {
        let recipients = list(&[("alice", 1)]);
        let env = envelope(&recipients);
        assert_eq!(
            open_envelope(
                &env,
                RecipientSelector::Index(9),
                &recipients,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionRecipientIndexOutOfRange { index: 9 })
        );
        assert_eq!(
            open_envelope(
                &env,
                RecipientSelector::Label("nobody"),
                &recipients,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionRecipientNotFound)
        );
    }

    #[test]
    fn tampered_ciphertext_is_rejected() {
        let recipients = list(&[("alice", 1)]);
        let mut env = envelope(&recipients);
        let last = env.ciphertext.len() - 1;
        env.ciphertext[last] ^= 0xff;
        assert_eq!(
            open_envelope(
                &env,
                RecipientSelector::Index(0),
                &recipients,
                &InMemoryKeyWrapProvider,
            ),
            Err(SecurityError::CertificateEncryptionTamperedContent)
        );
    }

    #[test]
    fn envelope_is_deterministic_and_debug_redacts_secrets() {
        let recipients = list(&[("alice", 1), ("bob", 2)]);
        let a = envelope(&recipients).to_der();
        let b = envelope(&recipients).to_der();
        assert_eq!(a, b);
        let mut reader = DerReader::new(&a);
        let outer = reader.read_tlv().expect("content info");
        reader.expect_end().expect("end");
        assert_eq!(outer.tag(), Tag::SEQUENCE);
        let env = envelope(&recipients);
        let rendered = format!("{env:?}");
        assert!(!rendered.contains("confidential"));
        assert!(rendered.contains("<redacted>"));
        let opened = open_envelope(
            &env,
            RecipientSelector::Index(0),
            &recipients,
            &InMemoryKeyWrapProvider,
        )
        .expect("open");
        let opened_rendered = format!("{opened:?}");
        assert!(!opened_rendered.contains("confidential"));
        assert!(opened_rendered.contains("<22 bytes>"));
    }

    #[test]
    fn envelope_der_carries_cms_enveloped_data_oid() {
        let env = envelope(&list(&[("alice", 1)]));
        let der = env.to_der();
        let oid_der = encode_oid(&OID_ENVELOPED_DATA).expect("oid");
        assert!(
            der.windows(oid_der.len())
                .any(|window| window == oid_der.as_slice()),
            "envelopedData OID must appear in DER"
        );
        let data_oid = encode_oid(&OID_DATA).expect("oid");
        assert!(
            der.windows(data_oid.len())
                .any(|window| window == data_oid.as_slice()),
            "data OID must appear in DER"
        );
    }

    #[test]
    fn aes_kw_provider_wraps_cek_with_authenticated_length() {
        let recipients = list(&[("alice", 1)]);
        let recipient = recipients.get(0).expect("recipient");
        let provider = AesKwKeyWrapProvider::new(&[0x2bu8; 32]).expect("provider");
        let cek = [0x07u8; 16];
        let wrapped = provider.wrap_key(&cek, recipient).expect("wrap");
        // RFC 3394 output is always one 8-byte semiblock longer than the CEK.
        assert_eq!(wrapped.len(), cek.len() + 8);
        assert_eq!(
            provider.unwrap_key(&wrapped, recipient).expect("unwrap"),
            cek
        );
        assert!(!format!("{provider:?}").contains("2b2b"));

        let env = build_envelope(
            b"aes-kw content",
            &recipients,
            Permissions::new(Permission::PRINT, PasswordRole::User),
            ContentCipher::Aes128Cbc,
            &cek,
            [3u8; 16],
            &provider,
        )
        .expect("envelope");
        assert_eq!(env.recipient_keys()[0].wrapped_len(), cek.len() + 8);
        let opened =
            open_envelope(&env, RecipientSelector::Index(0), &recipients, &provider).expect("open");
        assert_eq!(opened.plaintext, b"aes-kw content");
    }

    #[test]
    fn aes_kw_provider_rejects_wrong_kek() {
        let recipients = list(&[("alice", 1)]);
        let recipient = recipients.get(0).expect("recipient");
        let wrap_provider = AesKwKeyWrapProvider::new(&[0x11u8; 16]).expect("provider");
        let wrong_provider = AesKwKeyWrapProvider::new(&[0x22u8; 16]).expect("provider");
        let wrapped = wrap_provider
            .wrap_key(&[0x07u8; 16], recipient)
            .expect("wrap");
        assert_eq!(
            wrong_provider.unwrap_key(&wrapped, recipient),
            Err(SecurityError::CertificateEncryptionKeyUnwrapFailed)
        );
    }

    #[test]
    fn aes_kw_wrapped_key_tamper_is_rejected() {
        let recipients = list(&[("alice", 1)]);
        let provider = AesKwKeyWrapProvider::new(&[0x31u8; 24]).expect("provider");
        let cek = [0x09u8; 32];
        let mut env = build_envelope(
            b"tamper the wrapped key",
            &recipients,
            Permissions::new(Permission::NONE, PasswordRole::User),
            ContentCipher::Aes256Cbc,
            &cek,
            [4u8; 16],
            &provider,
        )
        .expect("envelope");
        assert_eq!(env.recipient_keys()[0].wrapped_len(), 40);
        // Untouched envelope opens with the correct KEK.
        assert_eq!(
            open_envelope(&env, RecipientSelector::Index(0), &recipients, &provider)
                .expect("open")
                .plaintext,
            b"tamper the wrapped key"
        );
        // Flipping a bit in the wrapped key breaks the RFC 3394 integrity check.
        let last = env.recipients[0].wrapped_key.len() - 1;
        env.recipients[0].wrapped_key[last] ^= 0x80;
        assert_eq!(
            open_envelope(&env, RecipientSelector::Index(0), &recipients, &provider),
            Err(SecurityError::CertificateEncryptionKeyUnwrapFailed)
        );
    }

    #[test]
    fn in_memory_provider_uses_aes_kw_not_xor() {
        let recipients = list(&[("alice", 1)]);
        let recipient = recipients.get(0).expect("recipient");
        let cek = [0x07u8; 16];
        let wrapped = InMemoryKeyWrapProvider
            .wrap_key(&cek, recipient)
            .expect("wrap");
        // XOR would return 16 bytes; AES-KW returns 24.
        assert_eq!(wrapped.len(), 24);
        assert_eq!(
            InMemoryKeyWrapProvider
                .unwrap_key(&wrapped, recipient)
                .expect("unwrap"),
            cek
        );
    }

    #[test]
    fn aes_kw_provider_rejects_invalid_kek_length() {
        assert_eq!(
            AesKwKeyWrapProvider::new(&[0u8; 12]).err(),
            Some(SecurityError::AesKwInvalidKekLength(12))
        );
        assert_eq!(
            AesKwKeyWrapProvider::new(&[0u8; 20]).err(),
            Some(SecurityError::AesKwInvalidKekLength(20))
        );
        assert!(AesKwKeyWrapProvider::new(&[0u8; 16]).is_ok());
        assert!(AesKwKeyWrapProvider::new(&[0u8; 24]).is_ok());
        assert!(AesKwKeyWrapProvider::new(&[0u8; 32]).is_ok());
    }
}
