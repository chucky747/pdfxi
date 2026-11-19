//! Security protector contract and a test double.
//!
//! A [`Protector`] owns the encryption method and permission policy for a
//! document. [`MockProtector`] is a deterministic in-memory implementation used
//! in tests and by other crates' test doubles. It is **not** real cryptography
//! and must never be used to protect real documents.

use std::fmt;

use crate::encryption::EncryptionMethod;
use crate::error::{Result, SecurityError};
use crate::permissions::{PasswordRole, Permission, Permissions};

/// A security handler that encrypts content and enforces a permission policy.
///
/// Implementations are expected to be cheaply shareable and to never expose or
/// log passwords or document contents. Password validation is performed by
/// [`Protector::unlock`], which returns the effective [`Permissions`] for the
/// matching role.
pub trait Protector {
    /// Encryption method used by this handler.
    fn method(&self) -> EncryptionMethod;

    /// Validate `password` and return the permissions for the matching role.
    ///
    /// Returns [`SecurityError::WrongPassword`] when the password matches
    /// neither the user nor the owner password.
    fn unlock(&self, password: &[u8]) -> Result<Permissions>;

    /// Encrypt plaintext bytes into ciphertext.
    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>>;

    /// Decrypt ciphertext bytes back into plaintext.
    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>>;

    /// Permissions associated with the user (non-owner) role of this handler.
    fn permissions(&self) -> Permissions;
}

/// Deterministic test double implementing [`Protector`].
///
/// The transform used by [`Protector::encrypt`] and [`Protector::decrypt`] is a
/// reversible byte shift, not a cipher. This exists solely so trait contracts,
/// permission roles, and wrong-password handling can be tested without a crypto
/// backend. Real handlers will be added in later milestones.
pub struct MockProtector {
    method: EncryptionMethod,
    user_password: Vec<u8>,
    owner_password: Vec<u8>,
    granted: Permission,
}

impl MockProtector {
    /// Create a mock protector.
    ///
    /// If both passwords are identical, the owner role wins. An empty user
    /// password is valid and models PDFs that open without a prompt.
    pub fn new(
        method: EncryptionMethod,
        user_password: &[u8],
        owner_password: &[u8],
        granted: Permission,
    ) -> Self {
        Self {
            method,
            user_password: user_password.to_vec(),
            owner_password: owner_password.to_vec(),
            granted,
        }
    }

    /// Override the permission bits granted to the user role.
    pub fn with_permissions(mut self, granted: Permission) -> Self {
        self.granted = granted;
        self
    }

    /// Length-independent equality that does not short-circuit on mismatch.
    fn bytes_equal(a: &[u8], b: &[u8]) -> bool {
        if a.len() != b.len() {
            return false;
        }
        let mut difference = 0u8;
        for (left, right) in a.iter().zip(b.iter()) {
            difference |= left ^ right;
        }
        difference == 0
    }

    /// Reversible byte shift used as a stand-in for a real cipher.
    fn transform(input: &[u8], delta: u8) -> Vec<u8> {
        input.iter().map(|byte| byte.wrapping_add(delta)).collect()
    }
}

impl Protector for MockProtector {
    fn method(&self) -> EncryptionMethod {
        self.method
    }

    fn unlock(&self, password: &[u8]) -> Result<Permissions> {
        if Self::bytes_equal(password, &self.owner_password) {
            return Ok(Permissions::new(Permission::ALL, PasswordRole::Owner));
        }
        if Self::bytes_equal(password, &self.user_password) {
            return Ok(Permissions::new(self.granted, PasswordRole::User));
        }
        Err(SecurityError::WrongPassword)
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        if self.method.is_encrypted() {
            Ok(Self::transform(plaintext, 0x5A))
        } else {
            Ok(plaintext.to_vec())
        }
    }

    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        if self.method.is_encrypted() {
            Ok(Self::transform(ciphertext, 0x5A_u8.wrapping_neg()))
        } else {
            Ok(ciphertext.to_vec())
        }
    }

    fn permissions(&self) -> Permissions {
        Permissions::new(self.granted, PasswordRole::User)
    }
}

impl fmt::Debug for MockProtector {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("MockProtector")
            .field("method", &self.method)
            .field("granted", &self.granted)
            .field("user_password", &"<redacted>")
            .field("owner_password", &"<redacted>")
            .finish()
    }
}
