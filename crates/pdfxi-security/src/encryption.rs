//! Encryption method identifiers and metadata.
//!
//! This module models the encryption *methods* defined by the PDF
//! specification. It deliberately does not implement the block ciphers: the
//! algorithms are identified, key lengths are validated, and stable string IDs
//! round-trip through serialized forms. Concrete cipher handlers and a test
//! double live behind the [`crate::Protector`] trait.

use std::fmt;

use crate::error::{Result, SecurityError};

/// An encryption method used to protect a PDF document.
///
/// The variants cover the legacy RC4 handlers and the modern AES handlers used
/// by PDF 1.6+ (AES-128) and PDF 2.0 (AES-256).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EncryptionMethod {
    /// Content is stored in plaintext; no security handler is active.
    None,
    /// RC4 with a 40-bit key (legacy, security revision 2).
    Rc4_40,
    /// RC4 with a 128-bit key (security revision 3).
    Rc4_128,
    /// AES with a 128-bit key (AESV2 / security revision 4).
    Aes128,
    /// AES with a 256-bit key (AESV3 / security revisions 5 and 6).
    Aes256,
}

impl EncryptionMethod {
    /// Every method in a stable order, useful for exhaustive round-trip tests.
    pub const ALL: [Self; 5] = [
        Self::None,
        Self::Rc4_40,
        Self::Rc4_128,
        Self::Aes128,
        Self::Aes256,
    ];

    /// Nominal key length in bits, or `0` for [`Self::None`].
    pub const fn key_length_bits(self) -> u16 {
        match self {
            Self::None => 0,
            Self::Rc4_40 => 40,
            Self::Rc4_128 | Self::Aes128 => 128,
            Self::Aes256 => 256,
        }
    }

    /// Returns `true` when this method actually encrypts content.
    pub const fn is_encrypted(self) -> bool {
        !matches!(self, Self::None)
    }

    /// Stable string identifier suitable for persisting in metadata.
    pub const fn as_id(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Rc4_40 => "rc4-40",
            Self::Rc4_128 => "rc4-128",
            Self::Aes128 => "aes-128",
            Self::Aes256 => "aes-256",
        }
    }

    /// Parse an identifier produced by [`Self::as_id`].
    ///
    /// Returns `None` for any unrecognized or malformed identifier.
    pub fn from_id(id: &str) -> Option<Self> {
        match id {
            "none" => Some(Self::None),
            "rc4-40" => Some(Self::Rc4_40),
            "rc4-128" => Some(Self::Rc4_128),
            "aes-128" => Some(Self::Aes128),
            "aes-256" => Some(Self::Aes256),
            _ => None,
        }
    }

    /// Cipher family label: `"None"`, `"RC4"`, or `"AES"`.
    pub const fn family(self) -> &'static str {
        match self {
            Self::None => "None",
            Self::Rc4_40 | Self::Rc4_128 => "RC4",
            Self::Aes128 | Self::Aes256 => "AES",
        }
    }

    /// Verify that `bits` is the key length required by this method.
    pub fn accepts_key_length(self, bits: u16) -> Result<()> {
        if bits == self.key_length_bits() {
            Ok(())
        } else {
            Err(SecurityError::InvalidKeyLength(bits))
        }
    }
}

impl fmt::Display for EncryptionMethod {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.as_id())
    }
}
