//! PDF standard security handler: password authentication, key derivation, and
//! per-object encryption for revisions 2 through 6.
//!
//! The algorithms are taken from ISO 32000-1:2008 §7.6.3–7.6.4 (revisions 2–4)
//! and ISO 32000-2:2017 §7.6.4 (revisions 5–6, including Algorithm 2.B, the
//! hardened hash). No document contents or secrets are logged; all password
//! comparisons use [`crate::crypto::constant_time_eq`].
//!
//! # Workflow
//!
//! 1. Build [`SecurityParameters`] from the `/Encrypt` dictionary and the
//!    document ID.
//! 2. Construct a [`StandardSecurityHandler`] and call
//!    [`StandardSecurityHandler::authenticate`]. It tries the user password
//!    first and then the owner password, returning a [`Decryptor`] that carries
//!    the file key and the effective [`Permissions`].
//! 3. Decrypt (or encrypt) individual streams with
//!    [`Decryptor::decrypt_object`] / [`Decryptor::encrypt_object`].
//!
//! The free functions at the bottom of this module compute the `/O`, `/U`,
//! `/UE`, `/OE`, and `/Perms` dictionary values so that test fixtures (and a
//! future writer) can produce self-consistent parameters.

use crate::crypto::{aes, constant_time_eq, md5, rc4, sha};
use crate::encryption::EncryptionMethod;
use crate::error::{Result, SecurityError};
use crate::permissions::{PasswordRole, Permission, Permissions};
use crate::protector::Protector;

/// The 32-byte password-padding string defined by the PDF specification.
pub const PASSWORD_PADDING: [u8; 32] = [
    0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01, 0x08,
    0x2e, 0x2e, 0x00, 0xb6, 0xd0, 0x68, 0x3e, 0x80, 0x2f, 0x0c, 0xa9, 0xfe, 0x64, 0x53, 0x69, 0x7a,
];

/// The 4-byte "salt" appended when deriving an AES object key.
const AES_SALT: [u8; 4] = [0x73, 0x41, 0x6c, 0x54]; // "sAlT"
/// The all-zero initialization vector used to wrap the AES-256 file key.
const ZERO_IV: [u8; 16] = [0u8; 16];
/// Maximum iterations of Algorithm 2.B; a defensive bound against hostile
/// inputs (well-chosen passwords converge in roughly 64–100 rounds).
const HASH_ROUND_CAP: i32 = 4096;

/// Pad or truncate a password to exactly 32 bytes for revisions 2–4.
pub fn pad_password(password: &[u8]) -> [u8; 32] {
    let mut padded = [0u8; 32];
    let take = password.len().min(32);
    padded[..take].copy_from_slice(&password[..take]);
    padded[take..].copy_from_slice(&PASSWORD_PADDING[..32 - take]);
    padded
}

/// Truncate a password to 127 bytes as required by ISO 32000-2 §7.6.4.3.2.
fn trunc_127(password: &[u8]) -> &[u8] {
    &password[..password.len().min(127)]
}

/// The values from an `/Encrypt` dictionary needed by the standard handler.
///
/// All byte fields are the raw decoded strings from the dictionary; this crate
/// does not parse PDF syntax, so callers supply them directly. No field is ever
/// included in a log message or error value.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SecurityParameters {
    /// Security handler revision (`/R`): 2, 3, 4, 5, or 6.
    pub revision: u8,
    /// Encryption version (`/V`): 1, 2, 4, or 5.
    pub version: u8,
    /// Encryption key length in bits (`/Length`).
    pub key_length_bits: u16,
    /// The `/O` entry (32 bytes for R2–R4, 48 bytes for R5–R6).
    pub o: Vec<u8>,
    /// The `/U` entry (32 bytes for R2–R4, 48 bytes for R5–R6).
    pub u: Vec<u8>,
    /// The permissions mask (`/P`) as an unsigned 32-bit bit pattern.
    pub p: u32,
    /// First element of the trailer `/ID` array.
    pub id_first: Vec<u8>,
    /// Value of `/EncryptMetadata` (defaults to `true` when absent).
    pub encrypt_metadata: bool,
    /// The `/UE` entry (R5–R6 only; otherwise empty).
    pub ue: Vec<u8>,
    /// The `/OE` entry (R5–R6 only; otherwise empty).
    pub oe: Vec<u8>,
    /// The `/Perms` entry (R5–R6 only; otherwise empty).
    pub perms: Vec<u8>,
    /// For V4, whether the stream crypt filter selects AESV2 rather than V2
    /// (RC4). Ignored for V5, which always uses AESV3.
    pub aes_streams: bool,
}

impl SecurityParameters {
    /// Return the effective encryption method implied by `/V` and the crypt
    /// filter selection.
    pub fn method(&self) -> Result<EncryptionMethod> {
        match self.version {
            1 => Ok(EncryptionMethod::Rc4_40),
            2 => Ok(EncryptionMethod::Rc4_128),
            4 => Ok(if self.aes_streams {
                EncryptionMethod::Aes128
            } else {
                EncryptionMethod::Rc4_128
            }),
            5 => Ok(EncryptionMethod::Aes256),
            other => Err(SecurityError::UnsupportedMethod(format!("V{other}"))),
        }
    }

    fn validate(&self) -> Result<()> {
        let expected_lengths = match self.revision {
            2 => (32usize, 32usize),
            3 | 4 => (32, 32),
            5 | 6 => (48, 48),
            other => return Err(SecurityError::UnsupportedMethod(format!("R{other}"))),
        };
        if self.o.len() != expected_lengths.0 || self.u.len() != expected_lengths.1 {
            return Err(SecurityError::Configuration(
                "O/U entries have the wrong length for the revision".into(),
            ));
        }
        if self.revision >= 5 {
            if self.ue.len() != 32 || self.oe.len() != 32 || self.perms.len() != 16 {
                return Err(SecurityError::Configuration(
                    "UE/OE/Perms entries have the wrong length".into(),
                ));
            }
            if self.key_length_bits != 256 || self.version != 5 {
                return Err(SecurityError::InvalidKeyLength(self.key_length_bits));
            }
        } else {
            if self.version > 4 {
                return Err(SecurityError::UnsupportedMethod(format!(
                    "V{}",
                    self.version
                )));
            }
            let bits = self.key_length_bits;
            if !(40..=128).contains(&bits) || !bits.is_multiple_of(8) {
                return Err(SecurityError::InvalidKeyLength(bits));
            }
            if self.revision == 2 && bits != 40 {
                return Err(SecurityError::InvalidKeyLength(bits));
            }
        }
        if self.id_first.is_empty() {
            return Err(SecurityError::Configuration(
                "the document ID is required for key derivation".into(),
            ));
        }
        let _ = self.method()?;
        Ok(())
    }

    /// Effective permissions for the user (non-owner) role.
    pub fn user_permissions(&self) -> Permissions {
        Permissions::new(Permission::from_bits_truncated(self.p), PasswordRole::User)
    }
}

/// Compute the file encryption key from a user password (Algorithm 2 for
/// revisions 2–4).
pub fn compute_file_key(user_password: &[u8], params: &SecurityParameters) -> Result<Vec<u8>> {
    let length = (params.key_length_bits / 8) as usize;
    let padded = pad_password(user_password);
    let mut hasher = md5::Md5::new();
    hasher.update(&padded);
    hasher.update(&params.o);
    hasher.update(&params.p.to_le_bytes());
    hasher.update(&params.id_first);
    if params.revision >= 4 && !params.encrypt_metadata {
        hasher.update(&[0xff; 4]);
    }
    let mut digest = hasher.finalize();
    if params.revision >= 3 {
        for _ in 0..50 {
            digest = md5::md5(&digest[..length]);
        }
    }
    Ok(digest[..length].to_vec())
}

/// Compute the `/O` entry from the owner and user passwords (Algorithm 3).
pub fn compute_o_value(
    owner_password: &[u8],
    user_password: &[u8],
    revision: u8,
    key_length_bits: u16,
) -> Result<Vec<u8>> {
    let length = (key_length_bits / 8) as usize;
    let padded_owner = pad_password(owner_password);
    let mut digest = md5::md5(&padded_owner);
    if revision >= 3 {
        for _ in 0..50 {
            digest = md5::md5(&digest);
        }
    }
    let key = &digest[..length];
    let mut o = rc4::rc4(key, &pad_password(user_password));
    if revision >= 3 {
        for round in 1u8..=19 {
            let round_key: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            o = rc4::rc4(&round_key, &o);
        }
    }
    Ok(o)
}

/// Compute the `/U` entry from a file encryption key (Algorithm 4 for R2,
/// Algorithm 5 for R3–R4).
pub fn compute_u_value(file_key: &[u8], params: &SecurityParameters) -> Result<Vec<u8>> {
    if params.revision <= 2 {
        return Ok(rc4::rc4(file_key, &PASSWORD_PADDING));
    }
    let mut hasher = md5::Md5::new();
    hasher.update(&PASSWORD_PADDING);
    hasher.update(&params.id_first);
    let digest = hasher.finalize();
    let mut u = rc4::rc4(file_key, &digest);
    for round in 1u8..=19 {
        let round_key: Vec<u8> = file_key.iter().map(|byte| byte ^ round).collect();
        u = rc4::rc4(&round_key, &u);
    }
    let mut full = [0u8; 32];
    full[..16].copy_from_slice(&u);
    // The trailing 16 bytes are arbitrary; the PDF specification only compares
    // the first 16 for revisions 3 and 4.
    full[16..].copy_from_slice(&PASSWORD_PADDING[..16]);
    Ok(full.to_vec())
}

/// Recover the padded user password from an owner password and the `/O` entry
/// (Algorithm 7, used for owner-password authentication).
fn recover_user_password(
    owner_password: &[u8],
    o_entry: &[u8],
    revision: u8,
    key_length_bits: u16,
) -> Result<Vec<u8>> {
    let length = (key_length_bits / 8) as usize;
    let padded_owner = pad_password(owner_password);
    let mut digest = md5::md5(&padded_owner);
    if revision >= 3 {
        for _ in 0..50 {
            digest = md5::md5(&digest);
        }
    }
    let key = &digest[..length];
    let mut user = o_entry.to_vec();
    if revision <= 2 {
        user = rc4::rc4(key, &user);
    } else {
        for round in (0u8..=19).rev() {
            let round_key: Vec<u8> = key.iter().map(|byte| byte ^ round).collect();
            user = rc4::rc4(&round_key, &user);
        }
    }
    Ok(user)
}

/// The iterated hardened hash of ISO 32000-2 Algorithm 2.B (revision 6).
fn algorithm_2b(seed: &[u8], password: &[u8], aux: &[u8]) -> Result<Vec<u8>> {
    let mut k = seed.to_vec();
    let mut count = 0i32;
    loop {
        // K1 = (password || K || aux) repeated 64 times. Its length is always
        // a multiple of 16 because it is multiplied by 64, so no CBC padding is
        // needed.
        let mut k1 = Vec::with_capacity((password.len() + k.len() + aux.len()) * 64);
        for _ in 0..64 {
            k1.extend_from_slice(password);
            k1.extend_from_slice(&k);
            k1.extend_from_slice(aux);
        }
        let mut key = [0u8; 16];
        let mut iv = [0u8; 16];
        key.copy_from_slice(&k[..16]);
        iv.copy_from_slice(&k[16..32]);
        let e = aes::encrypt_cbc_no_padding(&key, &iv, &k1)?;
        let selector: u32 = e[..16].iter().map(|byte| *byte as u32).sum::<u32>() % 3;
        k = match selector {
            0 => sha::sha256(&e).to_vec(),
            1 => sha::sha384(&e),
            _ => sha::sha512(&e),
        };
        count += 1;
        let last = e[e.len() - 1] as i32;
        if count >= 64 && last <= count - 32 {
            break;
        }
        if count >= HASH_ROUND_CAP {
            return Err(SecurityError::Configuration(
                "revision 6 hash did not converge".into(),
            ));
        }
    }
    Ok(k[..32].to_vec())
}

/// Password hash used by revisions 5 and 6 (Algorithm 2.A / 2.B).
fn hash_r5_r6(revision: u8, password: &[u8], salt: &[u8], aux: &[u8]) -> Result<Vec<u8>> {
    let mut hasher = sha::Sha256::new();
    hasher.update(password);
    hasher.update(salt);
    hasher.update(aux);
    let seed = hasher.finalize();
    if revision < 6 {
        Ok(seed.to_vec())
    } else {
        algorithm_2b(&seed, password, aux)
    }
}

fn unwrap_aes256_file_key(intermediate_key: &[u8], wrapped: &[u8]) -> Result<Vec<u8>> {
    if intermediate_key.len() != 32 || wrapped.len() != 32 {
        return Err(SecurityError::Configuration(
            "AES-256 key unwrap requires 32-byte inputs".into(),
        ));
    }
    aes::decrypt_cbc_no_padding(intermediate_key, &ZERO_IV, wrapped)
}

fn verify_perms(file_key: &[u8], params: &SecurityParameters) -> Result<()> {
    if params.perms.len() != 16 || file_key.len() != 32 {
        return Err(SecurityError::Configuration(
            "Perms validation requires a 32-byte key and 16-byte block".into(),
        ));
    }
    let mut block = [0u8; 16];
    block.copy_from_slice(&params.perms);
    let plain = aes::decrypt_ecb_block(file_key, &block)?;
    if &plain[9..12] != b"adb" {
        return Err(SecurityError::DecryptionFailed);
    }
    let stored_p = u32::from_le_bytes([plain[0], plain[1], plain[2], plain[3]]);
    if stored_p != params.p {
        return Err(SecurityError::DecryptionFailed);
    }
    let expected = if params.encrypt_metadata { b'T' } else { b'F' };
    if plain[8] != expected {
        return Err(SecurityError::DecryptionFailed);
    }
    Ok(())
}

/// Validate a password and produce a usable object decryptor.
///
/// The user password is tried first, then the owner password. Both checks use
/// constant-time comparisons, and a failed authentication returns
/// [`SecurityError::WrongPassword`] regardless of which entry failed.
#[derive(Clone, Debug)]
pub struct StandardSecurityHandler {
    params: SecurityParameters,
}

impl StandardSecurityHandler {
    /// Validate the dictionary values and create a handler.
    pub fn new(params: SecurityParameters) -> Result<Self> {
        params.validate()?;
        Ok(Self { params })
    }

    /// The validated parameters.
    pub fn params(&self) -> &SecurityParameters {
        &self.params
    }

    /// Effective encryption method.
    pub fn method(&self) -> EncryptionMethod {
        // Infallible after `new` validated the parameters.
        self.params.method().unwrap_or(EncryptionMethod::None)
    }

    /// Try the supplied password as a user password, then as an owner password.
    pub fn authenticate(&self, password: &[u8]) -> Result<Decryptor> {
        if self.params.revision >= 5 {
            self.authenticate_r5_r6(password)
        } else {
            self.authenticate_r2_r4(password)
        }
    }

    fn authenticate_r2_r4(&self, password: &[u8]) -> Result<Decryptor> {
        let user_file_key = self.try_user_password_r2_r4(password);
        if let Ok(file_key) = user_file_key {
            return Ok(self.decryptor(file_key, self.params.user_permissions()));
        }
        let owner_file_key = self.try_owner_password_r2_r4(password);
        if let Ok(file_key) = owner_file_key {
            let permissions = Permissions::new(Permission::ALL, PasswordRole::Owner);
            return Ok(self.decryptor(file_key, permissions));
        }
        Err(SecurityError::WrongPassword)
    }

    fn try_user_password_r2_r4(&self, password: &[u8]) -> Result<Vec<u8>> {
        let file_key = compute_file_key(password, &self.params)?;
        let expected_u = compute_u_value(&file_key, &self.params)?;
        let compared = if self.params.revision >= 3 { 16 } else { 32 };
        if !constant_time_eq(&expected_u[..compared], &self.params.u[..compared]) {
            return Err(SecurityError::WrongPassword);
        }
        Ok(file_key)
    }

    fn try_owner_password_r2_r4(&self, password: &[u8]) -> Result<Vec<u8>> {
        let recovered = recover_user_password(
            password,
            &self.params.o,
            self.params.revision,
            self.params.key_length_bits,
        )?;
        self.try_user_password_r2_r4(&recovered)
    }

    fn authenticate_r5_r6(&self, password: &[u8]) -> Result<Decryptor> {
        let pwd = trunc_127(password);
        if let Ok(file_key) = self.try_user_password_r5_r6(pwd) {
            return Ok(self.decryptor(file_key, self.params.user_permissions()));
        }
        if let Ok(file_key) = self.try_owner_password_r5_r6(pwd) {
            let permissions = Permissions::new(Permission::ALL, PasswordRole::Owner);
            return Ok(self.decryptor(file_key, permissions));
        }
        Err(SecurityError::WrongPassword)
    }

    fn try_user_password_r5_r6(&self, password: &[u8]) -> Result<Vec<u8>> {
        let u = &self.params.u;
        let validation = hash_r5_r6(self.params.revision, password, &u[32..40], &[])?;
        if !constant_time_eq(&validation, &u[..32]) {
            return Err(SecurityError::WrongPassword);
        }
        let intermediate = hash_r5_r6(self.params.revision, password, &u[40..48], &[])?;
        let file_key = unwrap_aes256_file_key(&intermediate, &self.params.ue)?;
        verify_perms(&file_key, &self.params)?;
        Ok(file_key)
    }

    fn try_owner_password_r5_r6(&self, password: &[u8]) -> Result<Vec<u8>> {
        let o = &self.params.o;
        let validation = hash_r5_r6(self.params.revision, password, &o[32..40], &self.params.u)?;
        if !constant_time_eq(&validation, &o[..32]) {
            return Err(SecurityError::WrongPassword);
        }
        let intermediate = hash_r5_r6(self.params.revision, password, &o[40..48], &self.params.u)?;
        let file_key = unwrap_aes256_file_key(&intermediate, &self.params.oe)?;
        verify_perms(&file_key, &self.params)?;
        Ok(file_key)
    }

    fn decryptor(&self, file_key: Vec<u8>, permissions: Permissions) -> Decryptor {
        Decryptor {
            params: self.params.clone(),
            file_key,
            permissions,
        }
    }
}

/// An authenticated, object-aware cipher implementing [`Protector`].
///
/// The file key never leaves this value and is never exposed through a getter
/// or a debug representation.
#[derive(Clone)]
pub struct Decryptor {
    params: SecurityParameters,
    file_key: Vec<u8>,
    permissions: Permissions,
}

impl Decryptor {
    /// Effective encryption method.
    pub fn method(&self) -> EncryptionMethod {
        self.params.method().unwrap_or(EncryptionMethod::None)
    }

    /// Effective permissions of the authenticated session.
    pub fn permissions_of_session(&self) -> Permissions {
        self.permissions
    }

    /// Derive the per-object key and, for RC4, confirm that no IV is needed.
    fn object_key(&self, object_number: u32, generation: u16) -> Result<Vec<u8>> {
        let method = self.method();
        if method == EncryptionMethod::Aes256 {
            // AESV3 uses the file key directly for every object; the IV is
            // stored in the stream itself.
            return Ok(self.file_key.clone());
        }
        let mut input = Vec::with_capacity(self.file_key.len() + 9);
        input.extend_from_slice(&self.file_key);
        input.extend_from_slice(&object_number.to_le_bytes()[..3]);
        input.extend_from_slice(&generation.to_le_bytes()[..2]);
        if matches!(method, EncryptionMethod::Aes128) {
            input.extend_from_slice(&AES_SALT);
        }
        let digest = md5::md5(&input);
        let length = (self.file_key.len() + 5).min(16);
        Ok(digest[..length].to_vec())
    }

    /// Deterministic IV used when *encrypting* an AES stream. It is prepended
    /// to the ciphertext; readers use the stored bytes directly.
    fn aes_stream_iv(&self, object_number: u32, generation: u16) -> Result<[u8; 16]> {
        let mut input = Vec::with_capacity(self.file_key.len() + 9);
        input.extend_from_slice(&self.file_key);
        input.extend_from_slice(&object_number.to_le_bytes()[..3]);
        input.extend_from_slice(&generation.to_le_bytes()[..2]);
        input.extend_from_slice(&AES_SALT);
        let digest = md5::md5(&input);
        let mut iv = [0u8; 16];
        iv.copy_from_slice(&digest);
        Ok(iv)
    }

    /// Decrypt a stream or string belonging to the given object and generation.
    pub fn decrypt_object(
        &self,
        object_number: u32,
        generation: u16,
        ciphertext: &[u8],
    ) -> Result<Vec<u8>> {
        let key = self.object_key(object_number, generation)?;
        match self.method() {
            EncryptionMethod::None => Ok(ciphertext.to_vec()),
            EncryptionMethod::Rc4_40 | EncryptionMethod::Rc4_128 => Ok(rc4::rc4(&key, ciphertext)),
            EncryptionMethod::Aes128 | EncryptionMethod::Aes256 => {
                if ciphertext.len() < 32 || !ciphertext.len().is_multiple_of(16) {
                    return Err(SecurityError::DecryptionFailed);
                }
                let mut iv = [0u8; 16];
                iv.copy_from_slice(&ciphertext[..16]);
                aes::decrypt_cbc(&key, &iv, &ciphertext[16..])
            }
        }
    }

    /// Encrypt a stream or string belonging to the given object and generation.
    pub fn encrypt_object(
        &self,
        object_number: u32,
        generation: u16,
        plaintext: &[u8],
    ) -> Result<Vec<u8>> {
        let key = self.object_key(object_number, generation)?;
        match self.method() {
            EncryptionMethod::None => Ok(plaintext.to_vec()),
            EncryptionMethod::Rc4_40 | EncryptionMethod::Rc4_128 => Ok(rc4::rc4(&key, plaintext)),
            EncryptionMethod::Aes128 | EncryptionMethod::Aes256 => {
                let iv = self.aes_stream_iv(object_number, generation)?;
                let mut output = iv.to_vec();
                output.extend_from_slice(&aes::encrypt_cbc(&key, &iv, plaintext)?);
                Ok(output)
            }
        }
    }
}

impl Protector for Decryptor {
    fn method(&self) -> EncryptionMethod {
        self.method()
    }

    fn unlock(&self, password: &[u8]) -> Result<Permissions> {
        StandardSecurityHandler::new(self.params.clone())?
            .authenticate(password)
            .map(|session| session.permissions)
    }

    fn encrypt(&self, plaintext: &[u8]) -> Result<Vec<u8>> {
        self.encrypt_object(0, 0, plaintext)
    }

    fn decrypt(&self, ciphertext: &[u8]) -> Result<Vec<u8>> {
        self.decrypt_object(0, 0, ciphertext)
    }

    fn permissions(&self) -> Permissions {
        self.permissions
    }
}

impl std::fmt::Debug for Decryptor {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Decryptor")
            .field("method", &self.method())
            .field("revision", &self.params.revision)
            .field("permissions", &self.permissions)
            .field("file_key", &"<redacted>")
            .finish()
    }
}

// ---------------------------------------------------------------------------
// Writer-side helpers (used to build deterministic test fixtures)
// ---------------------------------------------------------------------------

/// Compute the `/U` and `/UE` entries for revisions 5 and 6.
pub fn compute_u_and_ue(
    revision: u8,
    user_password: &[u8],
    file_key: &[u8],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let password = trunc_127(user_password);
    let hash = hash_r5_r6(revision, password, validation_salt, &[])?;
    let mut u = [0u8; 48];
    u[..32].copy_from_slice(&hash);
    u[32..40].copy_from_slice(validation_salt);
    u[40..48].copy_from_slice(key_salt);
    let intermediate = hash_r5_r6(revision, password, key_salt, &[])?;
    let ue = aes::encrypt_cbc_no_padding(&intermediate, &ZERO_IV, file_key)?;
    let mut ue_array = [0u8; 32];
    ue_array.copy_from_slice(&ue);
    Ok((u, ue_array))
}

/// Compute the `/O` and `/OE` entries for revisions 5 and 6.
pub fn compute_o_and_oe(
    revision: u8,
    owner_password: &[u8],
    u_entry: &[u8; 48],
    file_key: &[u8],
    validation_salt: &[u8; 8],
    key_salt: &[u8; 8],
) -> Result<([u8; 48], [u8; 32])> {
    let password = trunc_127(owner_password);
    let hash = hash_r5_r6(revision, password, validation_salt, u_entry)?;
    let mut o = [0u8; 48];
    o[..32].copy_from_slice(&hash);
    o[32..40].copy_from_slice(validation_salt);
    o[40..48].copy_from_slice(key_salt);
    let intermediate = hash_r5_r6(revision, password, key_salt, u_entry)?;
    let oe = aes::encrypt_cbc_no_padding(&intermediate, &ZERO_IV, file_key)?;
    let mut oe_array = [0u8; 32];
    oe_array.copy_from_slice(&oe);
    Ok((o, oe_array))
}

/// Compute the `/Perms` entry for revisions 5 and 6.
pub fn compute_perms(
    file_key: &[u8],
    p: u32,
    encrypt_metadata: bool,
    random_padding: &[u8; 4],
) -> Result<[u8; 16]> {
    let mut block = [0u8; 16];
    block[..4].copy_from_slice(&p.to_le_bytes());
    block[4..8].copy_from_slice(&[0xff; 4]);
    block[8] = if encrypt_metadata { b'T' } else { b'F' };
    block[9..12].copy_from_slice(b"adb");
    block[12..16].copy_from_slice(random_padding);
    aes::encrypt_ecb_block(file_key, &block)
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: [u8; 16] = [
        0x28, 0xbf, 0x4e, 0x5e, 0x4e, 0x75, 0x8a, 0x41, 0x64, 0x00, 0x4e, 0x56, 0xff, 0xfa, 0x01,
        0x08,
    ];

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    fn r2_params() -> SecurityParameters {
        // A revision-2 (RC4-40) fixture built with this crate's writer helpers.
        let user = b"user";
        let owner = b"owner";
        let o = compute_o_value(owner, user, 2, 40).expect("O");
        let mut params = SecurityParameters {
            revision: 2,
            version: 1,
            key_length_bits: 40,
            o,
            u: vec![0u8; 32],
            p: 0xffff_ffd4,
            id_first: ID.to_vec(),
            encrypt_metadata: true,
            ue: Vec::new(),
            oe: Vec::new(),
            perms: Vec::new(),
            aes_streams: false,
        };
        let key = compute_file_key(user, &params).expect("file key");
        params.u = compute_u_value(&key, &params).expect("U");
        params
    }

    fn r4_params(aes: bool) -> SecurityParameters {
        let user = b"user-password";
        let owner = b"owner-password";
        let o = compute_o_value(owner, user, 4, 128).expect("O");
        let mut params = SecurityParameters {
            revision: 4,
            version: 4,
            key_length_bits: 128,
            o,
            u: vec![0u8; 32],
            p: 0xffff_f0c0,
            id_first: ID.to_vec(),
            encrypt_metadata: true,
            ue: Vec::new(),
            oe: Vec::new(),
            perms: Vec::new(),
            aes_streams: aes,
        };
        let key = compute_file_key(user, &params).expect("file key");
        params.u = compute_u_value(&key, &params).expect("U");
        params
    }

    fn r5_r6_params(revision: u8) -> SecurityParameters {
        let user = b"open-sesame";
        let owner = b"owner-secret";
        let file_key = [0x5Au8; 32];
        let u_salt = [0x11u8; 8];
        let u_key_salt = [0x22u8; 8];
        let o_salt = [0x33u8; 8];
        let o_key_salt = [0x44u8; 8];
        let (u, ue) =
            compute_u_and_ue(revision, user, &file_key, &u_salt, &u_key_salt).expect("U/UE");
        let (o, oe) =
            compute_o_and_oe(revision, owner, &u, &file_key, &o_salt, &o_key_salt).expect("O/OE");
        let perms = compute_perms(&file_key, 0xffff_f0d4, true, &[1, 2, 3, 4]).expect("Perms");
        SecurityParameters {
            revision,
            version: 5,
            key_length_bits: 256,
            o: o.to_vec(),
            u: u.to_vec(),
            p: 0xffff_f0d4,
            id_first: ID.to_vec(),
            encrypt_metadata: true,
            ue: ue.to_vec(),
            oe: oe.to_vec(),
            perms: perms.to_vec(),
            aes_streams: false,
        }
    }

    #[test]
    fn pad_password_matches_reference_padding() {
        assert_eq!(pad_password(b""), PASSWORD_PADDING);
        assert_eq!(pad_password(b"user")[..4], *b"user");
        assert_eq!(pad_password(b"a").len(), 32);
        let long = [0x41u8; 40];
        assert_eq!(&pad_password(&long)[..], &[0x41u8; 32]);
    }

    #[test]
    fn algorithm_2_key_derivation_matches_independent_dotnet_oracle() {
        // Fixed vectors computed independently with .NET MD5 and a separate RC4
        // implementation (Development oracle; no Rust code shared). Inputs:
        //   ID = 28bf4e5e4e758a4164004e56fffa0108, P = -44 / -1047488,
        //   user/owner passwords "user"/"owner" (R2) and
        //   "user-password"/"owner-password" (R4), Length 40 / 128.
        let r2 = r2_params();
        assert_eq!(
            hex(&r2.o),
            "94e8094419662a774442fb072e3d9f19e9d130ec09a4d0061e78fe920f7ab62f"
        );
        assert_eq!(
            hex(&compute_file_key(b"user", &r2).expect("R2 key")),
            "66485b968e"
        );
        assert_eq!(
            hex(&r2.u),
            "239dee5d22b65b885dd5f38d9e595bf22dce31e39b3f9720f7f12358825d0a6a"
        );

        let r4 = r4_params(false);
        assert_eq!(
            hex(&r4.o),
            "fd4be2f02ab6c33952a8640ec1efadf4ecf7138582d141232f407cbcbc8fd303"
        );
        assert_eq!(
            hex(&compute_file_key(b"user-password", &r4).expect("R4 key")),
            "788e223f580f3fd94f94c965e05c464f"
        );
        assert_eq!(
            hex(&r4.u),
            "565640f8852ff888271d1484c96eda6c28bf4e5e4e758a4164004e56fffa0108"
        );
    }

    #[test]
    fn revision_2_user_and_owner_passwords_authenticate() {
        let params = r2_params();
        let handler = StandardSecurityHandler::new(params).expect("handler");
        assert_eq!(handler.method(), EncryptionMethod::Rc4_40);
        let user_session = handler.authenticate(b"user").expect("user password");
        assert_eq!(user_session.permissions().role(), PasswordRole::User);
        let owner_session = handler.authenticate(b"owner").expect("owner password");
        assert_eq!(owner_session.permissions().role(), PasswordRole::Owner);
        assert!(owner_session.permissions().allows(Permission::ASSEMBLE));
    }

    #[test]
    fn wrong_password_is_rejected_with_specific_error() {
        let handler = StandardSecurityHandler::new(r4_params(false)).expect("handler");
        for candidate in [
            b"".as_slice(),
            b"wrong",
            b"user-password ",
            b"USER-PASSWORD",
        ] {
            assert_eq!(
                handler.authenticate(candidate).err(),
                Some(SecurityError::WrongPassword)
            );
        }
    }

    #[test]
    fn revision_4_rc4_and_aes_round_trip_with_object_isolation() {
        for aes in [false, true] {
            let handler = StandardSecurityHandler::new(r4_params(aes)).expect("handler");
            let session = handler.authenticate(b"user-password").expect("auth");
            let expected = if aes {
                EncryptionMethod::Aes128
            } else {
                EncryptionMethod::Rc4_128
            };
            assert_eq!(session.method(), expected);
            let plaintext = b"PDFXI stream payload that is longer than one block";
            for (obj, generation) in [(1u32, 0u16), (2, 0), (1, 1)] {
                let ciphertext = session
                    .encrypt_object(obj, generation, plaintext)
                    .expect("encrypt");
                assert_eq!(
                    session
                        .decrypt_object(obj, generation, &ciphertext)
                        .expect("decrypt"),
                    plaintext.to_vec()
                );
            }
            if aes {
                // AES prepends a 16-byte IV.
                assert!(
                    session
                        .encrypt_object(1, 0, plaintext)
                        .expect("encrypt")
                        .len()
                        >= 32
                );
            }
        }
    }

    #[test]
    fn object_keys_differ_for_different_object_numbers_and_generations() {
        let handler = StandardSecurityHandler::new(r4_params(false)).expect("handler");
        let session = handler.authenticate(b"user-password").expect("auth");
        let k1 = session.object_key(1, 0).expect("key");
        let k2 = session.object_key(2, 0).expect("key");
        let k3 = session.object_key(1, 1).expect("key");
        assert_ne!(k1, k2);
        assert_ne!(k1, k3);
        assert_ne!(k2, k3);
    }

    #[test]
    fn revision_5_authenticates_user_and_owner_and_extracts_permissions() {
        let handler = StandardSecurityHandler::new(r5_r6_params(5)).expect("handler");
        let user = handler.authenticate(b"open-sesame").expect("user");
        assert_eq!(user.permissions().role(), PasswordRole::User);
        assert!(user.permissions().allows(Permission::PRINT));
        assert!(user.permissions().allows(Permission::COPY));
        assert!(!user.permissions().allows(Permission::ASSEMBLE));
        assert_eq!(
            user.permissions().bits(),
            Permission::from_bits_truncated(0xffff_f0d4)
        );
        let owner = handler.authenticate(b"owner-secret").expect("owner");
        assert_eq!(owner.permissions().role(), PasswordRole::Owner);
        assert_eq!(
            handler.authenticate(b"not-the-password").err(),
            Some(SecurityError::WrongPassword)
        );
    }

    #[test]
    fn revision_6_hardened_hash_authenticates_user_and_owner() {
        let handler = StandardSecurityHandler::new(r5_r6_params(6)).expect("handler");
        let user = handler.authenticate(b"open-sesame").expect("user");
        assert_eq!(user.method(), EncryptionMethod::Aes256);
        assert_eq!(user.permissions().role(), PasswordRole::User);
        let owner = handler.authenticate(b"owner-secret").expect("owner");
        assert_eq!(owner.permissions().role(), PasswordRole::Owner);
        assert_eq!(
            handler.authenticate(b"open-sesam").err(),
            Some(SecurityError::WrongPassword)
        );
    }

    #[test]
    fn revision_6_hash_is_deterministic_and_rejects_tampered_perms() {
        let seed = sha::sha256(b"seed");
        let a = algorithm_2b(&seed, b"password", b"aux").expect("hash");
        let b = algorithm_2b(&seed, b"password", b"aux").expect("hash");
        assert_eq!(a, b);
        assert_eq!(a.len(), 32);
        let mut params = r5_r6_params(6);
        params.perms[15] ^= 0x01;
        assert!(
            StandardSecurityHandler::new(params)
                .expect("handler")
                .authenticate(b"open-sesame")
                .is_err()
        );
    }

    #[test]
    fn protector_trait_is_implemented_by_decryptor() {
        let handler = StandardSecurityHandler::new(r4_params(false)).expect("handler");
        let session = handler.authenticate(b"user-password").expect("auth");
        let protector: Box<dyn Protector> = Box::new(session);
        assert_eq!(protector.method(), EncryptionMethod::Rc4_128);
        let encrypted = protector.encrypt(b"trait round-trip").expect("encrypt");
        assert_eq!(
            protector.decrypt(&encrypted).expect("decrypt"),
            b"trait round-trip".to_vec()
        );
        assert_eq!(
            protector.unlock(b"bad").err(),
            Some(SecurityError::WrongPassword)
        );
    }

    #[test]
    fn debug_output_redacts_the_file_key() {
        let session = StandardSecurityHandler::new(r5_r6_params(5))
            .expect("handler")
            .authenticate(b"open-sesame")
            .expect("auth");
        let rendered = format!("{session:?}");
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains(&hex(&[0x5Au8; 32])));
    }

    #[test]
    fn malformed_dictionary_values_are_rejected() {
        let mut params = r2_params();
        params.revision = 7;
        assert!(StandardSecurityHandler::new(params).is_err());
        let mut params = r5_r6_params(5);
        params.ue.clear();
        assert!(StandardSecurityHandler::new(params).is_err());
    }
}
