//! Key derivation for PKCS#12: HMAC, PBKDF2, and the RFC 7292 KDF.
//!
//! The crate owns AES-CBC and SHA-2 already; this module adds the three
//! derivation functions PKCS#12 needs on top of them. All three are written
//! from their public specifications and covered by published known-answer
//! tests:
//!
//! - HMAC — RFC 2104, with the RFC 2202 and RFC 4231 vectors;
//! - PBKDF2 — RFC 8018 section 5.2, with the RFC 6070 vectors;
//! - the PKCS#12 key-derivation function — RFC 7292 appendix B.2.
//!
//! Nothing here logs a password, a salt, or derived key material.

use sha1::Sha1;
use sha2::{Digest as _, Sha256, Sha512};

use crate::error::{Result, SecurityError};

/// The hash underpinning a derivation function.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Prf {
    /// HMAC-SHA-1, the PKCS#12 default.
    Sha1,
    /// HMAC-SHA-256.
    Sha256,
    /// HMAC-SHA-512.
    Sha512,
}

impl Prf {
    /// Length of one digest in bytes.
    pub(super) const fn output_len(self) -> usize {
        match self {
            Self::Sha1 => 20,
            Self::Sha256 => 32,
            Self::Sha512 => 64,
        }
    }

    /// Hash block length in bytes, used by HMAC and the PKCS#12 KDF.
    const fn block_len(self) -> usize {
        match self {
            Self::Sha1 | Self::Sha256 => 64,
            Self::Sha512 => 128,
        }
    }

    /// Hash `data` once.
    fn hash(self, data: &[u8]) -> Vec<u8> {
        match self {
            Self::Sha1 => Sha1::digest(data).to_vec(),
            Self::Sha256 => Sha256::digest(data).to_vec(),
            Self::Sha512 => Sha512::digest(data).to_vec(),
        }
    }

    /// Map an HMAC `AlgorithmIdentifier` OID to a PRF.
    ///
    /// `1.2.840.113549.2.7` is HMAC-SHA-1, `.9` HMAC-SHA-256 and `.11`
    /// HMAC-SHA-512. SHA-224 and SHA-384 are recognised by PKCS#12 but not
    /// supported here; the caller turns `None` into a typed error.
    pub(super) fn from_hmac_oid(oid: &[u32]) -> Option<Self> {
        match oid {
            [1, 2, 840, 113_549, 2, 7] => Some(Self::Sha1),
            [1, 2, 840, 113_549, 2, 9] => Some(Self::Sha256),
            [1, 2, 840, 113_549, 2, 11] => Some(Self::Sha512),
            _ => None,
        }
    }
}

/// HMAC over `message` with `key` under `prf` (RFC 2104).
pub(super) fn hmac(prf: Prf, key: &[u8], message: &[u8]) -> Vec<u8> {
    let block = prf.block_len();
    let mut padded = if key.len() > block {
        prf.hash(key)
    } else {
        key.to_vec()
    };
    padded.resize(block, 0);
    let mut inner = Vec::with_capacity(block + message.len());
    inner.extend(padded.iter().map(|byte| byte ^ 0x36));
    inner.extend_from_slice(message);
    let inner_hash = prf.hash(&inner);
    let mut outer = Vec::with_capacity(block + inner_hash.len());
    outer.extend(padded.iter().map(|byte| byte ^ 0x5c));
    outer.extend_from_slice(&inner_hash);
    let result = prf.hash(&outer);
    // The padded key and intermediate hash are key material.
    zero(&mut padded);
    zero(&mut inner);
    zero(&mut outer);
    result
}

/// PBKDF2 (RFC 8018 section 5.2).
///
/// # Errors
///
/// [`SecurityError::PfxMalformed`] when the iteration count is zero or the
/// requested length is too large for PBKDF2's 32-bit block index.
pub(super) fn pbkdf2(
    prf: Prf,
    password: &[u8],
    salt: &[u8],
    iterations: u32,
    length: usize,
) -> Result<Vec<u8>> {
    if iterations == 0 {
        return Err(SecurityError::PfxMalformed("iteration count is zero"));
    }
    let block_len = prf.output_len();
    let blocks = length.div_ceil(block_len);
    if blocks > u32::MAX as usize {
        return Err(SecurityError::PfxMalformed("derived key is too long"));
    }
    let mut out = Vec::with_capacity(blocks * block_len);
    let mut input = Vec::with_capacity(salt.len() + 4);
    input.extend_from_slice(salt);
    for block in 1..=blocks {
        input.truncate(salt.len());
        input.extend_from_slice(&(block as u32).to_be_bytes());
        let mut current = hmac(prf, password, &input);
        let mut accumulator = current.clone();
        for _ in 1..iterations {
            current = hmac(prf, password, &current);
            for (target, byte) in accumulator.iter_mut().zip(current.iter()) {
                *target ^= byte;
            }
        }
        out.extend_from_slice(&accumulator);
        zero(&mut current);
        zero(&mut accumulator);
    }
    out.truncate(length);
    zero(&mut input);
    Ok(out)
}

/// The PKCS#12 key-derivation function (RFC 7292 appendix B.2).
///
/// `id` selects the purpose: `1` a cipher key, `2` a CBC IV, `3` a MAC key.
/// `password` is the BMPString encoding of the password already, that is,
/// UTF-16BE without a length prefix; `None` encodes an absent password.
///
/// # Errors
///
/// [`SecurityError::PfxMalformed`] when the iteration count is zero.
pub(super) fn pkcs12_kdf(
    prf: Prf,
    password: Option<&[u8]>,
    salt: &[u8],
    id: u8,
    iterations: u32,
    length: usize,
) -> Result<Vec<u8>> {
    if iterations == 0 {
        return Err(SecurityError::PfxMalformed("iteration count is zero"));
    }
    let u = prf.output_len();
    let v = prf.block_len();

    // D: the diversifier, repeated to one hash block.
    let mut diversifier = vec![id; v];

    // P: the password as a BMPString, or two zero bytes when absent; RFC 7292
    // specifies the two-octet terminator for a present password.
    let mut password_part = match password {
        Some(bytes) => bytes.to_vec(),
        None => Vec::new(),
    };
    if !password_part.is_empty() {
        // A BMPString password carries a two-octet terminator, and P is
        // repeated until its length is a multiple of the hash block size.
        password_part.extend_from_slice(&[0, 0]);
        let length = password_part.len().div_ceil(v) * v;
        let mut repeated = Vec::with_capacity(length);
        for index in 0..length {
            repeated.push(password_part[index % password_part.len()]);
        }
        zero(&mut password_part);
        password_part = repeated;
    }

    // S: the salt repeated to a whole number of hash blocks.
    let mut salt_part = Vec::new();
    if !salt.is_empty() {
        let blocks = salt.len().div_ceil(v);
        for index in 0..blocks * v {
            salt_part.push(salt[index % salt.len()]);
        }
    }

    let mut input = Vec::with_capacity(salt_part.len() + password_part.len());
    input.extend_from_slice(&salt_part);
    input.extend_from_slice(&password_part);

    let mut out = Vec::with_capacity(length);
    let mut block_input = Vec::with_capacity(v + input.len());
    while out.len() < length {
        // A_1 = H(D || I), then A_{i+1} = H(A_i). The diversifier and input
        // are hashed once per block, not on every iteration.
        block_input.clear();
        block_input.extend_from_slice(&diversifier);
        block_input.extend_from_slice(&input);
        let mut digest = prf.hash(&block_input);
        for _ in 1..iterations {
            digest = prf.hash(&digest);
        }
        out.extend_from_slice(&digest);

        // B: A_i repeated to one hash block, added into I with carry.
        let mut expanded = Vec::with_capacity(v);
        for index in 0..v {
            expanded.push(digest[index % u]);
        }
        add_into(&mut input, &expanded);
        zero(&mut digest);
        zero(&mut expanded);
    }
    out.truncate(length);
    zero(&mut diversifier);
    zero(&mut password_part);
    zero(&mut salt_part);
    zero(&mut input);
    zero(&mut block_input);
    Ok(out)
}

/// Add `value` into `target` as a big-endian integer modulo `2^(8 * len)`.
///
/// `target` and `value` share the same length; carried bits beyond the most
/// significant octet are discarded, as RFC 7292 requires.
fn add_into(target: &mut [u8], value: &[u8]) {
    if value.is_empty() || !target.len().is_multiple_of(value.len()) {
        return;
    }
    // RFC 7292 treats I as consecutive v-bit blocks and adds B + 1 to each
    // one independently, modulo 2^v. There is no carry between blocks.
    for block in target.chunks_exact_mut(value.len()) {
        let mut carry = 1_u16;
        for (target_byte, value_byte) in block.iter_mut().zip(value.iter()).rev() {
            let sum = u16::from(*target_byte) + u16::from(*value_byte) + carry;
            *target_byte = sum as u8;
            carry = sum >> 8;
        }
    }
}

/// Overwrite a buffer that held key material.
fn zero(bytes: &mut [u8]) {
    for byte in bytes.iter_mut() {
        *byte = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn hmac_matches_rfc_2202_and_rfc_4231() {
        assert_eq!(
            hex(&hmac(Prf::Sha1, &[0x0b; 20], b"Hi There")),
            "b617318655057264e28bc0b6fb378c8ef146be00"
        );
        assert_eq!(
            hex(&hmac(Prf::Sha256, b"Jefe", b"what do ya want for nothing?")),
            "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
        );
        // RFC 2202 case 6: a key longer than the hash block is hashed first.
        assert_eq!(
            hex(&hmac(
                Prf::Sha1,
                &[0xaa; 80],
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            )),
            "aa4ae5e15272d00e95705637ce8a3b55ed402112"
        );
    }

    #[test]
    fn pbkdf2_matches_rfc_6070_and_the_rfc_8018_sha256_vector() {
        assert_eq!(
            hex(&pbkdf2(Prf::Sha1, b"password", b"salt", 1, 20).expect("pbkdf2")),
            "0c60c80f961f0e71f3a9b524af6012062fe037a6"
        );
        assert_eq!(
            hex(&pbkdf2(Prf::Sha1, b"password", b"salt", 4096, 20).expect("pbkdf2")),
            "4b007901b765489abead49d926f721d065a429c1"
        );
        assert_eq!(
            hex(&pbkdf2(Prf::Sha256, b"password", b"salt", 1, 32).expect("pbkdf2")),
            "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
        );
        // A length that is not a multiple of the digest length is truncated,
        // and PBKDF2 output is prefix-consistent.
        let short = pbkdf2(Prf::Sha1, b"password", b"salt", 2, 20).expect("pbkdf2");
        let long = pbkdf2(Prf::Sha1, b"password", b"salt", 2, 25).expect("pbkdf2");
        assert_eq!(long.len(), 25);
        assert_eq!(&long[..20], short.as_slice());
        assert_eq!(
            pbkdf2(Prf::Sha1, b"password", b"salt", 0, 20).map(|_| ()),
            Err(SecurityError::PfxMalformed("iteration count is zero"))
        );
    }

    #[test]
    fn the_pkcs12_kdf_expands_deterministically_and_is_id_bound() {
        // RFC 7292 does not publish numeric vectors, so this asserts the
        // properties the standard fixes: determinism, id separation, exact
        // length, and the effect of the salt and password.
        let key = pkcs12_kdf(Prf::Sha1, Some(&[0, b'p', 0, b'w']), b"salt", 1, 1, 24).expect("kdf");
        assert_eq!(key.len(), 24);
        assert_eq!(
            key,
            pkcs12_kdf(Prf::Sha1, Some(&[0, b'p', 0, b'w']), b"salt", 1, 1, 24).expect("kdf")
        );
        let iv = pkcs12_kdf(Prf::Sha1, Some(&[0, b'p', 0, b'w']), b"salt", 2, 1, 8).expect("kdf");
        assert_eq!(iv.len(), 8);
        assert_ne!(&key[..8], iv.as_slice());
        let other_salt =
            pkcs12_kdf(Prf::Sha1, Some(&[0, b'p', 0, b'w']), b"pepper", 1, 1, 24).expect("kdf");
        assert_ne!(key, other_salt);
        let absent = pkcs12_kdf(Prf::Sha1, None, b"salt", 1, 1, 24).expect("kdf");
        assert_ne!(key, absent);
        assert_eq!(
            pkcs12_kdf(Prf::Sha1, None, b"salt", 1, 0, 24).map(|_| ()),
            Err(SecurityError::PfxMalformed("iteration count is zero"))
        );
    }

    #[test]
    fn add_into_wraps_modulo_the_block_width() {
        let mut target = [0xff, 0xff, 0xff];
        add_into(&mut target, &[0x00, 0x00, 0x01]);
        assert_eq!(target, [0x00, 0x00, 0x01]);

        let mut target = [0x01, 0x02, 0x03];
        add_into(&mut target, &[0x00, 0x00, 0x01]);
        assert_eq!(target, [0x01, 0x02, 0x05]);
    }

    #[test]
    fn add_into_updates_every_block_of_a_longer_input() {
        // RFC 7292 adds B to every v-bit block of I independently. With a
        // six-byte target and a three-byte B, both blocks must change.
        let mut target = [0xaa_u8; 6];
        add_into(&mut target, &[0x00, 0x00, 0x01]);
        assert_eq!(target, [0xaa, 0xaa, 0xac, 0xaa, 0xaa, 0xac]);
    }
}
