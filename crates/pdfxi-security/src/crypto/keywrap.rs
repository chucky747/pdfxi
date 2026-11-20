//! RFC 3394 AES Key Wrap (AES-KW).
//!
//! This module implements the authenticated key-wrap and key-unwrap algorithms
//! from RFC 3394 sections 2.2.1 and 2.2.2, built on the FIPS 197 AES block
//! cipher in [`super::aes`]. AES-KW wraps a content-encryption key (CEK) under a
//! key-encryption key (KEK) and carries a fixed `A6A6A6A6A6A6A6A6` integrity
//! check value, so a wrong KEK or a modified ciphertext is rejected instead of
//! yielding a corrupt key.
//!
//! The key data must be at least two 64-bit semiblocks (16 bytes) and a multiple
//! of 8 bytes; the KEK must be 16, 24, or 32 bytes. The wrapped output is the
//! input length plus one 8-byte semiblock.
//!
//! No item here logs its inputs. Keys and plaintexts are ordinary byte slices
//! and are never written to diagnostics.

use super::aes::Aes;
use crate::error::{Result, SecurityError};

/// The fixed initial value / integrity check register from RFC 3394 section 2.2.1.
const AES_KW_IV: [u8; 8] = [0xA6; 8];

/// Wrap `key_data` under `kek` using RFC 3394 AES Key Wrap.
///
/// # Errors
///
/// - [`SecurityError::AesKwInvalidKekLength`] when `kek` is not 16, 24, or 32
///   bytes.
/// - [`SecurityError::AesKwInvalidKeyDataLength`] when `key_data` is shorter
///   than 16 bytes or not a multiple of 8 bytes.
pub fn aes_key_wrap(kek: &[u8], key_data: &[u8]) -> Result<Vec<u8>> {
    let cipher = validate_kek(kek)?;
    let n = validate_semiblocks("key data", key_data, 2)?;

    let mut a = AES_KW_IV;
    let mut blocks: Vec<[u8; 8]> = key_data
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| {
            let mut block = [0u8; 8];
            block.copy_from_slice(chunk);
            block
        })
        .collect();

    for j in 0..6u64 {
        for (i, block) in blocks.iter_mut().enumerate() {
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&a);
            input[8..].copy_from_slice(block);
            let b = cipher.encrypt_block(&input);
            let t = n as u64 * j + i as u64 + 1;
            a.copy_from_slice(&b[..8]);
            xor_u64_be(&mut a, t);
            block.copy_from_slice(&b[8..]);
        }
    }

    let mut wrapped = Vec::with_capacity(key_data.len() + 8);
    wrapped.extend_from_slice(&a);
    for block in &blocks {
        wrapped.extend_from_slice(block);
    }
    Ok(wrapped)
}

/// Unwrap `wrapped` under `kek` using RFC 3394 AES Key Unwrap.
///
/// Returns a typed [`SecurityError::AesKwIntegrityCheckFailed`] when the
/// integrity register does not match the RFC 3394 `A6A6A6A6A6A6A6A6` value,
/// which is how a wrong KEK or a modified ciphertext is detected.
///
/// # Errors
///
/// - [`SecurityError::AesKwInvalidKekLength`] when `kek` is not 16, 24, or 32
///   bytes.
/// - [`SecurityError::AesKwInvalidKeyDataLength`] when `wrapped` is shorter
///   than 24 bytes or not a multiple of 8 bytes.
/// - [`SecurityError::AesKwIntegrityCheckFailed`] when the integrity check
///   fails.
pub fn aes_key_unwrap(kek: &[u8], wrapped: &[u8]) -> Result<Vec<u8>> {
    let cipher = validate_kek(kek)?;
    // Wrapped input is n+1 semiblocks; require at least n=2 data blocks.
    let blocks_total = validate_semiblocks("wrapped key", wrapped, 3)?;
    let n = blocks_total - 1;

    let (a_tlv, rest) = wrapped.split_at(8);
    let mut a = [0u8; 8];
    a.copy_from_slice(a_tlv);
    let mut blocks: Vec<[u8; 8]> = rest
        .as_chunks::<8>()
        .0
        .iter()
        .map(|chunk| {
            let mut block = [0u8; 8];
            block.copy_from_slice(chunk);
            block
        })
        .collect();

    for j in (0..6u64).rev() {
        for i in (0..n).rev() {
            let t = n as u64 * j + i as u64 + 1;
            let mut a_xor_t = a;
            xor_u64_be(&mut a_xor_t, t);
            let mut input = [0u8; 16];
            input[..8].copy_from_slice(&a_xor_t);
            input[8..].copy_from_slice(&blocks[i]);
            let b = cipher.decrypt_block(&input);
            a.copy_from_slice(&b[..8]);
            blocks[i].copy_from_slice(&b[8..]);
        }
    }

    if !crate::crypto::constant_time_eq(&a, &AES_KW_IV) {
        return Err(SecurityError::AesKwIntegrityCheckFailed);
    }

    let mut key_data = Vec::with_capacity(n * 8);
    for block in &blocks {
        key_data.extend_from_slice(block);
    }
    Ok(key_data)
}

fn validate_kek(kek: &[u8]) -> Result<Aes> {
    match kek.len() {
        16 | 24 | 32 => Aes::new(kek),
        other => Err(SecurityError::AesKwInvalidKekLength(other)),
    }
}

fn validate_semiblocks(label: &'static str, data: &[u8], min: usize) -> Result<usize> {
    if data.len() < min * 8 || !data.len().is_multiple_of(8) {
        return Err(SecurityError::AesKwInvalidKeyDataLength {
            label,
            len: data.len(),
        });
    }
    Ok(data.len() / 8)
}

fn xor_u64_be(target: &mut [u8; 8], value: u64) {
    let bytes = value.to_be_bytes();
    for i in 0..8 {
        target[i] ^= bytes[i];
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(text: &str) -> Vec<u8> {
        let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
        assert!(clean.len().is_multiple_of(2));
        let mut out = Vec::with_capacity(clean.len() / 2);
        let bytes = clean.as_bytes();
        let mut i = 0;
        while i < bytes.len() {
            let hi = (bytes[i] as char).to_digit(16).expect("hex digit");
            let lo = (bytes[i + 1] as char).to_digit(16).expect("hex digit");
            out.push(((hi << 4) | lo) as u8);
            i += 2;
        }
        out
    }

    const KEY_DATA_1: &str = "00112233445566778899aabbccddeeff";
    const KEY_DATA_2: &str = "00112233445566778899aabbccddeeff0001020304050607";

    fn check_vector(kek_hex: &str, data_hex: &str, expected_hex: &str) {
        let kek = hex(kek_hex);
        let data = hex(data_hex);
        let expected = hex(expected_hex);
        let wrapped = aes_key_wrap(&kek, &data).expect("wrap vector");
        assert_eq!(wrapped, expected);
        assert_eq!(wrapped.len(), data.len() + 8);
        assert_eq!(aes_key_unwrap(&kek, &wrapped).expect("unwrap vector"), data);
    }

    #[test]
    fn rfc3394_wrap_vector_128_bit_kek() {
        check_vector(
            "000102030405060708090a0b0c0d0e0f",
            KEY_DATA_1,
            "1fa68b0a8112b447aef34bd8fb5a7b829d3e862371d2cfe5",
        );
    }

    #[test]
    fn rfc3394_wrap_vector_192_bit_kek() {
        check_vector(
            "000102030405060708090a0b0c0d0e0f1011121314151617",
            KEY_DATA_1,
            "96778b25ae6ca435f92b5b97c050aed2468ab8a17ad84e5d",
        );
    }

    #[test]
    fn rfc3394_wrap_vector_256_bit_kek() {
        check_vector(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            KEY_DATA_1,
            "64e8c3f9ce0f5ba263e9777905818a2a93c8191e7d6e8ae7",
        );
    }

    #[test]
    fn rfc3394_wrap_vector_multiple_semiblocks() {
        check_vector(
            "000102030405060708090a0b0c0d0e0f1011121314151617",
            KEY_DATA_2,
            "031d33264e15d33268f24ec260743edce1c6c7dde e725a936ba814915c6762d2",
        );
        check_vector(
            "000102030405060708090a0b0c0d0e0f101112131415161718191a1b1c1d1e1f",
            KEY_DATA_2,
            "a8f9bc1612c68b3ff6e6f4fbe30e71e4769c8b80a32cb8958cd5d17d6b254da1",
        );
    }

    #[test]
    fn rfc3394_unwrap_rejects_tampered_ciphertext() {
        let kek = hex("000102030405060708090a0b0c0d0e0f");
        let mut wrapped = aes_key_wrap(&kek, &hex(KEY_DATA_1)).expect("wrap");
        wrapped[10] ^= 0x01;
        assert_eq!(
            aes_key_unwrap(&kek, &wrapped),
            Err(SecurityError::AesKwIntegrityCheckFailed)
        );
    }

    #[test]
    fn rfc3394_unwrap_rejects_wrong_kek() {
        let wrapped =
            aes_key_wrap(&hex("000102030405060708090a0b0c0d0e0f"), &hex(KEY_DATA_1)).expect("wrap");
        let wrong_kek = hex("0f0e0d0c0b0a09080706050403020100");
        assert_eq!(
            aes_key_unwrap(&wrong_kek, &wrapped),
            Err(SecurityError::AesKwIntegrityCheckFailed)
        );
    }

    #[test]
    fn rfc3394_rejects_malformed_lengths() {
        let kek = [0x11u8; 16];
        assert_eq!(
            aes_key_wrap(&[0u8; 12], &[0u8; 16]),
            Err(SecurityError::AesKwInvalidKekLength(12))
        );
        assert_eq!(
            aes_key_wrap(&kek, &[0u8; 8]),
            Err(SecurityError::AesKwInvalidKeyDataLength {
                label: "key data",
                len: 8,
            })
        );
        assert_eq!(
            aes_key_wrap(&kek, &[0u8; 17]),
            Err(SecurityError::AesKwInvalidKeyDataLength {
                label: "key data",
                len: 17,
            })
        );
        assert_eq!(
            aes_key_unwrap(&kek, &[0u8; 16]),
            Err(SecurityError::AesKwInvalidKeyDataLength {
                label: "wrapped key",
                len: 16,
            })
        );
        assert_eq!(
            aes_key_unwrap(&kek, &[0u8; 25]),
            Err(SecurityError::AesKwInvalidKeyDataLength {
                label: "wrapped key",
                len: 25,
            })
        );
        assert_eq!(
            aes_key_unwrap(&[0u8; 20], &[0u8; 24]),
            Err(SecurityError::AesKwInvalidKekLength(20))
        );
    }

    #[test]
    fn rfc3394_round_trip_for_all_kek_sizes() {
        let data = hex(KEY_DATA_2);
        for kek_len in [16, 24, 32] {
            let kek = vec![0x2bu8; kek_len];
            let wrapped = aes_key_wrap(&kek, &data).expect("wrap");
            assert_eq!(aes_key_unwrap(&kek, &wrapped).expect("unwrap"), data);
        }
    }
}
