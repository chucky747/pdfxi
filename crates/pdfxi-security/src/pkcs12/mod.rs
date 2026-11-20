//! PKCS#12 (`.pfx` / `.p12`) identity import.
//!
//! [`import_pfx`](crate::pkcs12::import_pfx) parses an RFC 7292 archive,
//! verifies its MAC when one is present, decrypts the shrouded private key,
//! and returns the PKCS#8 key plus every certificate it carried.
//! [`crate::pdf_signature::SigningIdentity`] turns that into a usable signing
//! identity.
//!
//! # What is supported
//!
//! - BER containers, including the indefinite-length constructed `OCTET STRING`
//!   values Windows and OpenSSL emit (`ber`);
//! - `authenticatedSafe` with any number of `data` and `encryptedData` safe
//!   contents;
//! - `keyBag`, `pkcs8ShroudedKeyBag`, and `certBag` bags;
//! - PBES2 with PBKDF2 and AES-128/192/256-CBC, and PBES1
//!   `pbeWithSHAAnd{2,3}-KeyTripleDES-CBC` (`decrypt`);
//! - MAC verification with HMAC-SHA-1, HMAC-SHA-256, and HMAC-SHA-512.
//!
//! RC2-based PBES1, SHA-224/SHA-384 MACs, `signedData` containers, and CRL or
//! secret bags are rejected or ignored with a typed result rather than guessed
//! at.
//!
//! # Security properties
//!
//! - The password is never logged, formatted, or carried in an error value.
//! - A wrong password and a corrupt container both produce
//!   [`SecurityError::PfxPasswordIncorrect`], so a guess cannot probe the
//!   container. When the container carries a MAC, the check is constant time.
//! - Decrypted key bytes are zeroed when the value is dropped, and every
//!   intermediate key or IV buffer is zeroed before it goes out of scope.
//! - Sizes are bounded: at most [`MAX_CERTIFICATES`](crate::pkcs12::MAX_CERTIFICATES)
//!   certificates, a private key no larger than
//!   [`MAX_PRIVATE_KEY_BYTES`](crate::pkcs12::MAX_PRIVATE_KEY_BYTES), and a
//!   decrypted safe content no larger than
//!   [`MAX_SAFE_CONTENTS_BYTES`](crate::pkcs12::MAX_SAFE_CONTENTS_BYTES).
//! - No network access and no host-clock read.

pub(crate) mod ber;
mod decrypt;
pub(crate) mod kdf;

use ber::{BerReader, Element};
use decrypt::{AlgorithmIdentifier, MacParameters};

use crate::error::{Result, SecurityError};

/// Maximum number of certificates accepted from one container.
pub const MAX_CERTIFICATES: usize = 64;
/// Maximum DER length of the private key accepted from one container.
pub const MAX_PRIVATE_KEY_BYTES: usize = 8 * 1024;
/// Maximum length of one decrypted `SafeContents` value.
pub const MAX_SAFE_CONTENTS_BYTES: usize = 1024 * 1024;

/// `id-data`.
const OID_DATA: &[u32] = &[1, 2, 840, 113_549, 1, 7, 1];
/// `id-encryptedData`.
const OID_ENCRYPTED_DATA: &[u32] = &[1, 2, 840, 113_549, 1, 7, 6];
/// `id-signedData`.
const OID_SIGNED_DATA: &[u32] = &[1, 2, 840, 113_549, 1, 7, 2];
/// `pkcs-12-keyBag`.
const OID_KEY_BAG: &[u32] = &[1, 2, 840, 113_549, 1, 12, 10, 1, 1];
/// `pkcs-12-pkcs8ShroudedKeyBag`.
const OID_SHROUDED_KEY_BAG: &[u32] = &[1, 2, 840, 113_549, 1, 12, 10, 1, 2];
/// `pkcs-12-certBag`.
const OID_CERT_BAG: &[u32] = &[1, 2, 840, 113_549, 1, 12, 10, 1, 3];
/// `x509Certificate`.
const OID_X509_CERTIFICATE: &[u32] = &[1, 2, 840, 113_549, 1, 9, 22, 1];
/// `sha-1` as a digest algorithm.
const OID_SHA1: &[u32] = &[1, 3, 14, 3, 2, 26];
/// `sha-256` as a digest algorithm.
const OID_SHA256: &[u32] = &[2, 16, 840, 1, 101, 3, 4, 2, 1];
/// `sha-512` as a digest algorithm.
const OID_SHA512: &[u32] = &[2, 16, 840, 1, 101, 3, 4, 2, 3];

/// The material recovered from a PKCS#12 container.
///
/// The private key is opaque DER; parsing it is the caller's job. Dropping the
/// value zeroes the key bytes in place.
pub struct PfxContents {
    key_pkcs8: Vec<u8>,
    certificates: Vec<Vec<u8>>,
}

impl PfxContents {
    /// The private key, as a PKCS#8 `PrivateKeyInfo` or, failing that, a
    /// PKCS#1 `RSAPrivateKey` DER value, exactly as it was stored.
    pub fn private_key_der(&self) -> &[u8] {
        &self.key_pkcs8
    }

    /// Every certificate found, in container order, as DER.
    pub fn certificates(&self) -> &[Vec<u8>] {
        &self.certificates
    }
}

impl Drop for PfxContents {
    fn drop(&mut self) {
        for byte in self.key_pkcs8.iter_mut() {
            *byte = 0;
        }
    }
}

impl core::fmt::Debug for PfxContents {
    fn fmt(&self, formatter: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        formatter
            .debug_struct("PfxContents")
            .field(
                "private_key",
                &format_args!("<{} bytes>", self.key_pkcs8.len()),
            )
            .field("certificates", &self.certificates.len())
            .finish()
    }
}

/// Import a PKCS#12 container with the given password.
///
/// # Errors
///
/// - [`SecurityError::PfxMalformed`] for a truncated, misordered, or oversized
///   container;
/// - [`SecurityError::PfxPasswordIncorrect`] when the MAC or the ciphertext does
///   not match the password;
/// - [`SecurityError::PfxUnsupportedAlgorithm`] for an encryption or MAC
///   algorithm this round does not implement;
/// - [`SecurityError::PfxMissingPrivateKey`] / [`SecurityError::PfxMissingCertificate`]
///   when the container carries no usable key or certificate.
pub fn import_pfx(bytes: &[u8], password: &str) -> Result<PfxContents> {
    let mut reader = BerReader::new(bytes);
    let outer = reader.read()?;
    // Some producers pad the file with zero bytes; ignore those but reject any
    // other trailing data.
    if reader.remaining_bytes().iter().any(|byte| *byte != 0) {
        return Err(SecurityError::PfxMalformed("trailing data after the PFX"));
    }
    let mut fields = outer.children()?;
    let version = fields.read()?.integer_u32()?;
    if version != 3 {
        return Err(SecurityError::PfxMalformed("unsupported PFX version"));
    }
    let auth_safe = parse_content_info(&fields.read()?, "authSafe")?;
    let auth_safe_bytes = match auth_safe {
        Content::Data(bytes) => bytes,
        Content::Encrypted(_, _) => {
            return Err(SecurityError::PfxMalformed(
                "authSafe must not be encryptedData",
            ));
        }
        Content::Other(oid) => {
            return Err(SecurityError::PfxUnsupportedAlgorithm(oid_text(&oid)));
        }
    };

    // The MAC, when present, authenticates `authSafe` under the password.
    if !fields.is_empty() {
        let mac_data = fields.read()?;
        let mac = parse_mac_data(&mac_data)?;
        decrypt::verify_mac(&mac, &auth_safe_bytes, password)?;
    }
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed("PFX has trailing fields"));
    }

    let mut key: Option<Vec<u8>> = None;
    let mut certificates = Vec::new();
    let mut safe_reader = BerReader::new(&auth_safe_bytes);
    let authenticated_safe = safe_reader.read()?;
    let mut safes = authenticated_safe.children()?;
    while !safes.is_empty() {
        let content = parse_content_info(&safes.read()?, "authenticatedSafe entry")?;
        let plaintext = match content {
            Content::Data(bytes) => {
                if bytes.len() > MAX_SAFE_CONTENTS_BYTES {
                    return Err(SecurityError::PfxMalformed("safe contents are too large"));
                }
                bytes
            }
            Content::Encrypted(ciphertext, algorithm) => {
                let plaintext = decrypt::decrypt(&algorithm, &ciphertext, password)?;
                if plaintext.len() > MAX_SAFE_CONTENTS_BYTES {
                    return Err(SecurityError::PfxMalformed(
                        "decrypted safe contents are too large",
                    ));
                }
                plaintext
            }
            Content::Other(oid) => {
                return Err(SecurityError::PfxUnsupportedAlgorithm(format!(
                    "authenticatedSafe content {}",
                    oid_text(&oid)
                )));
            }
        };
        collect_bags(&plaintext, password, &mut key, &mut certificates)?;
    }
    if certificates.len() > MAX_CERTIFICATES {
        return Err(SecurityError::PfxMalformed("too many certificates"));
    }
    let key = key.ok_or(SecurityError::PfxMissingPrivateKey)?;
    if certificates.is_empty() {
        return Err(SecurityError::PfxMissingCertificate);
    }
    Ok(PfxContents {
        key_pkcs8: key,
        certificates,
    })
}

/// One decoded `ContentInfo` payload.
enum Content<'a> {
    /// `id-data`, already flattened from its `OCTET STRING`.
    Data(Vec<u8>),
    /// `id-encryptedData`: the ciphertext and its encryption algorithm.
    Encrypted(Vec<u8>, AlgorithmIdentifier<'a>),
    /// Any other content type, reported by OID.
    Other(Vec<u32>),
}

/// Parse `ContentInfo ::= SEQUENCE { contentType OID, content [0] EXPLICIT ANY }`.
fn parse_content_info<'a>(element: &Element<'a>, what: &'static str) -> Result<Content<'a>> {
    let mut fields = element
        .children()
        .map_err(|_| SecurityError::PfxMalformed(what))?;
    let oid = fields.read()?.oid()?;
    let wrapper = fields.read()?;
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed(
            "ContentInfo has trailing fields",
        ));
    }
    let mut wrapped = wrapper.elements()?;
    let value = wrapped.read()?;
    if !wrapped.is_empty() {
        return Err(SecurityError::PfxMalformed(
            "ContentInfo content is not one value",
        ));
    }
    if oid == OID_DATA {
        Ok(Content::Data(value.octets()?))
    } else if oid == OID_ENCRYPTED_DATA {
        let (ciphertext, algorithm) = parse_encrypted_data(&value)?;
        Ok(Content::Encrypted(ciphertext, algorithm))
    } else if oid == OID_SIGNED_DATA {
        Err(SecurityError::PfxUnsupportedAlgorithm(
            "signedData container".to_owned(),
        ))
    } else {
        Ok(Content::Other(oid))
    }
}

/// Parse `EncryptedData` and return its content octets with their algorithm.
fn parse_encrypted_data<'a>(element: &Element<'a>) -> Result<(Vec<u8>, AlgorithmIdentifier<'a>)> {
    let mut fields = element.children()?;
    let _version = fields.read()?.integer_u32()?;
    let encrypted_content_info = fields.read()?;
    let mut info = encrypted_content_info.children()?;
    let content_type = info.read()?.oid()?;
    if content_type != OID_DATA {
        return Err(SecurityError::PfxUnsupportedAlgorithm(format!(
            "encryptedData content type {}",
            oid_text(&content_type)
        )));
    }
    let algorithm = AlgorithmIdentifier::parse(&info.read()?)?;
    let encrypted = info.read()?;
    if !info.is_empty() {
        return Err(SecurityError::PfxMalformed(
            "EncryptedContentInfo has trailing fields",
        ));
    }
    // `encryptedContent` is `[0] IMPLICIT OCTET STRING`; accept the
    // context-specific identifier as well as a plain OCTET STRING.
    let ciphertext = if encrypted.tag() & !0x20 == 0x80 {
        encrypted.implicit_octets()?
    } else {
        encrypted.octets()?
    };
    // Optional `unprotectedAttrs` follow and are not used.
    Ok((ciphertext, algorithm))
}

/// Parse `MacData ::= SEQUENCE { mac DigestInfo, macSalt OCTET STRING,
/// iterations INTEGER DEFAULT 1 }`.
fn parse_mac_data(element: &Element<'_>) -> Result<MacParameters> {
    let mut fields = element.children()?;
    let digest_info = fields.read()?;
    let mut info = digest_info.children()?;
    let algorithm = AlgorithmIdentifier::parse(&info.read()?)?;
    let digest = info.read()?.octets()?;
    let salt = fields.read()?.octets()?;
    let iterations = if fields.is_empty() {
        1
    } else {
        fields.read()?.integer_u32()?
    };
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed("MacData has trailing fields"));
    }
    let prf = match algorithm.oid() {
        oid if oid == OID_SHA1 => kdf::Prf::Sha1,
        oid if oid == OID_SHA256 => kdf::Prf::Sha256,
        oid if oid == OID_SHA512 => kdf::Prf::Sha512,
        other => {
            return Err(SecurityError::PfxUnsupportedAlgorithm(format!(
                "MAC digest {}",
                oid_text(other)
            )));
        }
    };
    if digest.len() != prf.output_len() {
        return Err(SecurityError::PfxMalformed(
            "MAC length does not match its hash",
        ));
    }
    if salt.is_empty() {
        return Err(SecurityError::PfxMalformed("MAC salt is empty"));
    }
    Ok(MacParameters {
        prf,
        digest,
        salt,
        iterations,
    })
}

/// Walk one `SafeContents` value and collect the key and certificates.
fn collect_bags(
    plaintext: &[u8],
    password: &str,
    key: &mut Option<Vec<u8>>,
    certificates: &mut Vec<Vec<u8>>,
) -> Result<()> {
    let mut reader = BerReader::new(plaintext);
    let safe_contents = reader.read()?;
    let mut bags = safe_contents.children()?;
    while !bags.is_empty() {
        let bag = bags.read()?;
        let mut fields = bag.children()?;
        let bag_id = fields.read()?.oid()?;
        let wrapper = fields.read()?;
        let mut wrapped = wrapper.elements()?;
        let value = wrapped.read()?;
        if bag_id == OID_KEY_BAG {
            let der = value.encoded().to_vec();
            store_key(key, der)?;
        } else if bag_id == OID_SHROUDED_KEY_BAG {
            let der = decrypt_shrouded_key(&value, password)?;
            store_key(key, der)?;
        } else if bag_id == OID_CERT_BAG {
            if certificates.len() >= MAX_CERTIFICATES {
                return Err(SecurityError::PfxMalformed("too many certificates"));
            }
            if let Some(der) = parse_cert_bag(&value)? {
                certificates.push(der);
            }
        }
        // `crlBag`, `secretBag`, `safeContentsBag` and unknown bags are not
        // part of an identity and are ignored.
    }
    Ok(())
}

/// Record a private key, refusing a second one.
fn store_key(key: &mut Option<Vec<u8>>, der: Vec<u8>) -> Result<()> {
    if der.len() > MAX_PRIVATE_KEY_BYTES {
        return Err(SecurityError::PfxMalformed("private key is too large"));
    }
    if key.is_some() {
        return Err(SecurityError::PfxMalformed("more than one private key"));
    }
    *key = Some(der);
    Ok(())
}

/// Decrypt `EncryptedPrivateKeyInfo ::= SEQUENCE { algorithm, encryptedData }`.
fn decrypt_shrouded_key(element: &Element<'_>, password: &str) -> Result<Vec<u8>> {
    let mut fields = element.children()?;
    let algorithm = AlgorithmIdentifier::parse(&fields.read()?)?;
    let ciphertext = fields.read()?.octets()?;
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed(
            "EncryptedPrivateKeyInfo has trailing fields",
        ));
    }
    let mut plaintext = decrypt::decrypt(&algorithm, &ciphertext, password)?;
    if plaintext.len() > MAX_PRIVATE_KEY_BYTES {
        for byte in plaintext.iter_mut() {
            *byte = 0;
        }
        return Err(SecurityError::PfxMalformed("private key is too large"));
    }
    Ok(plaintext)
}

/// Parse `CertBag ::= SEQUENCE { certId OID, certValue [0] EXPLICIT OCTET STRING }`.
fn parse_cert_bag(element: &Element<'_>) -> Result<Option<Vec<u8>>> {
    let mut fields = element.children()?;
    let cert_id = fields.read()?.oid()?;
    let wrapper = fields.read()?;
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed("CertBag has trailing fields"));
    }
    let mut wrapped = wrapper.elements()?;
    let value = wrapped.read()?;
    if cert_id != OID_X509_CERTIFICATE {
        // `sdsiCertificate` and other certificate kinds are not identities.
        return Ok(None);
    }
    Ok(Some(value.octets()?))
}

/// Render an OID for a typed error message.
fn oid_text(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

#[cfg(test)]
mod tests {
    //! Synthetic PKCS#12 containers that need no external key material. These
    //! exercise the structural paths and the typed missing-key/missing-
    //! certificate results; real Windows-exported PFX files are covered by the
    //! ignored `pfx_signing` integration test.

    use super::*;

    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(content.len() + 6);
        out.push(tag);
        if content.len() < 0x80 {
            out.push(content.len() as u8);
        } else {
            let bytes = content.len().to_be_bytes();
            let first = bytes
                .iter()
                .position(|byte| *byte != 0)
                .unwrap_or(bytes.len() - 1);
            out.push(0x80 | (bytes.len() - first) as u8);
            out.extend_from_slice(&bytes[first..]);
        }
        out.extend_from_slice(content);
        out
    }

    fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut content = Vec::new();
        for part in parts {
            content.extend_from_slice(part);
        }
        tlv(0x30, &content)
    }

    fn oid(arcs: &[u32]) -> Vec<u8> {
        fn base128(mut value: u64, out: &mut Vec<u8>) {
            let mut buffer = [0_u8; 10];
            let mut index = buffer.len();
            loop {
                index -= 1;
                buffer[index] = (value & 0x7f) as u8;
                value >>= 7;
                if value == 0 {
                    break;
                }
            }
            let end = buffer.len() - 1;
            for byte in &mut buffer[index..end] {
                *byte |= 0x80;
            }
            out.extend_from_slice(&buffer[index..]);
        }
        let mut content = Vec::new();
        base128(u64::from(arcs[0]) * 40 + u64::from(arcs[1]), &mut content);
        for arc in &arcs[2..] {
            base128(u64::from(*arc), &mut content);
        }
        tlv(0x06, &content)
    }

    fn integer(value: u32) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        tlv(0x02, &bytes[first..])
    }

    fn explicit(value: &[u8]) -> Vec<u8> {
        tlv(0xa0, value)
    }

    fn data_content(payload: &[u8]) -> Vec<u8> {
        sequence(&[oid(OID_DATA), explicit(&tlv(0x04, payload))])
    }

    fn key_bag() -> Vec<u8> {
        // The key bag value is stored verbatim, so a synthetic SEQUENCE is
        // enough for the structural import test.
        let private_key_info = sequence(&[integer(0), sequence(&[])]);
        sequence(&[oid(OID_KEY_BAG), explicit(&private_key_info)])
    }

    fn cert_bag() -> Vec<u8> {
        let certificate = sequence(&[integer(1)]);
        let cert_bag = sequence(&[
            oid(OID_X509_CERTIFICATE),
            explicit(&tlv(0x04, &certificate)),
        ]);
        sequence(&[oid(OID_CERT_BAG), explicit(&cert_bag)])
    }

    fn pfx(version: u32, bags: &[Vec<u8>]) -> Vec<u8> {
        let mut contents = Vec::new();
        for bag in bags {
            contents.extend_from_slice(bag);
        }
        let authenticated_safe = sequence(&[data_content(&sequence(&[contents]))]);
        sequence(&[integer(version), data_content(&authenticated_safe)])
    }

    #[test]
    fn a_container_without_a_key_reports_the_typed_error() {
        let cert_only = pfx(3, &[cert_bag()]);
        assert_eq!(
            import_pfx(&cert_only, "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMissingPrivateKey)
        );
        assert_eq!(
            import_pfx(&pfx(3, &[]), "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMissingPrivateKey)
        );
    }

    #[test]
    fn a_container_without_a_certificate_reports_the_typed_error() {
        assert_eq!(
            import_pfx(&pfx(3, &[key_bag()]), "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMissingCertificate)
        );
    }

    #[test]
    fn structural_errors_are_typed() {
        assert_eq!(
            import_pfx(&pfx(2, &[key_bag(), cert_bag()]), "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMalformed("unsupported PFX version"))
        );
        assert_eq!(
            import_pfx(&pfx(3, &[key_bag(), key_bag(), cert_bag()]), "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMalformed("more than one private key"))
        );
        let mut trailing = pfx(3, &[key_bag(), cert_bag()]);
        trailing.push(0x01);
        assert_eq!(
            import_pfx(&trailing, "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMalformed("trailing data after the PFX"))
        );
        assert_eq!(
            import_pfx(b"not a pfx", "irrelevant").map(|_| ()),
            Err(SecurityError::PfxMalformed("truncated value"))
        );
    }
}
