//! Dependency-free implementations of the ciphers and hashes used by the PDF
//! standard security handler.
//!
//! Everything in this module is written from the public specifications:
//!
//! - MD5 — RFC 1321.
//! - SHA-256 / SHA-384 / SHA-512 — FIPS 180-4.
//! - RC4 — the public RC4 keystream generator (KSA/PRGA).
//! - AES — FIPS 197, with CBC mode from NIST SP 800-38A and PKCS#7 padding.
//!
//! No item here logs its inputs. Keys, passwords, and plaintexts are ordinary
//! byte slices and are never written to diagnostics.
//!
//! The implementations intentionally avoid external crates so that the
//! `pdfxi-security` package stays dependency-free.

pub mod aes;
pub mod md5;
pub mod rc4;
pub mod sha;

/// Compare two byte strings for equality in time that depends only on their
/// length, not on the position of the first differing byte.
///
/// Password validation uses this helper so that a wrong guess does not reveal
/// how many leading bytes were correct. The lengths themselves are treated as
/// public information (they are dictated by the encryption dictionary).
pub fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut difference = 0u8;
    for (left, right) in a.iter().zip(b.iter()) {
        difference |= left ^ right;
    }
    difference == 0
}

#[cfg(test)]
mod tests {
    use super::constant_time_eq;

    #[test]
    fn constant_time_eq_handles_all_lengths_and_mismatches() {
        assert!(constant_time_eq(b"", b""));
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"abd", b"abc"));
        assert!(!constant_time_eq(b"abc", b"ab"));
        assert!(!constant_time_eq(b"ab", b"abc"));
    }
}
