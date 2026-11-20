//! Local digital-ID lifecycle: validated key/certificate pairs, a sorted store,
//! and a password-protected store envelope (SEC-004).
//!
//! This module completes the local side of SEC-004 on top of the existing
//! [`crate::x509`] types. It adds four things the certificate model alone does
//! not provide:
//!
//! - [`PrivateKeyPkcs8`] — a caller-supplied, "PKCS#8-ish" private key whose
//!   DER structure is validated on import. The crate never generates a key and
//!   never prints its bytes; the `Debug` implementation is deliberately redacted.
//! - [`KeyCertificatePair`] — a certificate plus such a key, checked for
//!   agreement between the key algorithm and the certificate's public-key OID.
//! - [`DigitalIdLifecycleStore`] — an in-memory, label-keyed store whose listing
//!   is sorted and deterministic, with expiry/not-yet-valid classification
//!   against a *caller-supplied* time, and removal/rotation operations.
//! - [`seal_store`] / [`open_store`] — a password-protected envelope around the
//!   whole store, built only from the existing AES-CBC and SHA-256 primitives.
//!
//! # Trust and privacy boundary
//!
//! There is no filesystem access, no network access, and no wall-clock read.
//! Every timestamp and password is supplied by the caller. Private-key bytes
//! are held so they can be exported back to the caller, but no `Debug`,
//! `Display`, or error value ever contains them. A wrong envelope password is
//! reported with the same typed error as tampered bytes so it cannot be used as
//! an oracle.

use std::collections::BTreeMap;
use std::fmt;

use crate::crypto::aes::{decrypt_cbc, encrypt_cbc};
use crate::crypto::constant_time_eq;
use crate::crypto::sha::{Sha256, hmac_sha256, sha256};
use crate::der::DerReader;
use crate::error::{Result, SecurityError};
use crate::x509::certificate::{Certificate, ValidityStatus};
use crate::x509::parse_algorithm_identifier;

const ENVELOPE_MAGIC: &[u8; 8] = b"PDFXIDS1";
const ENVELOPE_VERSION: u8 = 1;
const TAG_LEN: usize = 32;
const IV_LEN: usize = 16;
const DERIVE_ROUNDS: u32 = 1024;

/// Iterated, domain-separated key derivation producing an AES key and a MAC key.
fn derive_keys(password: &[u8], salt: &[u8]) -> ([u8; 32], [u8; 32]) {
    let mut hasher = Sha256::new();
    hasher.update(b"pdfxi-store-aes-v1\x00");
    hasher.update(&(salt.len() as u64).to_be_bytes());
    hasher.update(salt);
    hasher.update(&(password.len() as u64).to_be_bytes());
    hasher.update(password);
    let mut aes_key = hasher.finalize();

    let mut round = 1u32;
    while round < DERIVE_ROUNDS {
        let mut next = Sha256::new();
        next.update(&round.to_be_bytes());
        next.update(&aes_key);
        aes_key = next.finalize();
        round = round.saturating_add(1);
    }

    let mut mac_hasher = Sha256::new();
    mac_hasher.update(b"pdfxi-store-mac-v1\x00");
    mac_hasher.update(&aes_key);
    let mac_key = mac_hasher.finalize();
    (aes_key, mac_key)
}

/// Derive a deterministic IV from the AES key and the caller's salt.
fn derive_iv(aes_key: &[u8; 32], salt: &[u8]) -> [u8; IV_LEN] {
    let mut hasher = Sha256::new();
    hasher.update(b"pdfxi-store-iv-v1\x00");
    hasher.update(aes_key);
    hasher.update(&(salt.len() as u64).to_be_bytes());
    hasher.update(salt);
    let digest = hasher.finalize();
    let mut iv = [0u8; IV_LEN];
    iv.copy_from_slice(&digest[..IV_LEN]);
    iv
}

/// A caller-supplied private key in the minimal PKCS#8 v1 shape this subset
/// understands:
///
/// ```text
/// PrivateKeyInfo ::= SEQUENCE {
///     version              INTEGER (0),
///     privateKeyAlgorithm  AlgorithmIdentifier,
///     privateKey           OCTET STRING (SIZE (1..MAX))
/// }
/// ```
///
/// The bytes are retained verbatim so they can be exported back to the caller,
/// but every `Debug` rendering shows only non-secret metadata.
#[derive(Clone, PartialEq, Eq)]
pub struct PrivateKeyPkcs8 {
    der: Vec<u8>,
    algorithm_oid: Vec<u32>,
}

impl PrivateKeyPkcs8 {
    /// Validate and wrap a caller-supplied PKCS#8-style private key.
    ///
    /// Rejects a non-v1 version, a missing/malformed algorithm identifier, an
    /// empty key octet string, trailing bytes, and any other DER violation with
    /// a typed error. No key material is logged.
    pub fn parse(der: &[u8]) -> Result<Self> {
        let mut top = DerReader::new(der);
        let tlv = top.read_tlv()?;
        top.expect_end()?;
        let mut sequence = tlv.as_sequence()?;

        let version = sequence.read_tlv()?.as_integer_u64()?;
        if version != 0 {
            return Err(SecurityError::DigitalIdKeyMalformed("version"));
        }
        let algorithm_tlv = sequence.read_tlv()?;
        let (algorithm_oid, _parameters) = parse_algorithm_identifier(&algorithm_tlv)?;
        if algorithm_oid.is_empty() {
            return Err(SecurityError::DigitalIdKeyMalformed("algorithm"));
        }
        let private_key = sequence.read_tlv()?.as_octets()?;
        if private_key.is_empty() {
            return Err(SecurityError::DigitalIdKeyMalformed("privateKey"));
        }
        sequence.expect_end()?;

        Ok(Self {
            der: der.to_vec(),
            algorithm_oid,
        })
    }

    /// Return the validated DER bytes.
    ///
    /// These are the caller's own bytes, returned only through this explicit
    /// accessor; they are never placed in diagnostics.
    pub fn as_bytes(&self) -> &[u8] {
        &self.der
    }

    /// Return the algorithm OID declared by the key's `AlgorithmIdentifier`.
    pub fn algorithm_oid(&self) -> &[u32] {
        &self.algorithm_oid
    }

    /// Return the key length in bytes (metadata only).
    pub const fn len(&self) -> usize {
        self.der.len()
    }

    /// Always `false`: a validated key is never empty.
    pub const fn is_empty(&self) -> bool {
        false
    }
}

impl fmt::Debug for PrivateKeyPkcs8 {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("PrivateKeyPkcs8")
            .field("algorithm_oid", &self.algorithm_oid)
            .field("len", &self.der.len())
            .field("private_key", &"<redacted>")
            .finish()
    }
}

/// A validated certificate paired with its caller-supplied private key.
#[derive(Clone, PartialEq, Eq)]
pub struct KeyCertificatePair {
    certificate: Certificate,
    private_key: PrivateKeyPkcs8,
}

impl KeyCertificatePair {
    /// Validate a certificate/key pair.
    ///
    /// Both inputs are parsed strictly. The key's algorithm OID must equal the
    /// certificate's `SubjectPublicKeyInfo` algorithm OID; a mismatch is a
    /// typed [`SecurityError::DigitalIdKeyAlgorithmMismatch`].
    pub fn parse(certificate_der: &[u8], private_key_der: &[u8]) -> Result<Self> {
        let certificate = Certificate::parse(certificate_der)?;
        let private_key = PrivateKeyPkcs8::parse(private_key_der)?;
        if certificate.subject_public_key_info().algorithm_oid() != private_key.algorithm_oid() {
            return Err(SecurityError::DigitalIdKeyAlgorithmMismatch);
        }
        Ok(Self {
            certificate,
            private_key,
        })
    }

    /// Classify this pair's certificate at `now` Unix seconds.
    pub fn validity_at(&self, now: u64) -> ValidityStatus {
        self.certificate.validity_status_at(now)
    }

    /// Return the parsed certificate.
    pub fn certificate(&self) -> &Certificate {
        &self.certificate
    }

    /// Return the certificate's DER encoding.
    pub fn certificate_der(&self) -> &[u8] {
        self.certificate.der()
    }

    /// Return the paired private key (whose bytes are redacted in `Debug`).
    pub fn private_key(&self) -> &PrivateKeyPkcs8 {
        &self.private_key
    }
}

impl fmt::Debug for KeyCertificatePair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("KeyCertificatePair")
            .field("certificate", &self.certificate)
            .field("private_key", &self.private_key)
            .finish()
    }
}

/// The material returned by [`DigitalIdLifecycleStore::export`].
///
/// It holds the caller's certificate and key bytes so they can be re-imported;
/// its `Debug` rendering redacts the private key.
#[derive(Clone, PartialEq, Eq)]
pub struct ExportedKeyPair {
    /// Caller-assigned identity label.
    pub label: String,
    /// Certificate DER bytes.
    pub certificate_der: Vec<u8>,
    /// Validated PKCS#8-style private key bytes.
    pub private_key_der: Vec<u8>,
    /// Caller-supplied import timestamp (Unix seconds).
    pub imported_at: u64,
}

impl fmt::Debug for ExportedKeyPair {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ExportedKeyPair")
            .field("label", &self.label)
            .field("certificate_len", &self.certificate_der.len())
            .field("private_key", &"<redacted>")
            .field("imported_at", &self.imported_at)
            .finish()
    }
}

/// A non-secret, deterministic listing entry for one stored digital ID.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdListing {
    /// Caller-assigned identity label (the sort key).
    pub label: String,
    /// Deterministic subject display string.
    pub subject_display: String,
    /// Deterministic issuer display string.
    pub issuer_display: String,
    /// Certificate serial number.
    pub serial_number: u64,
    /// Lowercase hex SHA-256 fingerprint of the certificate DER.
    pub certificate_fingerprint_sha256: String,
    /// Dotted OID of the paired key's algorithm.
    pub key_algorithm_oid: String,
    /// Certificate validity at the caller-supplied listing time.
    pub validity: ValidityStatus,
    /// Caller-supplied import timestamp.
    pub imported_at: u64,
}

#[derive(Clone, PartialEq, Eq)]
struct StoredId {
    pair: KeyCertificatePair,
    imported_at: u64,
}

/// An in-memory, label-keyed collection of validated certificate/key pairs.
///
/// Lookups and listings are deterministic because entries are kept in a
/// [`BTreeMap`] ordered by label. The store never reads the clock or the
/// filesystem, and its `Debug` output contains no private-key bytes.
#[derive(Clone, Default, PartialEq, Eq)]
pub struct DigitalIdLifecycleStore {
    ids: BTreeMap<String, StoredId>,
}

impl fmt::Debug for DigitalIdLifecycleStore {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let labels: Vec<&str> = self.ids.keys().map(String::as_str).collect();
        f.debug_struct("DigitalIdLifecycleStore")
            .field("labels", &labels)
            .field("len", &self.ids.len())
            .finish()
    }
}

impl DigitalIdLifecycleStore {
    /// Create an empty store.
    pub fn new() -> Self {
        Self::default()
    }

    /// Return the number of stored identities.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Return `true` when the store is empty.
    pub fn is_empty(&self) -> bool {
        self.ids.is_empty()
    }

    /// Validate and import a certificate/key pair under `label`.
    ///
    /// Rejects an empty or control-character label, a duplicate label, and a
    /// duplicate certificate fingerprint (the same identity under two labels).
    pub fn import(
        &mut self,
        label: impl Into<String>,
        certificate_der: &[u8],
        private_key_der: &[u8],
        imported_at: u64,
    ) -> Result<()> {
        let pair = KeyCertificatePair::parse(certificate_der, private_key_der)?;
        let label = validate_label(label)?;
        self.check_no_duplicate(&label, &pair)?;
        self.ids.insert(label, StoredId { pair, imported_at });
        Ok(())
    }

    /// Re-import a previously exported pair (a full export/import round trip).
    pub fn import_exported(&mut self, exported: &ExportedKeyPair) -> Result<()> {
        self.import(
            exported.label.clone(),
            &exported.certificate_der,
            &exported.private_key_der,
            exported.imported_at,
        )
    }

    /// Return the validated pair for `label`, if present.
    pub fn get(&self, label: &str) -> Option<&KeyCertificatePair> {
        self.ids.get(label).map(|stored| &stored.pair)
    }

    /// Return `true` when `label` is present.
    pub fn contains(&self, label: &str) -> bool {
        self.ids.contains_key(label)
    }

    /// Remove and return the exported material for `label`.
    pub fn remove(&mut self, label: &str) -> Result<ExportedKeyPair> {
        let stored = self
            .ids
            .remove(label)
            .ok_or(SecurityError::DigitalIdNotFound)?;
        Ok(export_stored(label, &stored))
    }

    /// Replace the pair stored under `label` (key/certificate rotation).
    ///
    /// Returns the previous material so the caller can archive it. A missing
    /// label or a new certificate already used by a *different* label is a
    /// typed error, and the store is left unchanged on failure.
    pub fn rotate(
        &mut self,
        label: &str,
        certificate_der: &[u8],
        private_key_der: &[u8],
        imported_at: u64,
    ) -> Result<ExportedKeyPair> {
        let pair = KeyCertificatePair::parse(certificate_der, private_key_der)?;
        if !self.ids.contains_key(label) {
            return Err(SecurityError::DigitalIdNotFound);
        }
        let fingerprint = sha256(pair.certificate_der());
        let duplicate = self.ids.iter().any(|(other_label, stored)| {
            *other_label != label && sha256(stored.pair.certificate_der()) == fingerprint
        });
        if duplicate {
            return Err(SecurityError::DigitalIdDuplicate);
        }
        let previous = self
            .ids
            .get_mut(label)
            .ok_or(SecurityError::DigitalIdNotFound)?;
        let old = export_stored(label, previous);
        *previous = StoredId { pair, imported_at };
        Ok(old)
    }

    /// Return a sorted, deterministic listing with validity classified at `now`.
    pub fn list(&self, now: u64) -> Vec<IdListing> {
        self.ids
            .iter()
            .map(|(label, stored)| {
                let fingerprint = stored.pair.certificate().fingerprint_sha256();
                let key_oid = stored.pair.private_key().algorithm_oid();
                IdListing {
                    label: label.clone(),
                    subject_display: stored.pair.certificate().subject().display_rfc4514(),
                    issuer_display: stored.pair.certificate().issuer().display_rfc4514(),
                    serial_number: stored.pair.certificate().serial_number(),
                    certificate_fingerprint_sha256: hex(&fingerprint),
                    key_algorithm_oid: key_oid
                        .iter()
                        .map(u32::to_string)
                        .collect::<Vec<_>>()
                        .join("."),
                    validity: stored.pair.validity_at(now),
                    imported_at: stored.imported_at,
                }
            })
            .collect()
    }

    /// Export the material stored under `label`.
    pub fn export(&self, label: &str) -> Result<ExportedKeyPair> {
        let stored = self
            .ids
            .get(label)
            .ok_or(SecurityError::DigitalIdNotFound)?;
        Ok(export_stored(label, stored))
    }

    fn check_no_duplicate(&self, label: &str, pair: &KeyCertificatePair) -> Result<()> {
        if self.ids.contains_key(label) {
            return Err(SecurityError::DigitalIdDuplicate);
        }
        let fingerprint = sha256(pair.certificate_der());
        if self
            .ids
            .values()
            .any(|stored| sha256(stored.pair.certificate_der()) == fingerprint)
        {
            return Err(SecurityError::DigitalIdDuplicate);
        }
        Ok(())
    }
}

fn validate_label(label: impl Into<String>) -> Result<String> {
    let label = label.into();
    if label.is_empty() || label.chars().any(char::is_control) {
        return Err(SecurityError::Configuration(
            "digital-ID label must be non-empty and control-character free".to_owned(),
        ));
    }
    Ok(label)
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write;
    bytes
        .iter()
        .fold(String::with_capacity(bytes.len() * 2), |mut out, byte| {
            let _ = write!(out, "{byte:02x}");
            out
        })
}

fn export_stored(label: &str, stored: &StoredId) -> ExportedKeyPair {
    ExportedKeyPair {
        label: label.to_owned(),
        certificate_der: stored.pair.certificate_der().to_vec(),
        private_key_der: stored.pair.private_key().as_bytes().to_vec(),
        imported_at: stored.imported_at,
    }
}

// ---- Deterministic store serialization used by the password envelope ----

fn push_u32(out: &mut Vec<u8>, value: u32) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_u64(out: &mut Vec<u8>, value: u64) {
    out.extend_from_slice(&value.to_be_bytes());
}

fn push_bytes(out: &mut Vec<u8>, bytes: &[u8]) {
    push_u32(out, bytes.len() as u32);
    out.extend_from_slice(bytes);
}

fn encode_store(store: &DigitalIdLifecycleStore) -> Vec<u8> {
    let mut out = Vec::new();
    push_u32(&mut out, store.ids.len() as u32);
    for (label, stored) in &store.ids {
        push_bytes(&mut out, label.as_bytes());
        push_bytes(&mut out, stored.pair.certificate_der());
        push_bytes(&mut out, stored.pair.private_key().as_bytes());
        push_u64(&mut out, stored.imported_at);
    }
    out
}

/// A tiny bounds-checked big-endian cursor for envelope decoding.
struct Cursor<'a> {
    bytes: &'a [u8],
}

impl<'a> Cursor<'a> {
    fn new(bytes: &'a [u8]) -> Self {
        Self { bytes }
    }

    fn take(&mut self, length: usize) -> Result<&'a [u8]> {
        if length > self.bytes.len() {
            return Err(SecurityError::StoreEnvelopeMalformed("truncated field"));
        }
        let (head, tail) = self.bytes.split_at(length);
        self.bytes = tail;
        Ok(head)
    }

    fn take_u32(&mut self) -> Result<u32> {
        let bytes = self.take(4)?;
        let mut array = [0u8; 4];
        array.copy_from_slice(bytes);
        Ok(u32::from_be_bytes(array))
    }

    fn take_u64(&mut self) -> Result<u64> {
        let bytes = self.take(8)?;
        let mut array = [0u8; 8];
        array.copy_from_slice(bytes);
        Ok(u64::from_be_bytes(array))
    }

    fn take_sized(&mut self) -> Result<&'a [u8]> {
        let length = usize::try_from(self.take_u32()?)
            .map_err(|_| SecurityError::StoreEnvelopeMalformed("field length"))?;
        self.take(length)
    }

    fn finish(self) -> Result<()> {
        if self.bytes.is_empty() {
            Ok(())
        } else {
            Err(SecurityError::StoreEnvelopeMalformed("trailing bytes"))
        }
    }
}

fn decode_store(plain: &[u8]) -> Result<DigitalIdLifecycleStore> {
    let mut cursor = Cursor::new(plain);
    let count = cursor.take_u32()?;
    let mut store = DigitalIdLifecycleStore::new();
    for _ in 0..count {
        let label_bytes = cursor.take_sized()?;
        let label = std::str::from_utf8(label_bytes)
            .map_err(|_| SecurityError::StoreEnvelopeMalformed("label encoding"))?;
        let certificate_der = cursor.take_sized()?;
        let private_key_der = cursor.take_sized()?;
        let imported_at = cursor.take_u64()?;
        store.import(label, certificate_der, private_key_der, imported_at)?;
    }
    cursor.finish()?;
    Ok(store)
}

/// Seal a whole store into a password-protected, tamper-evident byte envelope.
///
/// The key schedule uses iterated SHA-256 over `password || salt`, the payload
/// is encrypted with AES-256-CBC, and an HMAC-SHA256 tag covers the header and
/// ciphertext. `salt` is supplied by the caller and stored (non-secret) in the
/// envelope header, so results are deterministic and the crate never needs a
/// randomness or clock source.
pub fn seal_store(
    store: &DigitalIdLifecycleStore,
    password: &[u8],
    salt: &[u8],
) -> Result<Vec<u8>> {
    if password.is_empty() {
        return Err(SecurityError::MissingPassword);
    }
    if salt.is_empty() {
        return Err(SecurityError::Configuration(
            "store salt must not be empty".to_owned(),
        ));
    }

    let plaintext = encode_store(store);
    let (aes_key, mac_key) = derive_keys(password, salt);
    let iv = derive_iv(&aes_key, salt);
    let ciphertext = encrypt_cbc(&aes_key, &iv, &plaintext)?;

    let mut out = Vec::with_capacity(
        ENVELOPE_MAGIC.len() + 1 + 4 + salt.len() + IV_LEN + 4 + ciphertext.len() + TAG_LEN,
    );
    out.extend_from_slice(ENVELOPE_MAGIC);
    out.push(ENVELOPE_VERSION);
    push_bytes(&mut out, salt);
    out.extend_from_slice(&iv);
    push_u32(&mut out, ciphertext.len() as u32);
    out.extend_from_slice(&ciphertext);
    let tag = hmac_sha256(&mac_key, &out);
    out.extend_from_slice(&tag);
    Ok(out)
}

/// Open a store envelope with a caller-supplied password.
///
/// The salt is read from the envelope header and the derived IV is checked
/// before the HMAC is verified in constant time; then the payload is decrypted
/// and the store is re-validated. A wrong password or any tampering yields the
/// same typed [`SecurityError::StoreEnvelopeIntegrityFailed`], so a guess cannot
/// be used as an oracle. Structural damage yields
/// [`SecurityError::StoreEnvelopeMalformed`]. No key or payload bytes are logged,
/// and the crate never reads a file or a clock.
pub fn open_store(envelope: &[u8], password: &[u8]) -> Result<DigitalIdLifecycleStore> {
    let minimum = ENVELOPE_MAGIC.len() + 1 + 4 + IV_LEN + 4 + TAG_LEN;
    if envelope.len() < minimum {
        return Err(SecurityError::StoreEnvelopeMalformed("envelope too short"));
    }
    let mut cursor = Cursor::new(envelope);
    if cursor.take(ENVELOPE_MAGIC.len())? != ENVELOPE_MAGIC {
        return Err(SecurityError::StoreEnvelopeMalformed("magic"));
    }
    let version = cursor
        .take(1)?
        .first()
        .copied()
        .ok_or(SecurityError::StoreEnvelopeMalformed("version"))?;
    if version != ENVELOPE_VERSION {
        return Err(SecurityError::StoreEnvelopeVersionUnsupported(version));
    }
    let salt = cursor.take_sized()?.to_vec();
    let iv_bytes = cursor.take(IV_LEN)?;
    let mut iv = [0u8; IV_LEN];
    iv.copy_from_slice(iv_bytes);
    let ciphertext_len = usize::try_from(cursor.take_u32()?)
        .map_err(|_| SecurityError::StoreEnvelopeMalformed("ciphertext length"))?;
    let ciphertext = cursor.take(ciphertext_len)?.to_vec();
    let tag = cursor.take(TAG_LEN)?;
    cursor.finish()?;

    let authenticated_len = envelope.len() - TAG_LEN;
    let (aes_key, mac_key) = derive_keys(password, &salt);
    let expected_iv = derive_iv(&aes_key, &salt);
    if !constant_time_eq(&expected_iv, &iv) {
        return Err(SecurityError::StoreEnvelopeIntegrityFailed);
    }
    let expected_tag = hmac_sha256(&mac_key, &envelope[..authenticated_len]);
    if !constant_time_eq(&expected_tag, tag) {
        return Err(SecurityError::StoreEnvelopeIntegrityFailed);
    }
    let plaintext = decrypt_cbc(&aes_key, &iv, &ciphertext)?;
    decode_store(&plaintext)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::der::{
        Tag, encode_integer, encode_null, encode_octet_string, encode_oid, encode_sequence,
    };
    use crate::x509::OID_SHA256_WITH_RSA;

    const OID_CN: [u32; 4] = [2, 5, 4, 3];
    const OID_RSA_ENCRYPTION: [u32; 7] = [1, 2, 840, 113_549, 1, 1, 1];
    const NOW: u64 = 1_650_000_000;

    fn pkcs8(seed: u8, oid: &[u32]) -> Vec<u8> {
        let mut alg = encode_oid(oid).expect("oid");
        alg.extend_from_slice(&encode_null());
        let mut children = encode_integer(0);
        children.extend_from_slice(&encode_sequence(&alg));
        children.extend_from_slice(&encode_octet_string(&[seed; 32]));
        encode_sequence(&children)
    }

    fn cert(cn: &str, serial: u64, not_before: u64, not_after: u64) -> Vec<u8> {
        use crate::der::{encode_set, encode_time, write_tlv};
        let mut ava = encode_oid(&OID_CN).expect("oid");
        write_tlv(&mut ava, Tag::UTF8_STRING, cn.as_bytes());
        let name = encode_sequence(&encode_set(&encode_sequence(&ava)));
        let mut validity = encode_time(not_before).expect("notBefore");
        validity.extend_from_slice(&encode_time(not_after).expect("notAfter"));
        let mut spki_children = encode_sequence(&{
            let mut alg = encode_oid(&OID_RSA_ENCRYPTION).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        });
        let mut bit = vec![0u8];
        bit.extend_from_slice(&[7u8; 32]);
        spki_children.extend_from_slice(&crate::der::encode_tlv(Tag::BIT_STRING, &bit));
        let spki = encode_sequence(&spki_children);
        let mut tbs = Vec::new();
        tbs.extend_from_slice(&encode_integer(serial));
        tbs.extend_from_slice(&encode_sequence(&{
            let mut alg = encode_oid(&OID_SHA256_WITH_RSA).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        }));
        tbs.extend_from_slice(&name);
        tbs.extend_from_slice(&encode_sequence(&validity));
        tbs.extend_from_slice(&name);
        tbs.extend_from_slice(&spki);
        let tbs = encode_sequence(&tbs);
        let signature = sha256(&tbs);
        let mut outer = tbs;
        outer.extend_from_slice(&encode_sequence(&{
            let mut alg = encode_oid(&OID_SHA256_WITH_RSA).expect("oid");
            alg.extend_from_slice(&encode_null());
            alg
        }));
        outer.extend_from_slice(&crate::der::encode_tlv(Tag::BIT_STRING, &{
            let mut bits = vec![0u8];
            bits.extend_from_slice(&signature[..16]);
            bits
        }));
        encode_sequence(&outer)
    }

    fn pair(cn: &str, serial: u64, seed: u8, window: (u64, u64)) -> (Vec<u8>, Vec<u8>) {
        (
            cert(cn, serial, window.0, window.1),
            pkcs8(seed, &OID_RSA_ENCRYPTION),
        )
    }

    fn store_with_three() -> DigitalIdLifecycleStore {
        let mut store = DigitalIdLifecycleStore::new();
        let valid = pair("Zoe", 3, 3, (NOW - 10, NOW + 10));
        let expired = pair("Alice", 1, 1, (NOW - 100, NOW - 1));
        let future = pair("Bob", 2, 2, (NOW + 1, NOW + 100));
        store
            .import("zoe", &valid.0, &valid.1, 100)
            .expect("import zoe");
        store
            .import("alice", &expired.0, &expired.1, 200)
            .expect("import alice");
        store
            .import("bob", &future.0, &future.1, 300)
            .expect("import bob");
        store
    }

    #[test]
    fn store_round_trip_lists_sorted_and_deterministic() {
        let store = store_with_three();
        let listing = store.list(NOW);
        let labels: Vec<&str> = listing.iter().map(|entry| entry.label.as_str()).collect();
        assert_eq!(labels, vec!["alice", "bob", "zoe"]);
        assert_eq!(listing, store.list(NOW));
        let exported = store.export("bob").expect("export");
        assert_eq!(
            exported.private_key_der,
            store.get("bob").expect("get").private_key().as_bytes()
        );
        let mut restored = DigitalIdLifecycleStore::new();
        restored.import_exported(&exported).expect("reimport");
        assert_eq!(restored.export("bob").expect("export"), exported);
    }

    #[test]
    fn wrong_password_is_typed() {
        let store = store_with_three();
        let envelope = seal_store(&store, b"correct", b"salt").expect("seal");
        assert_eq!(
            open_store(&envelope, b"wrong"),
            Err(SecurityError::StoreEnvelopeIntegrityFailed)
        );
        let opened = open_store(&envelope, b"correct").expect("open");
        assert_eq!(opened.list(NOW), store.list(NOW));
    }

    #[test]
    fn expired_and_not_yet_valid_ids_are_reported() {
        let store = store_with_three();
        let listing = store.list(NOW);
        assert_eq!(listing[0].validity, ValidityStatus::Expired);
        assert_eq!(listing[1].validity, ValidityStatus::NotYetValid);
        assert_eq!(listing[2].validity, ValidityStatus::Valid);
        assert_eq!(
            store.get("alice").expect("get").validity_at(NOW),
            ValidityStatus::Expired
        );
    }

    #[test]
    fn duplicate_identity_is_typed() {
        let mut store = DigitalIdLifecycleStore::new();
        let (cert, key) = pair("Alice", 1, 1, (NOW - 10, NOW + 10));
        store.import("one", &cert, &key, 1).expect("first");
        assert_eq!(
            store.import("two", &cert, &key, 2),
            Err(SecurityError::DigitalIdDuplicate)
        );
        assert_eq!(store.len(), 1);
    }

    #[test]
    fn rotation_replaces_pair_and_returns_old() {
        let mut store = DigitalIdLifecycleStore::new();
        let old = pair("Old", 9, 9, (NOW - 10, NOW + 10));
        store.import("id", &old.0, &old.1, 1).expect("import");
        let new = pair("New", 10, 10, (NOW - 10, NOW + 10));
        let previous = store.rotate("id", &new.0, &new.1, 2).expect("rotate");
        assert_eq!(previous.label, "id");
        assert_eq!(previous.certificate_der, old.0);
        assert_eq!(
            store.get("id").expect("get").certificate().serial_number(),
            10
        );
        assert_eq!(
            store.rotate("missing", &new.0, &new.1, 3),
            Err(SecurityError::DigitalIdNotFound)
        );
        assert_eq!(
            store.remove("missing"),
            Err(SecurityError::DigitalIdNotFound)
        );
    }

    #[test]
    fn malformed_pkcs8_is_typed() {
        let (cert, _) = pair("Alice", 1, 1, (NOW - 10, NOW + 10));
        assert!(matches!(
            KeyCertificatePair::parse(&cert, b"not der"),
            Err(SecurityError::DerTruncated | SecurityError::DerUnexpectedTag { .. })
        ));
        let bad_version = {
            let mut children = encode_integer(1);
            let mut alg = encode_oid(&OID_RSA_ENCRYPTION).expect("oid");
            alg.extend_from_slice(&encode_null());
            children.extend_from_slice(&encode_sequence(&alg));
            children.extend_from_slice(&encode_octet_string(&[7u8; 8]));
            encode_sequence(&children)
        };
        assert_eq!(
            PrivateKeyPkcs8::parse(&bad_version),
            Err(SecurityError::DigitalIdKeyMalformed("version"))
        );
    }

    #[test]
    fn key_algorithm_mismatch_is_typed() {
        let (cert, _) = pair("Alice", 1, 1, (NOW - 10, NOW + 10));
        let ecdsa_key = pkcs8(1, &crate::x509::OID_ECDSA_SHA256);
        assert_eq!(
            KeyCertificatePair::parse(&cert, &ecdsa_key),
            Err(SecurityError::DigitalIdKeyAlgorithmMismatch)
        );
    }

    #[test]
    fn secrets_are_redacted_in_debug() {
        let store = store_with_three();
        let rendered = format!("{store:?}");
        assert!(!rendered.contains("private_key"));
        let exported = store.export("bob").expect("export");
        let rendered_pair = format!("{exported:?}");
        assert!(rendered_pair.contains("<redacted>"));
        assert!(!rendered_pair.contains("private_key_der"));
    }

    #[test]
    fn envelope_tampering_is_typed() {
        let store = store_with_three();
        let mut envelope = seal_store(&store, b"pw", b"salt").expect("seal");
        let last = envelope.len() - 1;
        envelope[last] ^= 0xff;
        assert_eq!(
            open_store(&envelope, b"pw"),
            Err(SecurityError::StoreEnvelopeIntegrityFailed)
        );
        assert_eq!(
            open_store(b"too short", b"pw"),
            Err(SecurityError::StoreEnvelopeMalformed("envelope too short"))
        );
    }
}
