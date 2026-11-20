//! PKCS#12 decryption: PBES2/AES, legacy PBES1/3DES, and the container MAC.
//!
//! Only the schemes that appear in real Windows, OpenSSL, and Java exports are
//! implemented:
//!
//! | Scheme | OID | Status |
//! |---|---|---|
//! | `pbes2` + PBKDF2 + AES-128/192/256-CBC | `1.2.840.113549.1.5.13` | supported |
//! | `pbeWithSHAAnd3-KeyTripleDES-CBC` | `1.2.840.113549.1.12.1.3` | supported |
//! | `pbeWithSHAAnd2-KeyTripleDES-CBC` | `1.2.840.113549.1.12.1.4` | supported |
//! | RC2 based PBES1 (`.5`, `.6`) | `1.2.840.113549.1.12.1` | typed error |
//!
//! A wrong password and a corrupt ciphertext are reported with the same typed
//! error, so a guess cannot be distinguished from damage. Every failure path
//! zeroes the derived key before returning.

use cipher::generic_array::GenericArray;
use cipher::{BlockDecrypt, KeyInit};
use des::TdesEde3;

use super::ber::{Element, INTEGER, SEQUENCE};
use super::kdf::{Prf, hmac, pbkdf2, pkcs12_kdf};
use crate::crypto::aes;
use crate::crypto::constant_time_eq;
use crate::error::{Result, SecurityError};

/// `pbes2`.
const OID_PBES2: &[u32] = &[1, 2, 840, 113_549, 1, 5, 13];
/// `pbkdf2`.
const OID_PBKDF2: &[u32] = &[1, 2, 840, 113_549, 1, 5, 12];
/// `pbeWithSHAAnd3-KeyTripleDES-CBC`.
const OID_PBE_SHA1_3DES: &[u32] = &[1, 2, 840, 113_549, 1, 12, 1, 3];
/// `pbeWithSHAAnd2-KeyTripleDES-CBC`.
const OID_PBE_SHA1_2DES: &[u32] = &[1, 2, 840, 113_549, 1, 12, 1, 4];

/// A parsed `AlgorithmIdentifier`.
pub(super) struct AlgorithmIdentifier<'a> {
    oid: Vec<u32>,
    parameters: Option<Element<'a>>,
}

impl<'a> AlgorithmIdentifier<'a> {
    /// Parse `SEQUENCE { algorithm OID, parameters ANY OPTIONAL }`.
    pub(super) fn parse(element: &Element<'a>) -> Result<Self> {
        let mut children = element.children()?;
        let oid = children.read()?.oid()?;
        let parameters = if children.is_empty() {
            None
        } else {
            Some(children.read()?)
        };
        if !children.is_empty() {
            return Err(SecurityError::PfxMalformed(
                "AlgorithmIdentifier has trailing fields",
            ));
        }
        Ok(Self { oid, parameters })
    }

    /// The algorithm OID arcs.
    pub(super) fn oid(&self) -> &[u32] {
        &self.oid
    }

    /// Parse and return the algorithm parameters, or a typed error when absent.
    pub(super) fn parameters(&self, what: &'static str) -> Result<Element<'a>> {
        self.parameters.ok_or(SecurityError::PfxMalformed(what))
    }

    /// The OID rendered for an error message.
    pub(super) fn oid_text(&self) -> String {
        oid_text(&self.oid)
    }
}

/// Map decryption failures onto the deliberately ambiguous password error.
///
/// A wrong password, a modified ciphertext, and damaged padding are
/// indistinguishable by design.
fn password_error() -> SecurityError {
    SecurityError::PfxPasswordIncorrect
}

/// The UTF-16BE (BMPString) encoding a PKCS#12 KDF expects.///
/// The two-octet terminator is appended by [`pkcs12_kdf`], not here.
pub(super) fn bmp_password(password: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(password.len() * 2);
    for unit in password.encode_utf16() {
        out.extend_from_slice(&unit.to_be_bytes());
    }
    out
}

/// Decrypt a PBES2 `EncryptedPrivateKeyInfo` or `EncryptedData` payload.
fn decrypt_pbes2(
    algorithm: &AlgorithmIdentifier<'_>,
    ciphertext: &[u8],
    password: &str,
) -> Result<Vec<u8>> {
    let params = algorithm.parameters("PBES2 has no parameters")?;
    let mut fields = params.children()?;
    let kdf = AlgorithmIdentifier::parse(&fields.read()?)?;
    let scheme = AlgorithmIdentifier::parse(&fields.read()?)?;
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed("PBES2 parameters are too long"));
    }
    if kdf.oid() != OID_PBKDF2 {
        return Err(SecurityError::PfxUnsupportedAlgorithm(format!(
            "PBES2 key derivation {}",
            kdf.oid_text()
        )));
    }
    let kdf_params = kdf.parameters("PBKDF2 has no parameters")?;
    let mut kdf_fields = kdf_params.children()?;
    let salt = kdf_fields.read()?.octets()?;
    let iterations = kdf_fields.read()?.integer_u32()?;
    let mut key_length = None;
    let mut prf = Prf::Sha1;
    while !kdf_fields.is_empty() {
        let next = kdf_fields.read()?;
        if next.tag() == INTEGER {
            key_length = Some(next.integer_u32()?);
        } else if next.tag() == SEQUENCE {
            let prf_algorithm = AlgorithmIdentifier::parse(&next)?;
            prf = Prf::from_hmac_oid(prf_algorithm.oid()).ok_or_else(|| {
                SecurityError::PfxUnsupportedAlgorithm(format!(
                    "PBKDF2 PRF {}",
                    prf_algorithm.oid_text()
                ))
            })?;
        } else {
            return Err(SecurityError::PfxMalformed("unknown PBKDF2 parameter"));
        }
    }

    // Only AES-CBC encryption schemes are accepted.
    let expected_length = match scheme.oid() {
        [2, 16, 840, 1, 101, 3, 4, 1, 2] => 16,
        [2, 16, 840, 1, 101, 3, 4, 1, 22] => 24,
        [2, 16, 840, 1, 101, 3, 4, 1, 42] => 32,
        _ => {
            return Err(SecurityError::PfxUnsupportedAlgorithm(format!(
                "PBES2 encryption scheme {}",
                scheme.oid_text()
            )));
        }
    };
    if let Some(declared) = key_length
        && declared as usize != expected_length
    {
        return Err(SecurityError::PfxMalformed(
            "PBKDF2 key length does not match the cipher",
        ));
    }
    let iv_bytes = scheme.parameters("AES-CBC has no IV")?.octets()?;
    let iv: [u8; 16] = iv_bytes
        .as_slice()
        .try_into()
        .map_err(|_| SecurityError::PfxMalformed("AES-CBC IV is not 16 bytes"))?;

    // PBES2 treats the password as an octet string; every producer this crate
    // interops with uses the UTF-8 bytes.
    let mut key = pbkdf2(prf, password.as_bytes(), &salt, iterations, expected_length)?;
    let plaintext = aes::decrypt_cbc(&key, &iv, ciphertext).map_err(|_| password_error());
    zero(&mut key);
    plaintext
}

/// Decrypt a legacy PBES1 3DES `EncryptedPrivateKeyInfo` payload.
fn decrypt_pbes1_des(
    algorithm: &AlgorithmIdentifier<'_>,
    ciphertext: &[u8],
    password: &str,
    two_key: bool,
) -> Result<Vec<u8>> {
    let params = algorithm.parameters("PBES1 has no parameters")?;
    let mut fields = params.children()?;
    let salt = fields.read()?.octets()?;
    let iterations = fields.read()?.integer_u32()?;
    if !fields.is_empty() {
        return Err(SecurityError::PfxMalformed("PBES1 parameters are too long"));
    }
    let mut bmp = bmp_password(password);
    let mut derived = pkcs12_kdf(Prf::Sha1, Some(&bmp), &salt, 1, iterations, 24)?;
    let iv_bytes = pkcs12_kdf(Prf::Sha1, Some(&bmp), &salt, 2, iterations, 8)?;
    zero(&mut bmp);
    let mut iv = [0_u8; 8];
    iv.copy_from_slice(&iv_bytes);
    if two_key {
        // Two-key 3DES expands to K1 || K2 || K1.
        let first_half: [u8; 8] = derived[..8]
            .try_into()
            .map_err(|_| SecurityError::PfxMalformed("3DES key is too short"))?;
        derived[16..24].copy_from_slice(&first_half);
    }
    let cipher = TdesEde3::new_from_slice(&derived)
        .map_err(|_| SecurityError::PfxUnsupportedAlgorithm("3DES key length".to_owned()))?;
    let plaintext = decrypt_cbc_8(&cipher, &iv, ciphertext);
    zero(&mut derived);
    plaintext
}

/// Decrypt 8-byte-block CBC and remove PKCS#7 padding.
fn decrypt_cbc_8(cipher: &TdesEde3, iv: &[u8; 8], ciphertext: &[u8]) -> Result<Vec<u8>> {
    if ciphertext.is_empty() || !ciphertext.len().is_multiple_of(8) {
        return Err(password_error());
    }
    let mut previous = *iv;
    let mut out = Vec::with_capacity(ciphertext.len());
    for chunk in ciphertext.as_chunks::<8>().0 {
        let mut block = [0_u8; 8];
        block.copy_from_slice(chunk);
        let mut decrypted = block;
        cipher.decrypt_block(GenericArray::from_mut_slice(&mut decrypted));
        for (index, byte) in decrypted.iter().enumerate() {
            out.push(byte ^ previous[index]);
        }
        previous = block;
    }
    unpad_8(&out).map(<[u8]>::to_vec).ok_or_else(password_error)
}

/// Remove PKCS#7 padding for an 8-byte block cipher.
fn unpad_8(data: &[u8]) -> Option<&[u8]> {
    let pad = usize::from(*data.last()?);
    if pad == 0 || pad > 8 || pad > data.len() {
        return None;
    }
    if data[data.len() - pad..]
        .iter()
        .any(|byte| usize::from(*byte) != pad)
    {
        return None;
    }
    Some(&data[..data.len() - pad])
}

/// Decrypt an `EncryptedPrivateKeyInfo` or `EncryptedData` content.
///
/// `algorithm` is the `AlgorithmIdentifier` from the structure and `ciphertext`
/// its encrypted content octets.
pub(super) fn decrypt(
    algorithm: &AlgorithmIdentifier<'_>,
    ciphertext: &[u8],
    password: &str,
) -> Result<Vec<u8>> {
    let oid = algorithm.oid();
    if oid == OID_PBES2 {
        decrypt_pbes2(algorithm, ciphertext, password)
    } else if oid == OID_PBE_SHA1_3DES {
        decrypt_pbes1_des(algorithm, ciphertext, password, false)
    } else if oid == OID_PBE_SHA1_2DES {
        decrypt_pbes1_des(algorithm, ciphertext, password, true)
    } else {
        Err(SecurityError::PfxUnsupportedAlgorithm(algorithm.oid_text()))
    }
}

/// The MAC recorded by a PKCS#12 `MacData` value.
#[derive(Debug, Clone)]
pub(super) struct MacParameters {
    /// Hash used by the MAC.
    pub(super) prf: Prf,
    /// Recorded MAC value.
    pub(super) digest: Vec<u8>,
    /// MAC salt.
    pub(super) salt: Vec<u8>,
    /// Key-derivation iteration count.
    pub(super) iterations: u32,
}

/// Verify `MacData` over the `authSafe` content.
///
/// Returns [`SecurityError::PfxPasswordIncorrect`] on any mismatch: a wrong
/// password and a modified container are deliberately indistinguishable.
pub(super) fn verify_mac(mac: &MacParameters, auth_safe: &[u8], password: &str) -> Result<()> {
    let mut bmp = bmp_password(password);
    let mut key = pkcs12_kdf(
        mac.prf,
        Some(&bmp),
        &mac.salt,
        3,
        mac.iterations,
        mac.prf.output_len(),
    )?;
    let computed = hmac(mac.prf, &key, auth_safe);
    zero(&mut key);
    zero(&mut bmp);
    if constant_time_eq(&computed, &mac.digest) {
        Ok(())
    } else {
        Err(password_error())
    }
}

/// Render an OID for a typed error message.
fn oid_text(oid: &[u32]) -> String {
    oid.iter().map(u32::to_string).collect::<Vec<_>>().join(".")
}

/// Overwrite a buffer that held key material.
fn zero(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        *byte = 0;
    }
}

#[cfg(test)]
mod tests {
    use cipher::BlockEncrypt;

    use super::super::ber::BerReader;
    use super::*;

    /// Encode a BER/DER tag, length, and content.
    fn tlv(tag: u8, content: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(content.len() + 6);
        out.push(tag);
        encode_length(content.len(), &mut out);
        out.extend_from_slice(content);
        out
    }

    fn encode_length(length: usize, out: &mut Vec<u8>) {
        if length < 0x80 {
            out.push(length as u8);
            return;
        }
        let bytes = length.to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        let significant = &bytes[first..];
        out.push(0x80 | significant.len() as u8);
        out.extend_from_slice(significant);
    }

    fn sequence(parts: &[Vec<u8>]) -> Vec<u8> {
        let mut content = Vec::new();
        for part in parts {
            content.extend_from_slice(part);
        }
        tlv(0x30, &content)
    }

    fn integer(value: u32) -> Vec<u8> {
        let bytes = value.to_be_bytes();
        let first = bytes
            .iter()
            .position(|byte| *byte != 0)
            .unwrap_or(bytes.len() - 1);
        let mut content = bytes[first..].to_vec();
        if content[0] & 0x80 != 0 {
            content.insert(0, 0);
        }
        tlv(0x02, &content)
    }

    fn octet_string(value: &[u8]) -> Vec<u8> {
        tlv(0x04, value)
    }

    fn null() -> Vec<u8> {
        tlv(0x05, &[])
    }

    fn oid(arcs: &[u32]) -> Vec<u8> {
        let mut content = Vec::new();
        encode_base128(u64::from(arcs[0]) * 40 + u64::from(arcs[1]), &mut content);
        for arc in &arcs[2..] {
            encode_base128(u64::from(*arc), &mut content);
        }
        tlv(0x06, &content)
    }

    fn encode_base128(mut value: u64, out: &mut Vec<u8>) {
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

    fn parse_algorithm(der: &[u8]) -> AlgorithmIdentifier<'_> {
        let mut reader = BerReader::new(der);
        let element = reader.read().expect("read AlgorithmIdentifier");
        assert!(reader.is_empty());
        AlgorithmIdentifier::parse(&element).expect("parse AlgorithmIdentifier")
    }

    fn encrypt_cbc_8(key: &[u8], iv: &[u8; 8], plaintext: &[u8]) -> Vec<u8> {
        let cipher = TdesEde3::new_from_slice(key).expect("3DES key");
        let pad = 8 - (plaintext.len() % 8);
        let mut padded = plaintext.to_vec();
        padded.resize(plaintext.len() + pad, pad as u8);
        let mut previous = *iv;
        let mut out = Vec::with_capacity(padded.len());
        for chunk in padded.as_chunks::<8>().0 {
            let mut block = [0_u8; 8];
            for index in 0..8 {
                block[index] = chunk[index] ^ previous[index];
            }
            let mut encrypted = block;
            cipher.encrypt_block(GenericArray::from_mut_slice(&mut encrypted));
            previous = encrypted;
            out.extend_from_slice(&encrypted);
        }
        out
    }

    #[test]
    fn three_des_block_matches_a_known_answer() {
        let key: Vec<u8> = (1..=24).collect();
        let cipher = TdesEde3::new_from_slice(&key).expect("3DES key");
        let mut block = *b"12345678";
        cipher.encrypt_block(GenericArray::from_mut_slice(&mut block));
        assert_eq!(block, [0xbd, 0xf5, 0xd7, 0x4d, 0x7d, 0x00, 0x8d, 0x0b]);
    }

    #[test]
    fn bmp_passwords_are_utf16_be() {
        assert_eq!(bmp_password(""), Vec::<u8>::new());
        assert_eq!(bmp_password("ab"), vec![0, b'a', 0, b'b']);
        // A non-BMP character becomes a surrogate pair, big-endian.
        assert_eq!(bmp_password("\u{1f600}"), vec![0xd8, 0x3d, 0xde, 0x00]);
    }

    #[test]
    fn eight_byte_padding_is_strict() {
        assert_eq!(
            unpad_8(&[1, 2, 3, 4, 4, 4, 4, 4]),
            Some(&[1_u8, 2, 3, 4][..])
        );
        assert_eq!(unpad_8(&[8; 8]), Some(&[][..]));
        assert_eq!(unpad_8(&[0; 8]), None);
        assert_eq!(unpad_8(&[9, 0, 0, 0, 0, 0, 0, 0]), None);
        assert_eq!(unpad_8(&[1, 2, 3, 4, 4, 4, 4, 5]), None);
        assert_eq!(unpad_8(&[]), None);
    }

    #[test]
    fn pbes2_aes256_round_trips_and_maps_wrong_password() {
        let password = "synthetic-pbes2";
        let salt = b"synthetic-salt-1";
        let iterations = 1_000;
        let iv = [0x42_u8; 16];
        let plaintext = b"PKCS#8 shaped private key bytes";
        let key = pbkdf2(Prf::Sha256, password.as_bytes(), salt, iterations, 32)
            .expect("derive PBES2 key");
        let ciphertext = aes::encrypt_cbc(&key, &iv, plaintext).expect("encrypt synthetic key");

        let kdf = sequence(&[
            oid(&[1, 2, 840, 113_549, 1, 5, 12]),
            sequence(&[
                octet_string(salt),
                integer(iterations),
                integer(32),
                sequence(&[oid(&[1, 2, 840, 113_549, 2, 9]), null()]),
            ]),
        ]);
        let scheme = sequence(&[oid(&[2, 16, 840, 1, 101, 3, 4, 1, 42]), octet_string(&iv)]);
        let algorithm_der = sequence(&[
            oid(&[1, 2, 840, 113_549, 1, 5, 13]),
            sequence(&[kdf, scheme]),
        ]);
        let algorithm = parse_algorithm(&algorithm_der);

        assert_eq!(
            decrypt(&algorithm, &ciphertext, password).expect("decrypt synthetic key"),
            plaintext
        );
        assert_eq!(
            decrypt(&algorithm, &ciphertext, "wrong password").map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect)
        );
        // A truncated ciphertext is a decryption failure, reported with the
        // same ambiguous error as a wrong password.
        assert_eq!(
            decrypt(&algorithm, &ciphertext[..15], password).map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect)
        );
    }

    #[test]
    fn pbes2_rejects_a_declared_key_length_that_does_not_match_the_cipher() {
        let salt = b"salt";
        let iv = [0_u8; 16];
        let kdf = sequence(&[
            oid(&[1, 2, 840, 113_549, 1, 5, 12]),
            sequence(&[
                octet_string(salt),
                integer(1),
                integer(16),
                sequence(&[oid(&[1, 2, 840, 113_549, 2, 9]), null()]),
            ]),
        ]);
        let scheme = sequence(&[oid(&[2, 16, 840, 1, 101, 3, 4, 1, 42]), octet_string(&iv)]);
        let algorithm_der = sequence(&[
            oid(&[1, 2, 840, 113_549, 1, 5, 13]),
            sequence(&[kdf, scheme]),
        ]);
        let algorithm = parse_algorithm(&algorithm_der);
        assert_eq!(
            decrypt(&algorithm, &[0_u8; 16], "password").map(|_| ()),
            Err(SecurityError::PfxMalformed(
                "PBKDF2 key length does not match the cipher"
            ))
        );
    }

    #[test]
    fn pbes1_three_des_round_trips_for_both_key_variants() {
        for (label, oid_arcs, two_key) in [
            ("3-key", &[1, 2, 840, 113_549, 1, 12, 1, 3][..], false),
            ("2-key", &[1, 2, 840, 113_549, 1, 12, 1, 4][..], true),
        ] {
            let password = "synthetic-pbes1";
            let salt = b"saltsalt";
            let iterations = 1_000;
            let plaintext = b"PKCS#1 RSA private key bytes";
            let mut bmp = bmp_password(password);
            let mut key =
                pkcs12_kdf(Prf::Sha1, Some(&bmp), salt, 1, iterations, 24).expect("derive key");
            let iv_bytes =
                pkcs12_kdf(Prf::Sha1, Some(&bmp), salt, 2, iterations, 8).expect("derive IV");
            zero(&mut bmp);
            let iv: [u8; 8] = iv_bytes.as_slice().try_into().expect("8-byte IV");
            if two_key {
                let first: [u8; 8] = key[..8].try_into().expect("first key half");
                key[16..24].copy_from_slice(&first);
            }
            let ciphertext = encrypt_cbc_8(&key, &iv, plaintext);
            let params = sequence(&[octet_string(salt), integer(iterations)]);
            let algorithm_der = sequence(&[oid(oid_arcs), params]);
            let algorithm = parse_algorithm(&algorithm_der);

            assert_eq!(
                decrypt(&algorithm, &ciphertext, password).expect("decrypt synthetic key"),
                plaintext,
                "{label} PBES1 round trip"
            );
            assert_eq!(
                decrypt(&algorithm, &ciphertext, "wrong password").map(|_| ()),
                Err(SecurityError::PfxPasswordIncorrect),
                "{label} wrong-password mapping"
            );
        }
    }

    #[test]
    fn mac_failure_and_wrong_password_are_indistinguishable() {
        let password = "mac-password";
        let salt = b"mac-salt";
        let iterations = 1_000;
        let auth_safe = b"authenticated safe bytes";
        let mut bmp = bmp_password(password);
        let key =
            pkcs12_kdf(Prf::Sha1, Some(&bmp), salt, 3, iterations, 20).expect("derive MAC key");
        zero(&mut bmp);
        let digest = hmac(Prf::Sha1, &key, auth_safe);
        let mac = MacParameters {
            prf: Prf::Sha1,
            digest,
            salt: salt.to_vec(),
            iterations,
        };

        assert_eq!(verify_mac(&mac, auth_safe, password), Ok(()));
        assert_eq!(
            verify_mac(&mac, auth_safe, "wrong password").map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect)
        );
        let mut tampered_mac = mac.clone();
        tampered_mac.digest[0] ^= 0x01;
        assert_eq!(
            verify_mac(&tampered_mac, auth_safe, password).map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect)
        );
        assert_eq!(
            verify_mac(&mac, b"modified authenticated safe", password).map(|_| ()),
            Err(SecurityError::PfxPasswordIncorrect)
        );
    }
}
