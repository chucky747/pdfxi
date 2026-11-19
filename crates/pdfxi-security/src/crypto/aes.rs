//! AES block cipher and CBC mode (FIPS 197, NIST SP 800-38A).
//!
//! The block cipher supports 128-, 192-, and 256-bit keys. CBC mode is exposed
//! both with PKCS#7 padding (used for PDF streams and strings) and without it
//! (used to wrap the AES-256 file key in the `/UE` and `/OE` entries).

use crate::error::{Result, SecurityError};

/// The AES S-box from FIPS 197 Figure 7.
const SBOX: [u8; 256] = [
    0x63, 0x7c, 0x77, 0x7b, 0xf2, 0x6b, 0x6f, 0xc5, 0x30, 0x01, 0x67, 0x2b, 0xfe, 0xd7, 0xab, 0x76,
    0xca, 0x82, 0xc9, 0x7d, 0xfa, 0x59, 0x47, 0xf0, 0xad, 0xd4, 0xa2, 0xaf, 0x9c, 0xa4, 0x72, 0xc0,
    0xb7, 0xfd, 0x93, 0x26, 0x36, 0x3f, 0xf7, 0xcc, 0x34, 0xa5, 0xe5, 0xf1, 0x71, 0xd8, 0x31, 0x15,
    0x04, 0xc7, 0x23, 0xc3, 0x18, 0x96, 0x05, 0x9a, 0x07, 0x12, 0x80, 0xe2, 0xeb, 0x27, 0xb2, 0x75,
    0x09, 0x83, 0x2c, 0x1a, 0x1b, 0x6e, 0x5a, 0xa0, 0x52, 0x3b, 0xd6, 0xb3, 0x29, 0xe3, 0x2f, 0x84,
    0x53, 0xd1, 0x00, 0xed, 0x20, 0xfc, 0xb1, 0x5b, 0x6a, 0xcb, 0xbe, 0x39, 0x4a, 0x4c, 0x58, 0xcf,
    0xd0, 0xef, 0xaa, 0xfb, 0x43, 0x4d, 0x33, 0x85, 0x45, 0xf9, 0x02, 0x7f, 0x50, 0x3c, 0x9f, 0xa8,
    0x51, 0xa3, 0x40, 0x8f, 0x92, 0x9d, 0x38, 0xf5, 0xbc, 0xb6, 0xda, 0x21, 0x10, 0xff, 0xf3, 0xd2,
    0xcd, 0x0c, 0x13, 0xec, 0x5f, 0x97, 0x44, 0x17, 0xc4, 0xa7, 0x7e, 0x3d, 0x64, 0x5d, 0x19, 0x73,
    0x60, 0x81, 0x4f, 0xdc, 0x22, 0x2a, 0x90, 0x88, 0x46, 0xee, 0xb8, 0x14, 0xde, 0x5e, 0x0b, 0xdb,
    0xe0, 0x32, 0x3a, 0x0a, 0x49, 0x06, 0x24, 0x5c, 0xc2, 0xd3, 0xac, 0x62, 0x91, 0x95, 0xe4, 0x79,
    0xe7, 0xc8, 0x37, 0x6d, 0x8d, 0xd5, 0x4e, 0xa9, 0x6c, 0x56, 0xf4, 0xea, 0x65, 0x7a, 0xae, 0x08,
    0xba, 0x78, 0x25, 0x2e, 0x1c, 0xa6, 0xb4, 0xc6, 0xe8, 0xdd, 0x74, 0x1f, 0x4b, 0xbd, 0x8b, 0x8a,
    0x70, 0x3e, 0xb5, 0x66, 0x48, 0x03, 0xf6, 0x0e, 0x61, 0x35, 0x57, 0xb9, 0x86, 0xc1, 0x1d, 0x9e,
    0xe1, 0xf8, 0x98, 0x11, 0x69, 0xd9, 0x8e, 0x94, 0x9b, 0x1e, 0x87, 0xe9, 0xce, 0x55, 0x28, 0xdf,
    0x8c, 0xa1, 0x89, 0x0d, 0xbf, 0xe6, 0x42, 0x68, 0x41, 0x99, 0x2d, 0x0f, 0xb0, 0x54, 0xbb, 0x16,
];

/// Inverse S-box, derived from [`SBOX`] at compile time to avoid a second
/// hand-transcribed table.
const INV_SBOX: [u8; 256] = build_inv_sbox();

const fn build_inv_sbox() -> [u8; 256] {
    let mut inverse = [0u8; 256];
    let mut i = 0usize;
    while i < 256 {
        inverse[SBOX[i] as usize] = i as u8;
        i += 1;
    }
    inverse
}

/// Multiply two bytes in GF(2^8) with the AES reduction polynomial
/// `x^8 + x^4 + x^3 + x + 1`.
fn gf_mul(mut a: u8, mut b: u8) -> u8 {
    let mut product = 0u8;
    for _ in 0..8 {
        if b & 1 != 0 {
            product ^= a;
        }
        let high = a & 0x80;
        a <<= 1;
        if high != 0 {
            a ^= 0x1b;
        }
        b >>= 1;
    }
    product
}

/// AES in ECB mode for a single 16-byte block.
///
/// `key` must be 16, 24, or 32 bytes.
#[derive(Clone)]
pub struct Aes {
    round_keys: Vec<u32>,
    rounds: usize,
}

impl Aes {
    /// Expand `key` into the round keys.
    ///
    /// Returns [`SecurityError::InvalidKeyLength`] unless the key is 16, 24, or
    /// 32 bytes long.
    pub fn new(key: &[u8]) -> Result<Self> {
        let nk = match key.len() {
            16 => 4,
            24 => 6,
            32 => 8,
            other => return Err(SecurityError::InvalidKeyLength(other as u16 * 8)),
        };
        let rounds = nk + 6;
        let total_words = 4 * (rounds + 1);
        let mut round_keys = vec![0u32; total_words];
        for i in 0..nk {
            let bytes = &key[i * 4..i * 4 + 4];
            round_keys[i] = u32::from_be_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
        }
        for i in nk..total_words {
            let mut temp = round_keys[i - 1];
            if i % nk == 0 {
                temp = sub_word(temp.rotate_left(8)) ^ ((rcon(i / nk) as u32) << 24);
            } else if nk > 6 && i % nk == 4 {
                temp = sub_word(temp);
            }
            round_keys[i] = round_keys[i - nk] ^ temp;
        }
        Ok(Self { round_keys, rounds })
    }

    /// Encrypt one 16-byte block with the expanded key.
    pub fn encrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut state = *input;
        add_round_key(&mut state, &self.round_keys, 0);
        for round in 1..self.rounds {
            sub_bytes(&mut state, &SBOX);
            shift_rows(&mut state);
            mix_columns(&mut state);
            add_round_key(&mut state, &self.round_keys, round);
        }
        sub_bytes(&mut state, &SBOX);
        shift_rows(&mut state);
        add_round_key(&mut state, &self.round_keys, self.rounds);
        state
    }

    /// Decrypt one 16-byte block with the expanded key.
    pub fn decrypt_block(&self, input: &[u8; 16]) -> [u8; 16] {
        let mut state = *input;
        add_round_key(&mut state, &self.round_keys, self.rounds);
        for round in (1..self.rounds).rev() {
            inv_shift_rows(&mut state);
            sub_bytes(&mut state, &INV_SBOX);
            add_round_key(&mut state, &self.round_keys, round);
            inv_mix_columns(&mut state);
        }
        inv_shift_rows(&mut state);
        sub_bytes(&mut state, &INV_SBOX);
        add_round_key(&mut state, &self.round_keys, 0);
        state
    }

    /// Number of AES rounds (10, 12, or 14).
    pub fn rounds(&self) -> usize {
        self.rounds
    }
}

fn sub_word(word: u32) -> u32 {
    let bytes = word.to_be_bytes();
    u32::from_be_bytes([
        SBOX[bytes[0] as usize],
        SBOX[bytes[1] as usize],
        SBOX[bytes[2] as usize],
        SBOX[bytes[3] as usize],
    ])
}

fn rcon(index: usize) -> u8 {
    let mut value = 1u8;
    for _ in 1..index {
        value = gf_mul(value, 2);
    }
    value
}

fn sub_bytes(state: &mut [u8; 16], box_table: &[u8; 256]) {
    for byte in state.iter_mut() {
        *byte = box_table[*byte as usize];
    }
}

/// State layout is column-major: `state[r + 4*c]`. Row `r` rotates left by `r`.
fn shift_rows(state: &mut [u8; 16]) {
    let s = *state;
    state[1] = s[5];
    state[5] = s[9];
    state[9] = s[13];
    state[13] = s[1];
    state[2] = s[10];
    state[6] = s[14];
    state[10] = s[2];
    state[14] = s[6];
    state[3] = s[15];
    state[7] = s[3];
    state[11] = s[7];
    state[15] = s[11];
}

fn inv_shift_rows(state: &mut [u8; 16]) {
    let s = *state;
    state[1] = s[13];
    state[5] = s[1];
    state[9] = s[5];
    state[13] = s[9];
    state[2] = s[10];
    state[6] = s[14];
    state[10] = s[2];
    state[14] = s[6];
    state[3] = s[7];
    state[7] = s[11];
    state[11] = s[15];
    state[15] = s[3];
}

fn mix_columns(state: &mut [u8; 16]) {
    for column in 0..4 {
        let i = column * 4;
        let (a0, a1, a2, a3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
        state[i] = gf_mul(a0, 2) ^ gf_mul(a1, 3) ^ a2 ^ a3;
        state[i + 1] = a0 ^ gf_mul(a1, 2) ^ gf_mul(a2, 3) ^ a3;
        state[i + 2] = a0 ^ a1 ^ gf_mul(a2, 2) ^ gf_mul(a3, 3);
        state[i + 3] = gf_mul(a0, 3) ^ a1 ^ a2 ^ gf_mul(a3, 2);
    }
}

fn inv_mix_columns(state: &mut [u8; 16]) {
    for column in 0..4 {
        let i = column * 4;
        let (a0, a1, a2, a3) = (state[i], state[i + 1], state[i + 2], state[i + 3]);
        state[i] = gf_mul(a0, 14) ^ gf_mul(a1, 11) ^ gf_mul(a2, 13) ^ gf_mul(a3, 9);
        state[i + 1] = gf_mul(a0, 9) ^ gf_mul(a1, 14) ^ gf_mul(a2, 11) ^ gf_mul(a3, 13);
        state[i + 2] = gf_mul(a0, 13) ^ gf_mul(a1, 9) ^ gf_mul(a2, 14) ^ gf_mul(a3, 11);
        state[i + 3] = gf_mul(a0, 11) ^ gf_mul(a1, 13) ^ gf_mul(a2, 9) ^ gf_mul(a3, 14);
    }
}

fn add_round_key(state: &mut [u8; 16], keys: &[u32], round: usize) {
    for column in 0..4 {
        let word = keys[round * 4 + column].to_be_bytes();
        for byte in 0..4 {
            state[column * 4 + byte] ^= word[byte];
        }
    }
}

/// Encrypt one block in ECB mode (equivalently CBC with a zero IV).
pub fn encrypt_ecb_block(key: &[u8], block: &[u8; 16]) -> Result<[u8; 16]> {
    Ok(Aes::new(key)?.encrypt_block(block))
}

/// Decrypt one block in ECB mode (equivalently CBC with a zero IV).
pub fn decrypt_ecb_block(key: &[u8], block: &[u8; 16]) -> Result<[u8; 16]> {
    Ok(Aes::new(key)?.decrypt_block(block))
}

/// Encrypt `data` in CBC mode without adding padding. `data` must be a multiple
/// of 16 bytes.
pub fn encrypt_cbc_no_padding(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(16) {
        return Err(SecurityError::InvalidKeyLength(data.len() as u16));
    }
    let cipher = Aes::new(key)?;
    let mut previous = *iv;
    let mut output = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<16>().0 {
        let mut block = [0u8; 16];
        for i in 0..16 {
            block[i] = chunk[i] ^ previous[i];
        }
        previous = cipher.encrypt_block(&block);
        output.extend_from_slice(&previous);
    }
    Ok(output)
}

/// Decrypt `data` in CBC mode without removing padding. `data` must be a
/// multiple of 16 bytes.
pub fn decrypt_cbc_no_padding(key: &[u8], iv: &[u8; 16], data: &[u8]) -> Result<Vec<u8>> {
    if !data.len().is_multiple_of(16) {
        return Err(SecurityError::InvalidKeyLength(data.len() as u16));
    }
    let cipher = Aes::new(key)?;
    let mut previous = *iv;
    let mut output = Vec::with_capacity(data.len());
    for chunk in data.as_chunks::<16>().0 {
        let mut block = [0u8; 16];
        block.copy_from_slice(chunk);
        let plaintext = cipher.decrypt_block(&block);
        for i in 0..16 {
            output.push(plaintext[i] ^ previous[i]);
        }
        previous = block;
    }
    Ok(output)
}

/// Add PKCS#7 padding: between 1 and 16 bytes, each equal to the pad length.
/// When the input is already a multiple of 16, a full padding block is added.
fn pkcs7_pad(data: &[u8]) -> Vec<u8> {
    let pad = 16 - (data.len() % 16);
    let mut padded = Vec::with_capacity(data.len() + pad);
    padded.extend_from_slice(data);
    padded.resize(data.len() + pad, pad as u8);
    padded
}

/// Validate and remove PKCS#7 padding.
fn pkcs7_unpad(data: &[u8]) -> Result<&[u8]> {
    if data.is_empty() || !data.len().is_multiple_of(16) {
        return Err(SecurityError::DecryptionFailed);
    }
    let pad = data[data.len() - 1] as usize;
    if pad == 0 || pad > 16 || pad > data.len() {
        return Err(SecurityError::DecryptionFailed);
    }
    let mut difference = 0u8;
    for byte in &data[data.len() - pad..] {
        difference |= *byte ^ (pad as u8);
    }
    if difference != 0 {
        return Err(SecurityError::DecryptionFailed);
    }
    Ok(&data[..data.len() - pad])
}

/// Encrypt `plaintext` in CBC mode with PKCS#7 padding.
pub fn encrypt_cbc(key: &[u8], iv: &[u8; 16], plaintext: &[u8]) -> Result<Vec<u8>> {
    let padded = pkcs7_pad(plaintext);
    encrypt_cbc_no_padding(key, iv, &padded)
}

/// Decrypt `ciphertext` in CBC mode and validate its PKCS#7 padding.
pub fn decrypt_cbc(key: &[u8], iv: &[u8; 16], ciphertext: &[u8]) -> Result<Vec<u8>> {
    let padded = decrypt_cbc_no_padding(key, iv, ciphertext)?;
    Ok(pkcs7_unpad(&padded)?.to_vec())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn aes_key_expansion_matches_fips_197_appendix_b() {
        // FIPS 197 Appendix B uses the key 2b7e1516...09cf4f3c.
        let key = hex::decode_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let aes = Aes::new(&key).expect("128-bit key");
        assert_eq!(aes.rounds(), 10);
        assert_eq!(aes.round_keys[0], 0x2b7e1516);
        assert_eq!(aes.round_keys[3], 0x09cf4f3c);
        assert_eq!(aes.round_keys[4], 0xa0fafe17);
        assert_eq!(aes.round_keys[5], 0x88542cb1);
        assert_eq!(aes.round_keys[6], 0x23a33939);
        assert_eq!(aes.round_keys[7], 0x2a6c7605);
    }

    #[test]
    fn aes_matches_fips_197_appendix_c_block_vectors() {
        let plaintext = [
            0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xaa, 0xbb, 0xcc, 0xdd,
            0xee, 0xff,
        ];
        let cases: [(&[u8], &str); 3] = [
            (
                &[
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f,
                ],
                "69c4e0d86a7b0430d8cdb78070b4c55a",
            ),
            (
                &[
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17,
                ],
                "dda97ca4864cdfe06eaf70a0ec0d7191",
            ),
            (
                &[
                    0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c,
                    0x0d, 0x0e, 0x0f, 0x10, 0x11, 0x12, 0x13, 0x14, 0x15, 0x16, 0x17, 0x18, 0x19,
                    0x1a, 0x1b, 0x1c, 0x1d, 0x1e, 0x1f,
                ],
                "8ea2b7ca516745bfeafc49904b496089",
            ),
        ];
        for (key, expected) in cases {
            let cipher = Aes::new(key).expect("valid AES key");
            let ciphertext = cipher.encrypt_block(&plaintext);
            assert_eq!(hex(&ciphertext), expected);
            assert_eq!(cipher.decrypt_block(&ciphertext), plaintext);
        }
    }

    #[test]
    fn aes_cbc_matches_nist_sp_800_38a_vectors() {
        let iv = [
            0x00, 0x01, 0x02, 0x03, 0x04, 0x05, 0x06, 0x07, 0x08, 0x09, 0x0a, 0x0b, 0x0c, 0x0d,
            0x0e, 0x0f,
        ];
        let plaintext = hex::decode_bytes(
            "6bc1bee22e409f96e93d7e117393172aae2d8a571e03ac9c9eb76fac45af8e51\
             30c81c46a35ce411e5fbc1191a0a52eff69f2445df4f9b17ad2b417be66c3710",
        );
        let key128 = hex::decode_bytes("2b7e151628aed2a6abf7158809cf4f3c");
        let expected128 = hex::decode_bytes(
            "7649abac8119b246cee98e9b12e9197d5086cb9b507219ee95db113a917678b2\
             73bed6b8e3c1743b7116e69e222295163ff1caa1681fac09120eca307586e1a7",
        );
        assert_eq!(
            encrypt_cbc_no_padding(&key128, &iv, &plaintext).expect("AES-128 CBC"),
            expected128
        );
        assert_eq!(
            decrypt_cbc_no_padding(&key128, &iv, &expected128).expect("AES-128 CBC decrypt"),
            plaintext
        );

        let key256 =
            hex::decode_bytes("603deb1015ca71be2b73aef0857d77811f352c073b6108d72d9810a30914dff4");
        let expected256 = hex::decode_bytes(
            "f58c4c04d6e5f1ba779eabfb5f7bfbd69cfc4e967edb808d679f777bc6702c7d\
             39f23369a9d9bacfa530e26304231461b2eb05e2c39be9fcda6c19078c6a9d1b",
        );
        assert_eq!(
            encrypt_cbc_no_padding(&key256, &iv, &plaintext).expect("AES-256 CBC"),
            expected256
        );
        assert_eq!(
            decrypt_cbc_no_padding(&key256, &iv, &expected256).expect("AES-256 CBC decrypt"),
            plaintext
        );
    }

    #[test]
    fn aes_cbc_pkcs7_round_trip_and_full_padding_block() {
        let key = [0x2bu8; 32];
        let iv = [0x7cu8; 16];
        for len in [0usize, 1, 15, 16, 17, 31, 32, 33] {
            let plaintext: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
            let ciphertext = encrypt_cbc(&key, &iv, &plaintext).expect("encrypt");
            assert_eq!(ciphertext.len() % 16, 0);
            assert!(ciphertext.len() >= 16);
            assert_eq!(
                decrypt_cbc(&key, &iv, &ciphertext).expect("decrypt"),
                plaintext
            );
        }
    }

    #[test]
    fn aes_cbc_rejects_tampered_padding_and_bad_iv() {
        let key = [0x11u8; 16];
        let iv = [0x22u8; 16];
        let ciphertext = encrypt_cbc(&key, &iv, b"integrity matters").expect("encrypt");
        let mut tampered = ciphertext.clone();
        tampered[15] ^= 0x01;
        assert_eq!(
            decrypt_cbc(&key, &iv, &tampered),
            Err(SecurityError::DecryptionFailed)
        );
        let wrong_iv = [0x33u8; 16];
        assert_ne!(
            decrypt_cbc(&key, &wrong_iv, &ciphertext).ok(),
            Some(b"integrity matters".to_vec())
        );
        assert_eq!(
            decrypt_cbc(&key, &iv, &ciphertext[..15]),
            Err(SecurityError::InvalidKeyLength(15))
        );
        assert_eq!(
            Aes::new(&[0u8; 12]).err(),
            Some(SecurityError::InvalidKeyLength(96))
        );
    }

    mod hex {
        pub fn decode_bytes(text: &str) -> Vec<u8> {
            let clean: String = text.chars().filter(|c| !c.is_whitespace()).collect();
            assert_eq!(clean.len() % 2, 0);
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
    }
}
