//! RC4 keystream cipher.
//!
//! RC4 is a stream cipher used by the legacy PDF standard security handlers of
//! revisions 2–4. The implementation is the standard key-scheduling algorithm
//! (KSA) followed by the pseudo-random generation algorithm (PRGA); the
//! keystream is XORed with the data. Encryption and decryption are identical.

/// An RC4 keystream generator.
///
/// Create one with [`Rc4::new`] and XOR bytes with [`Rc4::apply`], or use the
/// one-shot [`rc4`] helper.
pub struct Rc4 {
    state: [u8; 256],
    i: u8,
    j: u8,
}

impl Rc4 {
    /// Initialize the cipher with `key` using the RC4 key-scheduling algorithm.
    pub fn new(key: &[u8]) -> Self {
        let mut state = [0u8; 256];
        for (index, slot) in state.iter_mut().enumerate() {
            *slot = index as u8;
        }
        let mut j = 0u8;
        for i in 0..256usize {
            j = j.wrapping_add(state[i]).wrapping_add(key[i % key.len()]);
            state.swap(i, j as usize);
        }
        Self { state, i: 0, j: 0 }
    }

    /// XOR every byte of `data` with the next bytes of the keystream.
    pub fn apply(&mut self, data: &mut [u8]) {
        for byte in data.iter_mut() {
            self.i = self.i.wrapping_add(1);
            self.j = self.j.wrapping_add(self.state[self.i as usize]);
            self.state.swap(self.i as usize, self.j as usize);
            let index =
                self.state[self.i as usize].wrapping_add(self.state[self.j as usize]) as usize;
            *byte ^= self.state[index];
        }
    }
}

/// Apply RC4 with `key` to `data` in one call.
///
/// Applying this function twice with the same key restores the original data.
pub fn rc4(key: &[u8], data: &[u8]) -> Vec<u8> {
    let mut output = data.to_vec();
    Rc4::new(key).apply(&mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::rc4;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02X}")).collect()
    }

    #[test]
    fn rc4_matches_published_test_vectors() {
        // Widely published RC4 vectors (Schneier / RFC-style test corpus).
        assert_eq!(hex(&rc4(b"Key", b"Plaintext")), "BBF316E8D940AF0AD3");
        assert_eq!(hex(&rc4(b"Wiki", b"pedia")), "1021BF0420");
        assert_eq!(
            hex(&rc4(b"Secret", b"Attack at dawn")),
            "45A01F645FC35B383552544B9BF5"
        );
    }

    #[test]
    fn rc4_is_involutive_for_40_and_128_bit_keys() {
        for len in [5usize, 16] {
            let key = vec![0x42u8; len];
            let plaintext = b"pdfxi rc4 round-trip";
            let ciphertext = rc4(&key, plaintext);
            assert_ne!(ciphertext.as_slice(), plaintext.as_slice());
            assert_eq!(rc4(&key, &ciphertext), plaintext.to_vec());
        }
    }
}
