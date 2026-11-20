//! Encrypting save path for the PDF standard security handler.
//!
//! This module turns a plaintext [`pdfxi_core::Document`] into a standalone PDF
//! whose strings and streams are encrypted, and reopens such a file with a
//! password. All cryptographic work is delegated to the existing
//! [`crate::standard_handler`] and [`crate::crypto`] primitives; this module only
//! builds the `/Encrypt` dictionary, walks the object graph, and serializes the
//! result. No new cipher or hash implementation is introduced here.
//!
//! # Scope
//!
//! Supported methods are RC4-40 (R2), RC4-128 (R3), AES-128 (R4) and AES-256
//! (R5/R6). Signatures, linearized files and cross-reference-stream files are
//! deliberately refused with [`SecurityError::UnsupportedDocumentStructure`].
//! For AES streams the deterministic per-object IV derived by
//! [`crate::Decryptor`] is used so that output is reproducible; writers that
//! need fresh random IVs can seed the encryption differently.
//!
//! Source documents are expected to use a classic cross-reference table with
//! one `N G obj` header per line (the layout emitted by this crate and by the
//! core writer). Xref-stream inputs are refused with a typed error.
//!
//! No password, key or document content is ever logged or included in an error.

use std::collections::BTreeMap;

use pdfxi_core::{Dictionary, Document, Object, ObjectId};

use crate::crypto::{md5, sha};
use crate::encryption::EncryptionMethod;
use crate::error::{Result, SecurityError};
use crate::permissions::{PasswordRole, Permission, Permissions};
use crate::standard_handler::{
    Decryptor, SecurityParameters, StandardSecurityHandler, compute_file_key, compute_o_and_oe,
    compute_o_value, compute_perms, compute_u_and_ue, compute_u_value,
};

/// Options for an encrypting save.
///
/// Passwords are held as raw bytes. The [`std::fmt::Debug`] implementation
/// redacts them so an accidental diagnostic print cannot leak a secret.
#[derive(Clone)]
pub struct EncryptOptions {
    /// The PDF-standard encryption method to apply.
    pub method: EncryptionMethod,
    /// The user (open) password, as raw bytes.
    pub user_password: Vec<u8>,
    /// The owner (permissions) password, as raw bytes.
    pub owner_password: Vec<u8>,
    /// Rights granted to the user role when it opens the document.
    pub permissions: Permission,
    /// Whether document metadata strings and streams are encrypted.
    pub encrypt_metadata: bool,
    /// Optional deterministic seed used to derive the document ID and keys.
    pub id_seed: Option<Vec<u8>>,
    /// Optional security-handler revision override (2, 3, 4, 5, or 6).
    pub revision: Option<u8>,
}

impl std::fmt::Debug for EncryptOptions {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EncryptOptions")
            .field("method", &self.method)
            .field("user_password", &"<redacted>")
            .field("owner_password", &"<redacted>")
            .field("permissions", &self.permissions)
            .field("encrypt_metadata", &self.encrypt_metadata)
            .field("id_seed", &self.id_seed.as_ref().map(|seed| seed.len()))
            .field("revision", &self.revision)
            .finish()
    }
}

impl EncryptOptions {
    /// Create options for `method` with both passwords and the user permissions.
    ///
    /// Returns [`SecurityError::UnsupportedMethod`] for
    /// [`EncryptionMethod::None`], because an encrypting save requires an actual
    /// security handler.
    pub fn new(
        method: EncryptionMethod,
        user_password: &[u8],
        owner_password: &[u8],
        permissions: Permission,
    ) -> Result<Self> {
        if !method.is_encrypted() {
            return Err(SecurityError::UnsupportedMethod("none".to_owned()));
        }
        method.accepts_key_length(method.key_length_bits())?;
        Ok(Self {
            method,
            user_password: user_password.to_vec(),
            owner_password: owner_password.to_vec(),
            permissions,
            encrypt_metadata: true,
            id_seed: None,
            revision: None,
        })
    }

    /// Supply a deterministic seed used when the source has no document ID.
    ///
    /// With the same seed and the same input, [`encrypt_document`] is
    /// byte-for-byte reproducible. Production callers that require fresh keys
    /// should pass a unique seed per document.
    pub fn with_id_seed(mut self, seed: Vec<u8>) -> Self {
        self.id_seed = Some(seed);
        self
    }

    /// Set the value written to `/EncryptMetadata`.
    pub fn with_encrypt_metadata(mut self, value: bool) -> Self {
        self.encrypt_metadata = value;
        self
    }

    /// Override the security-handler revision written to `/R`.
    ///
    /// RC4-40 only accepts R2, RC4-128 only R3, AES-128 only R4, and AES-256
    /// accepts R5 or R6 (the default is R6).
    pub fn with_revision(mut self, revision: u8) -> Result<Self> {
        let allowed: &[u8] = match self.method {
            EncryptionMethod::Rc4_40 => &[2],
            EncryptionMethod::Rc4_128 => &[3],
            EncryptionMethod::Aes128 => &[4],
            EncryptionMethod::Aes256 => &[5, 6],
            EncryptionMethod::None => &[],
        };
        if !allowed.contains(&revision) {
            return Err(SecurityError::Configuration(format!(
                "revision {revision} is not valid for {}",
                self.method.as_id()
            )));
        }
        self.revision = Some(revision);
        Ok(self)
    }
}

/// A typed, side-effect-free report describing an encrypting save.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncryptionReport {
    /// Encryption method applied to the output.
    pub method: EncryptionMethod,
    /// Security handler revision written to `/R`.
    pub revision: u8,
    /// Key length in bits written to `/Length`.
    pub key_length_bits: u16,
    /// Rights granted to the user role.
    pub permissions: Permission,
    /// Raw `/P` bit pattern written to the dictionary.
    pub raw_permissions: u32,
    /// Whether a distinct owner password was configured.
    pub owner_password_set: bool,
    /// First element of the trailer `/ID`, used for key derivation.
    pub id_first: [u8; 16],
    /// Number of indirect objects whose strings or streams were transformed.
    pub encrypted_objects: usize,
}

/// A typed report returned when an encrypted document is opened successfully.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DecryptionReport {
    /// Encryption method recovered from the `/Encrypt` dictionary.
    pub method: EncryptionMethod,
    /// Security handler revision recovered from `/R`.
    pub revision: u8,
    /// Role granted by the supplied password.
    pub role: PasswordRole,
    /// Effective permissions of the unlocked session.
    pub permissions: Permissions,
}

fn core_error(error: pdfxi_core::Error) -> SecurityError {
    SecurityError::EncryptedDocumentInvalid(error.to_string())
}

/// PDF reserves bits 7-8 (always 1) and bits 13-32 (always 1) of `/P`.
fn permissions_mask(permissions: Permission) -> u32 {
    0xFFFF_F0C0 | permissions.bits()
}

fn default_revision(method: EncryptionMethod) -> u8 {
    match method {
        EncryptionMethod::Rc4_40 => 2,
        EncryptionMethod::Rc4_128 => 3,
        EncryptionMethod::Aes128 => 4,
        EncryptionMethod::Aes256 => 6,
        EncryptionMethod::None => 0,
    }
}

fn derive_id(seed: &[u8]) -> [u8; 16] {
    let digest = sha::sha256(seed);
    let mut id = [0u8; 16];
    id.copy_from_slice(&digest[..16]);
    id
}

fn derive_second_id(first: &[u8; 16]) -> [u8; 16] {
    let mut seed = Vec::with_capacity(first.len() + 15);
    seed.extend_from_slice(first);
    seed.extend_from_slice(b"pdfxi-second-id");
    derive_id(&seed)
}

fn salt(id: &[u8; 16], tag: &[u8]) -> [u8; 8] {
    let mut seed = Vec::with_capacity(id.len() + tag.len());
    seed.extend_from_slice(id);
    seed.extend_from_slice(tag);
    let digest = md5::md5(&seed);
    let mut value = [0u8; 8];
    value.copy_from_slice(&digest[..8]);
    value
}

fn perms_padding(id: &[u8; 16], tag: &[u8]) -> [u8; 4] {
    let mut seed = Vec::with_capacity(id.len() + tag.len());
    seed.extend_from_slice(id);
    seed.extend_from_slice(tag);
    let digest = md5::md5(&seed);
    let mut value = [0u8; 4];
    value.copy_from_slice(&digest[..4]);
    value
}

/// Derive a deterministic 32-byte AES-256 file key from the ID and passwords.
fn derive_file_key(id: &[u8; 16], options: &EncryptOptions) -> Vec<u8> {
    let mut input = Vec::new();
    input.extend_from_slice(id);
    input.extend_from_slice(&(options.user_password.len() as u32).to_le_bytes());
    input.extend_from_slice(&options.user_password);
    input.extend_from_slice(&(options.owner_password.len() as u32).to_le_bytes());
    input.extend_from_slice(&options.owner_password);
    input.extend_from_slice(options.method.as_id().as_bytes());
    sha::sha256(&input).to_vec()
}

/// Locate the byte range of the newest trailer dictionary.
fn trailer_bytes(bytes: &[u8]) -> Option<&[u8]> {
    let startxref = bytes
        .windows(9)
        .rposition(|window| window == b"startxref")?;
    let before = &bytes[..startxref];
    let trailer = before.windows(7).rposition(|window| window == b"trailer")?;
    Some(&before[trailer..startxref])
}

/// Parse the first hexadecimal string at or after `from`.
fn hex_string_after(data: &[u8], from: usize) -> Option<(Vec<u8>, usize)> {
    let relative = data.get(from..)?.iter().position(|&byte| byte == b'<')?;
    let start = from + relative;
    let end = start
        + 1
        + data
            .get(start + 1..)?
            .iter()
            .position(|&byte| byte == b'>')?;
    let mut bytes = Vec::new();
    let mut high = None;
    for &byte in &data[start + 1..end] {
        if byte.is_ascii_whitespace() {
            continue;
        }
        let digit = u8::try_from((byte as char).to_digit(16)?).ok()?;
        if let Some(hi) = high.take() {
            bytes.push((hi << 4) | digit);
        } else {
            high = Some(digit);
        }
    }
    if let Some(hi) = high {
        bytes.push(hi << 4);
    }
    Some((bytes, end + 1))
}

/// Read the first `/ID` element from a trailer, normalizing it to 16 bytes.
fn trailer_id(bytes: &[u8]) -> Option<[u8; 16]> {
    let trailer = trailer_bytes(bytes)?;
    let id_position = trailer.windows(3).position(|window| window == b"/ID")?;
    let after = trailer.get(id_position + 3..)?;
    let bracket = after.iter().position(|&byte| byte == b'[')?;
    let (first, _) = hex_string_after(after, bracket + 1)?;
    if first.is_empty() {
        return None;
    }
    let mut id = [0u8; 16];
    let take = first.len().min(16);
    id[..take].copy_from_slice(&first[..take]);
    if take < 16 {
        let extra = sha::sha256(&first);
        id[take..].copy_from_slice(&extra[..16 - take]);
    }
    Some(id)
}

fn build_parameters(
    options: &EncryptOptions,
    revision: u8,
    id_first: &[u8; 16],
) -> Result<SecurityParameters> {
    let method = options.method;
    let bits = method.key_length_bits();
    let p = permissions_mask(options.permissions);
    match method {
        EncryptionMethod::Rc4_40 | EncryptionMethod::Rc4_128 | EncryptionMethod::Aes128 => {
            let version = if method == EncryptionMethod::Rc4_40 {
                1
            } else if method == EncryptionMethod::Rc4_128 {
                2
            } else {
                4
            };
            let o = compute_o_value(
                &options.owner_password,
                &options.user_password,
                revision,
                bits,
            )?;
            let mut params = SecurityParameters {
                revision,
                version,
                key_length_bits: bits,
                o,
                u: vec![0u8; 32],
                p,
                id_first: id_first.to_vec(),
                encrypt_metadata: options.encrypt_metadata,
                ue: Vec::new(),
                oe: Vec::new(),
                perms: Vec::new(),
                aes_streams: method == EncryptionMethod::Aes128,
            };
            let file_key = compute_file_key(&options.user_password, &params)?;
            params.u = compute_u_value(&file_key, &params)?;
            Ok(params)
        }
        EncryptionMethod::Aes256 => {
            let file_key = derive_file_key(id_first, options);
            let (u, ue) = compute_u_and_ue(
                revision,
                &options.user_password,
                &file_key,
                &salt(id_first, b"u-validation"),
                &salt(id_first, b"u-key"),
            )?;
            let (o, oe) = compute_o_and_oe(
                revision,
                &options.owner_password,
                &u,
                &file_key,
                &salt(id_first, b"o-validation"),
                &salt(id_first, b"o-key"),
            )?;
            let perms = compute_perms(
                &file_key,
                p,
                options.encrypt_metadata,
                &perms_padding(id_first, b"perms"),
            )?;
            Ok(SecurityParameters {
                revision,
                version: 5,
                key_length_bits: bits,
                o: o.to_vec(),
                u: u.to_vec(),
                p,
                id_first: id_first.to_vec(),
                encrypt_metadata: options.encrypt_metadata,
                ue: ue.to_vec(),
                oe: oe.to_vec(),
                perms: perms.to_vec(),
                aes_streams: false,
            })
        }
        EncryptionMethod::None => Err(SecurityError::UnsupportedMethod("none".to_owned())),
    }
}

fn crypt_filter(cfm: &str, length: i64) -> Object {
    let mut filter = Dictionary::new();
    filter.insert("Type".to_owned(), Object::Name("CryptFilter".to_owned()));
    filter.insert("CFM".to_owned(), Object::Name(cfm.to_owned()));
    filter.insert("AuthEvent".to_owned(), Object::Name("DocOpen".to_owned()));
    filter.insert("Length".to_owned(), Object::Integer(length));
    let mut filters = Dictionary::new();
    filters.insert("StdCF".to_owned(), Object::Dictionary(filter));
    Object::Dictionary(filters)
}

/// Build the `/Encrypt` dictionary described by `params` and `options`.
pub fn build_encrypt_dictionary(
    params: &SecurityParameters,
    options: &EncryptOptions,
    revision: u8,
) -> Result<Dictionary> {
    let mut dictionary = Dictionary::new();
    dictionary.insert("Filter".to_owned(), Object::Name("Standard".to_owned()));
    dictionary.insert("V".to_owned(), Object::Integer(i64::from(params.version)));
    dictionary.insert("R".to_owned(), Object::Integer(i64::from(revision)));
    dictionary.insert(
        "Length".to_owned(),
        Object::Integer(i64::from(params.key_length_bits)),
    );
    dictionary.insert("O".to_owned(), Object::String(params.o.clone()));
    dictionary.insert("U".to_owned(), Object::String(params.u.clone()));
    // PDF stores /P as a signed 32-bit integer.
    dictionary.insert("P".to_owned(), Object::Integer(i64::from(params.p as i32)));
    match options.method {
        EncryptionMethod::Rc4_40 | EncryptionMethod::Rc4_128 => {}
        EncryptionMethod::Aes128 => {
            dictionary.insert("CF".to_owned(), crypt_filter("AESV2", 16));
            dictionary.insert("StmF".to_owned(), Object::Name("StdCF".to_owned()));
            dictionary.insert("StrF".to_owned(), Object::Name("StdCF".to_owned()));
            dictionary.insert(
                "EncryptMetadata".to_owned(),
                Object::Boolean(options.encrypt_metadata),
            );
        }
        EncryptionMethod::Aes256 => {
            dictionary.insert("OE".to_owned(), Object::String(params.oe.clone()));
            dictionary.insert("UE".to_owned(), Object::String(params.ue.clone()));
            dictionary.insert("Perms".to_owned(), Object::String(params.perms.clone()));
            dictionary.insert("CF".to_owned(), crypt_filter("AESV3", 32));
            dictionary.insert("StmF".to_owned(), Object::Name("StdCF".to_owned()));
            dictionary.insert("StrF".to_owned(), Object::Name("StdCF".to_owned()));
            dictionary.insert(
                "EncryptMetadata".to_owned(),
                Object::Boolean(options.encrypt_metadata),
            );
        }
        EncryptionMethod::None => {
            return Err(SecurityError::UnsupportedMethod("none".to_owned()));
        }
    }
    Ok(dictionary)
}

fn transform_value(
    id: ObjectId,
    object: &Object,
    decryptor: &Decryptor,
    encrypt: bool,
) -> Result<Object> {
    let transform = |data: &[u8]| {
        if encrypt {
            decryptor.encrypt_object(id.number, id.generation, data)
        } else {
            decryptor.decrypt_object(id.number, id.generation, data)
        }
    };
    match object {
        Object::String(bytes) => Ok(Object::String(transform(bytes)?)),
        Object::Array(items) => {
            let mut output = Vec::with_capacity(items.len());
            for item in items {
                output.push(transform_value(id, item, decryptor, encrypt)?);
            }
            Ok(Object::Array(output))
        }
        Object::Dictionary(dictionary) => {
            let mut output = Dictionary::new();
            for (key, value) in dictionary {
                output.insert(key.clone(), transform_value(id, value, decryptor, encrypt)?);
            }
            Ok(Object::Dictionary(output))
        }
        Object::Stream { dictionary, data } => {
            let mut output = Dictionary::new();
            for (key, value) in dictionary {
                output.insert(key.clone(), transform_value(id, value, decryptor, encrypt)?);
            }
            let payload = transform(data)?;
            let length =
                i64::try_from(payload.len()).map_err(|_| SecurityError::EncryptionFailed)?;
            output.insert("Length".to_owned(), Object::Integer(length));
            Ok(Object::Stream {
                dictionary: output,
                data: payload,
            })
        }
        other => Ok(other.clone()),
    }
}

fn transform_objects(
    objects: &[(ObjectId, Object)],
    decryptor: &Decryptor,
    encrypt: bool,
) -> Result<Vec<(ObjectId, Object)>> {
    objects
        .iter()
        .map(|(id, object)| Ok((*id, transform_value(*id, object, decryptor, encrypt)?)))
        .collect()
}

/// Collect object identifiers from classic `N G obj` headers in source bytes.
fn collect_object_ids(bytes: &[u8]) -> Vec<ObjectId> {
    let mut ids: Vec<ObjectId> = Vec::new();
    for line in bytes.split(|byte| *byte == b'\n') {
        let text = match std::str::from_utf8(line) {
            Ok(text) => text.trim(),
            Err(_) => continue,
        };
        let mut parts = text.split_ascii_whitespace();
        let (Some(number), Some(generation), Some(keyword)) =
            (parts.next(), parts.next(), parts.next())
        else {
            continue;
        };
        if keyword != "obj"
            || number.is_empty()
            || generation.is_empty()
            || !number.bytes().all(|byte| byte.is_ascii_digit())
            || !generation.bytes().all(|byte| byte.is_ascii_digit())
        {
            continue;
        }
        let (Ok(number), Ok(generation)) = (number.parse::<u32>(), generation.parse::<u16>())
        else {
            continue;
        };
        if number == 0 || ids.iter().any(|id| id.number == number) {
            continue;
        }
        ids.push(ObjectId::new(number, generation));
    }
    ids.sort_by_key(|id| id.number);
    ids
}

fn load_objects(document: &Document) -> Result<Vec<(ObjectId, Object)>> {
    let mut objects = Vec::new();
    for id in collect_object_ids(document.bytes()) {
        let object = document.object(id).map_err(core_error)?;
        objects.push((id, object));
    }
    Ok(objects)
}

fn reject_unsupported(document: &Document, objects: &[(ObjectId, Object)]) -> Result<()> {
    // Inspect xref sections directly from the bytes so an unrelated structural
    // problem in the page tree cannot mask the typed unsupported-path error.
    if pdfxi_core::inspect_xref(document.bytes())
        .map_err(core_error)?
        .uses_xref_stream
    {
        return Err(SecurityError::UnsupportedDocumentStructure(
            "cross-reference-stream documents are out of scope",
        ));
    }
    for (_, object) in objects {
        let Some(dictionary) = object.as_dictionary() else {
            continue;
        };
        if dictionary.contains_key("Linearized") {
            return Err(SecurityError::UnsupportedDocumentStructure(
                "linearized documents are out of scope",
            ));
        }
        let subtype = dictionary.get("Subtype").and_then(Object::as_name);
        if dictionary.get("Type").and_then(Object::as_name) == Some("Sig")
            || subtype == Some("Sig")
            || dictionary.contains_key("ByteRange")
        {
            return Err(SecurityError::UnsupportedDocumentStructure(
                "digital signatures are out of scope",
            ));
        }
    }
    Ok(())
}

fn write_name(name: &[u8], out: &mut Vec<u8>) {
    out.push(b'/');
    for &byte in name {
        let safe = !matches!(
            byte,
            0..=32 | b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#'
        );
        if safe {
            out.push(byte);
        } else {
            out.extend_from_slice(format!("#{byte:02X}").as_bytes());
        }
    }
}

fn write_dictionary(dictionary: &Dictionary, out: &mut Vec<u8>) -> Result<()> {
    out.extend_from_slice(b"<<");
    for (key, value) in dictionary {
        out.push(b' ');
        write_name(key.as_bytes(), out);
        out.push(b' ');
        write_object(value, out)?;
    }
    out.extend_from_slice(b" >>");
    Ok(())
}

fn write_object(object: &Object, out: &mut Vec<u8>) -> Result<()> {
    match object {
        Object::Null => out.extend_from_slice(b"null"),
        Object::Boolean(true) => out.extend_from_slice(b"true"),
        Object::Boolean(false) => out.extend_from_slice(b"false"),
        Object::Integer(value) => out.extend_from_slice(value.to_string().as_bytes()),
        Object::Real(value) => {
            if !value.is_finite() {
                return Err(SecurityError::EncryptionFailed);
            }
            if value.fract() == 0.0 {
                out.extend_from_slice(format!("{value:.1}").as_bytes());
            } else {
                out.extend_from_slice(value.to_string().as_bytes());
            }
        }
        Object::Name(name) => write_name(name.as_bytes(), out),
        Object::String(bytes) => {
            out.push(b'<');
            for byte in bytes {
                out.extend_from_slice(format!("{byte:02X}").as_bytes());
            }
            out.push(b'>');
        }
        Object::Array(items) => {
            out.push(b'[');
            for item in items {
                out.push(b' ');
                write_object(item, out)?;
            }
            out.extend_from_slice(b" ]");
        }
        Object::Dictionary(dictionary) => write_dictionary(dictionary, out)?,
        Object::Reference(id) => {
            out.extend_from_slice(format!("{} {} R", id.number, id.generation).as_bytes());
        }
        Object::Stream { dictionary, data } => {
            let mut sized = dictionary.clone();
            let length = i64::try_from(data.len()).map_err(|_| SecurityError::EncryptionFailed)?;
            sized.insert("Length".to_owned(), Object::Integer(length));
            write_dictionary(&sized, out)?;
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
    }
    Ok(())
}

fn write_xref(out: &mut Vec<u8>, offsets: &BTreeMap<u32, usize>) {
    out.extend_from_slice(b"xref\n");
    let mut entries: Vec<(u32, Option<usize>)> = vec![(0, None)];
    for (number, offset) in offsets {
        entries.push((*number, Some(*offset)));
    }
    let mut index = 0usize;
    while index < entries.len() {
        let first = entries[index].0;
        let mut count = 1usize;
        while index + count < entries.len()
            && first.checked_add(count as u32) == Some(entries[index + count].0)
        {
            count += 1;
        }
        out.extend_from_slice(format!("{first} {count}\n").as_bytes());
        for entry in &entries[index..index + count] {
            match entry.1 {
                None => out.extend_from_slice(b"0000000000 65535 f \n"),
                Some(offset) => {
                    out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes())
                }
            }
        }
        index += count;
    }
}

/// Serialize a standalone PDF with a single classic xref table.
fn write_pdf(
    objects: &[(ObjectId, Object)],
    root: ObjectId,
    trailer: &Dictionary,
) -> Result<Vec<u8>> {
    let mut ordered: Vec<(ObjectId, Object)> = objects.to_vec();
    ordered.sort_by_key(|(id, _)| id.number);

    let mut out = b"%PDF-1.7\n%PDFXI\n".to_vec();
    let mut offsets = BTreeMap::new();
    for (id, object) in &ordered {
        offsets.insert(id.number, out.len());
        out.extend_from_slice(format!("{} {} obj\n", id.number, id.generation).as_bytes());
        write_object(object, &mut out)?;
        out.extend_from_slice(b"\nendobj\n");
    }

    let xref_offset = out.len();
    write_xref(&mut out, &offsets);

    let highest = ordered.last().map_or(0, |(id, _)| id.number);
    let declared = trailer
        .get("Size")
        .and_then(Object::as_i64)
        .filter(|value| *value >= 0)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    let size = declared.max(highest.saturating_add(1)).max(1);
    let mut final_trailer = trailer.clone();
    final_trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));
    final_trailer.insert("Root".to_owned(), Object::Reference(root));
    out.extend_from_slice(b"trailer\n");
    write_dictionary(&final_trailer, &mut out)?;
    out.extend_from_slice(format!("\nstartxref\n{xref_offset}\n%%EOF\n").as_bytes());
    Ok(out)
}

/// Encrypt a plaintext document and return the PDF bytes plus a typed report.
///
/// The source is left untouched. Strings and streams of every in-use object are
/// encrypted with the per-object key of that object's number. The `/Encrypt`
/// dictionary is appended as a new object, and the trailer receives `/Encrypt`
/// plus a two-element `/ID`. An existing first `/ID` element is preserved;
/// otherwise one is derived from the seed or the passwords.
///
/// # Errors
///
/// Returns a typed error for unsupported structures, invalid options, or
/// serialization failures. No password or content is included in an error.
pub fn encrypt_document(
    source: &Document,
    options: &EncryptOptions,
) -> Result<(Vec<u8>, EncryptionReport)> {
    let objects = load_objects(source)?;
    reject_unsupported(source, &objects)?;

    let revision = options
        .revision
        .unwrap_or_else(|| default_revision(options.method));
    let id_first = trailer_id(source.bytes())
        .or_else(|| {
            options.id_seed.as_deref().map(derive_id).or_else(|| {
                let mut seed = Vec::new();
                seed.extend_from_slice(&options.user_password);
                seed.push(0);
                seed.extend_from_slice(&options.owner_password);
                seed.push(0);
                seed.extend_from_slice(options.method.as_id().as_bytes());
                Some(derive_id(&seed))
            })
        })
        .ok_or(SecurityError::EncryptionFailed)?;

    let params = build_parameters(options, revision, &id_first)?;
    let handler = StandardSecurityHandler::new(params.clone())?;
    let decryptor = handler.authenticate(&options.user_password)?;

    let mut encrypted = transform_objects(&objects, &decryptor, true)?;
    let encrypt_number = encrypted
        .iter()
        .map(|(id, _)| id.number)
        .max()
        .unwrap_or(0)
        .saturating_add(1);
    let encrypt_id = ObjectId::new(encrypt_number, 0);
    let encrypt_dictionary = build_encrypt_dictionary(&params, options, revision)?;
    encrypted.push((encrypt_id, Object::Dictionary(encrypt_dictionary)));

    let id_second = derive_second_id(&id_first);
    let mut trailer = Dictionary::new();
    trailer.insert(
        "ID".to_owned(),
        Object::Array(vec![
            Object::String(id_first.to_vec()),
            Object::String(id_second.to_vec()),
        ]),
    );
    trailer.insert("Encrypt".to_owned(), Object::Reference(encrypt_id));

    let bytes = write_pdf(&encrypted, source.root().map_err(core_error)?, &trailer)?;
    let report = EncryptionReport {
        method: options.method,
        revision,
        key_length_bits: options.method.key_length_bits(),
        permissions: options.permissions,
        raw_permissions: params.p,
        owner_password_set: options.owner_password != options.user_password,
        id_first,
        encrypted_objects: objects.len(),
    };
    Ok((bytes, report))
}

fn int_field(dictionary: &Dictionary, key: &str) -> Result<i64> {
    dictionary.get(key).and_then(Object::as_i64).ok_or_else(|| {
        SecurityError::EncryptedDocumentInvalid(format!("missing or invalid /{key}"))
    })
}

fn bytes_field(dictionary: &Dictionary, key: &str) -> Result<Vec<u8>> {
    match dictionary.get(key) {
        Some(Object::String(bytes)) => Ok(bytes.clone()),
        _ => Err(SecurityError::EncryptedDocumentInvalid(format!(
            "missing or invalid /{key}"
        ))),
    }
}

/// Locate the standard `/Encrypt` dictionary among a document's objects.
fn find_encrypt_dictionary(
    document: &Document,
    ids: &[ObjectId],
) -> Result<(ObjectId, Dictionary)> {
    for id in ids {
        let Ok(object) = document.object(*id) else {
            continue;
        };
        if let Some(dictionary) = object.as_dictionary()
            && dictionary.get("Filter").and_then(Object::as_name) == Some("Standard")
        {
            return Ok((*id, dictionary.clone()));
        }
    }
    Err(SecurityError::EncryptedDocumentInvalid(
        "no standard /Encrypt dictionary was found".to_owned(),
    ))
}

fn detect_aes_streams(dictionary: &Dictionary) -> bool {
    let Some(name) = dictionary.get("StmF").and_then(Object::as_name) else {
        return false;
    };
    dictionary
        .get("CF")
        .and_then(Object::as_dictionary)
        .and_then(|filters| filters.get(name))
        .and_then(Object::as_dictionary)
        .and_then(|filter| filter.get("CFM"))
        .and_then(Object::as_name)
        .is_some_and(|cfm| cfm == "AESV2" || cfm == "AESV3")
}

fn parameters_from_encrypt(
    dictionary: &Dictionary,
    id_first: [u8; 16],
) -> Result<SecurityParameters> {
    let revision = u8::try_from(int_field(dictionary, "R")?)
        .map_err(|_| SecurityError::EncryptedDocumentInvalid("/R out of range".to_owned()))?;
    let version = u8::try_from(int_field(dictionary, "V")?)
        .map_err(|_| SecurityError::EncryptedDocumentInvalid("/V out of range".to_owned()))?;
    let key_length_bits = u16::try_from(int_field(dictionary, "Length")?)
        .map_err(|_| SecurityError::EncryptedDocumentInvalid("/Length out of range".to_owned()))?;
    let p = (int_field(dictionary, "P")? as i32) as u32;
    let encrypt_metadata = dictionary
        .get("EncryptMetadata")
        .and_then(|value| match value {
            Object::Boolean(flag) => Some(*flag),
            _ => None,
        })
        .unwrap_or(true);
    let is_r5 = revision >= 5;
    Ok(SecurityParameters {
        revision,
        version,
        key_length_bits,
        o: bytes_field(dictionary, "O")?,
        u: bytes_field(dictionary, "U")?,
        p,
        id_first: id_first.to_vec(),
        encrypt_metadata,
        ue: if is_r5 {
            bytes_field(dictionary, "UE")?
        } else {
            Vec::new()
        },
        oe: if is_r5 {
            bytes_field(dictionary, "OE")?
        } else {
            Vec::new()
        },
        perms: if is_r5 {
            bytes_field(dictionary, "Perms")?
        } else {
            Vec::new()
        },
        aes_streams: detect_aes_streams(dictionary),
    })
}

/// Open an encrypted PDF with a password and return a decrypted document.
///
/// `None` means no password was supplied and yields
/// [`SecurityError::MissingPassword`]. A wrong password yields
/// [`SecurityError::WrongPassword`]. On success the returned document is a
/// fresh plaintext PDF (no `/Encrypt` entry) that can be read with the core
/// parser, together with a typed [`DecryptionReport`].
///
/// # Errors
///
/// Returns a typed error for a missing or wrong password, a malformed
/// `/Encrypt` dictionary, or an unsupported structure.
pub fn open_encrypted(
    bytes: &[u8],
    password: Option<&[u8]>,
) -> Result<(Document, DecryptionReport)> {
    let password = password.ok_or(SecurityError::MissingPassword)?;
    let document = Document::from_bytes(bytes.to_vec()).map_err(core_error)?;
    let ids = collect_object_ids(bytes);
    let (encrypt_id, encrypt_dictionary) = find_encrypt_dictionary(&document, &ids)?;
    let filter = encrypt_dictionary
        .get("Filter")
        .and_then(Object::as_name)
        .unwrap_or("");
    if filter != "Standard" {
        return Err(SecurityError::UnsupportedMethod(format!("filter {filter}")));
    }

    let id_first = trailer_id(bytes).ok_or_else(|| {
        SecurityError::EncryptedDocumentInvalid("the trailer has no /ID".to_owned())
    })?;
    let params = parameters_from_encrypt(&encrypt_dictionary, id_first)?;
    let revision = params.revision;
    let handler = StandardSecurityHandler::new(params)?;
    let decryptor = handler.authenticate(password)?;
    let permissions = decryptor.permissions_of_session();
    let role = permissions.role();
    let method = decryptor.method();

    let mut objects = load_objects(&document)?;
    objects.retain(|(id, _)| *id != encrypt_id);
    let decrypted = transform_objects(&objects, &decryptor, false)?;

    let mut trailer = Dictionary::new();
    let second = derive_second_id(&id_first);
    trailer.insert(
        "ID".to_owned(),
        Object::Array(vec![
            Object::String(id_first.to_vec()),
            Object::String(second.to_vec()),
        ]),
    );
    let plaintext = write_pdf(&decrypted, document.root().map_err(core_error)?, &trailer)?;
    let opened = Document::from_bytes(plaintext).map_err(core_error)?;
    let report = DecryptionReport {
        method,
        revision,
        role,
        permissions,
    };
    Ok((opened, report))
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID_HEX: &str = "00112233445566778899AABBCCDDEEFF";

    /// Assemble a classic-xref PDF from object bodies with accurate offsets.
    fn build_pdf(objects: &[&str], with_id: bool) -> Vec<u8> {
        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::new();
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n{}\nendobj\n", index + 1, body).as_bytes());
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        let id = if with_id {
            format!(" /ID [<{ID_HEX}> <{ID_HEX}>]")
        } else {
            String::new()
        };
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R /Info 5 0 R{id} >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A one-page PDF with a binary content stream and a string-bearing Info object.
    fn fixture(with_id: bool) -> Vec<u8> {
        build_pdf(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>",
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
                "<< /Length 25 >>\nstream\nBT (Secret payload) Tj ET\nendstream",
                "<< /Title (PDFXI Secret Title) /Author (QA) >>",
                "<< /Producer (PDFXI fixture) >>",
            ],
            with_id,
        )
    }

    /// A fixture with an extra linearization dictionary object.
    fn linearized_fixture() -> Vec<u8> {
        let mut objects: Vec<&str> = vec![
            "<< /Type /Catalog /Pages 2 0 R >>",
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
            "<< /Length 25 >>\nstream\nBT (Secret payload) Tj ET\nendstream",
            "<< /Title (PDFXI Secret Title) /Author (QA) >>",
            "<< /Producer (PDFXI fixture) >>",
        ];
        objects.push("<< /Linearized 1 /L 2048 /H [1 2] /O 1 /E 100 /N 1 /T 1024 >>");
        build_pdf(&objects, false)
    }

    /// A fixture carrying a signature dictionary, which encryption refuses.
    fn signed_fixture() -> Vec<u8> {
        build_pdf(
            &[
                "<< /Type /Catalog /Pages 2 0 R >>",
                "<< /Type /Pages /Kids [3 0 R] /Count 1 >>",
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>",
                "<< /Length 25 >>\nstream\nBT (Secret payload) Tj ET\nendstream",
                "<< /Title (PDFXI Secret Title) /Author (QA) >>",
                "<< /Type /Sig /Filter /Adobe.PPKLite /ByteRange [0 10 20 30] /Contents <00> >>",
            ],
            false,
        )
    }

    fn options(method: EncryptionMethod) -> EncryptOptions {
        EncryptOptions::new(method, b"user-secret", b"owner-secret", Permission::ALL)
            .expect("valid options")
            .with_id_seed(b"pdfxi-fixed-seed".to_vec())
    }

    fn open(bytes: &[u8], password: &[u8]) -> (Document, DecryptionReport) {
        open_encrypted(bytes, Some(password)).expect("open with the correct password")
    }

    #[test]
    fn rc4_40_round_trip() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Rc4_40)).expect("encrypt");
        assert_eq!(report.method, EncryptionMethod::Rc4_40);
        assert_eq!(report.revision, 2);
        assert_eq!(report.key_length_bits, 40);
        let (opened, opened_report) = open(&encrypted, b"user-secret");
        assert_eq!(opened_report.method, EncryptionMethod::Rc4_40);
        assert_eq!(opened_report.role, PasswordRole::User);
        assert_eq!(opened.page_text(0).expect("page text"), "Secret payload");
    }

    #[test]
    fn aes_128_round_trip() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Aes128)).expect("encrypt");
        assert_eq!(report.method, EncryptionMethod::Aes128);
        assert_eq!(report.revision, 4);
        assert_eq!(report.key_length_bits, 128);
        let (opened, _) = open(&encrypted, b"user-secret");
        assert_eq!(opened.page_text(0).expect("page text"), "Secret payload");
    }

    #[test]
    fn aes_256_r6_round_trip() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        assert_eq!(report.method, EncryptionMethod::Aes256);
        assert_eq!(report.revision, 6);
        assert_eq!(report.key_length_bits, 256);
        let (opened, _) = open(&encrypted, b"user-secret");
        assert_eq!(opened.page_text(0).expect("page text"), "Secret payload");
    }

    #[test]
    fn owner_password_round_trip_uses_owner_role() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        assert!(report.owner_password_set);
        let (_, opened_report) = open(&encrypted, b"owner-secret");
        assert_eq!(opened_report.role, PasswordRole::Owner);
        assert!(opened_report.permissions.allows(Permission::ASSEMBLE));
    }

    #[test]
    fn wrong_password_is_rejected() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        for method in [
            EncryptionMethod::Rc4_40,
            EncryptionMethod::Rc4_128,
            EncryptionMethod::Aes128,
            EncryptionMethod::Aes256,
        ] {
            let (encrypted, _) = encrypt_document(&source, &options(method)).expect("encrypt");
            for candidate in [
                b"".as_slice(),
                b"user-secre",
                b"owner-secre",
                b"USER-SECRET",
            ] {
                assert_eq!(
                    open_encrypted(&encrypted, Some(candidate)).err(),
                    Some(SecurityError::WrongPassword),
                    "{method} accepted a wrong password"
                );
            }
        }
    }

    #[test]
    fn missing_password_is_rejected() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, _) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        assert_eq!(
            open_encrypted(&encrypted, None).err(),
            Some(SecurityError::MissingPassword)
        );
    }

    #[test]
    fn permissions_values_are_preserved_and_enforced() {
        let granted = Permission::PRINT.union(Permission::COPY);
        let opts = EncryptOptions::new(
            EncryptionMethod::Aes256,
            b"user-secret",
            b"owner-secret",
            granted,
        )
        .expect("options")
        .with_id_seed(b"permissions-seed".to_vec());
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, report) = encrypt_document(&source, &opts).expect("encrypt");
        assert_eq!(report.permissions, granted);
        assert_eq!(report.raw_permissions & granted.bits(), granted.bits());
        // PDF reserves bits 7 and 8 of /P as always-set.
        assert_eq!(report.raw_permissions & 0xC0, 0xC0);

        let (_, user) = open(&encrypted, b"user-secret");
        assert_eq!(user.permissions.bits(), granted);
        assert!(user.permissions.allows(Permission::PRINT));
        assert!(user.permissions.allows(Permission::COPY));
        assert!(!user.permissions.allows(Permission::MODIFY));
        assert!(!user.permissions.allows(Permission::ASSEMBLE));

        let (_, owner) = open(&encrypted, b"owner-secret");
        assert!(owner.permissions.allows(Permission::MODIFY));
        assert!(owner.permissions.allows(Permission::ASSEMBLE));
    }

    #[test]
    fn encrypt_dictionary_has_expected_fields() {
        for (method, version, revision, aes) in [
            (EncryptionMethod::Rc4_40, 1, 2, false),
            (EncryptionMethod::Rc4_128, 2, 3, false),
            (EncryptionMethod::Aes128, 4, 4, true),
            (EncryptionMethod::Aes256, 5, 6, true),
        ] {
            let source = Document::from_bytes(fixture(true)).expect("source");
            let (encrypted, _) = encrypt_document(&source, &options(method)).expect("encrypt");
            let doc = Document::from_bytes(encrypted).expect("encrypted parses with core");
            let ids = collect_object_ids(doc.bytes());
            let (_, dict) = find_encrypt_dictionary(&doc, &ids).expect("encrypt dict");
            assert_eq!(
                dict.get("Filter").and_then(Object::as_name),
                Some("Standard")
            );
            assert_eq!(dict.get("V").and_then(Object::as_i64), Some(version));
            assert_eq!(
                dict.get("R").and_then(Object::as_i64),
                Some(i64::from(revision))
            );
            assert!(dict.contains_key("O"));
            assert!(dict.contains_key("U"));
            assert!(dict.contains_key("P"));
            assert_eq!(detect_aes_streams(&dict), aes);
            assert!(trailer_id(doc.bytes()).is_some());
        }
    }

    #[test]
    fn strings_are_encrypted_and_recovered() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, _) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        let encrypted_doc = Document::from_bytes(encrypted.clone()).expect("encrypted doc");
        let info = encrypted_doc
            .object(ObjectId::new(5, 0))
            .expect("info object");
        let title = info
            .as_dictionary()
            .and_then(|dict| dict.get("Title"))
            .and_then(|value| match value {
                Object::String(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .expect("title string");
        assert_ne!(title, b"PDFXI Secret Title".to_vec());

        let (opened, _) = open(&encrypted, b"user-secret");
        let info = opened.object(ObjectId::new(5, 0)).expect("info object");
        let title = info
            .as_dictionary()
            .and_then(|dict| dict.get("Title"))
            .and_then(|value| match value {
                Object::String(bytes) => Some(bytes.clone()),
                _ => None,
            })
            .expect("title string");
        assert_eq!(title, b"PDFXI Secret Title".to_vec());
    }

    #[test]
    fn streams_are_encrypted_and_recovered() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, _) =
            encrypt_document(&source, &options(EncryptionMethod::Aes128)).expect("encrypt");
        let encrypted_doc = Document::from_bytes(encrypted.clone()).expect("encrypted doc");
        let stream = encrypted_doc
            .object(ObjectId::new(4, 0))
            .expect("content object");
        let Object::Stream { data, .. } = stream else {
            panic!("content must be a stream");
        };
        assert!(!data.windows(7).any(|window| window == b"Secret"));

        let (opened, _) = open(&encrypted, b"user-secret");
        assert_eq!(
            opened.page_text(0).expect("decrypted text"),
            "Secret payload"
        );
    }

    #[test]
    fn deterministic_output() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let first = encrypt_document(&source, &options(EncryptionMethod::Aes256))
            .expect("encrypt")
            .0;
        let second = encrypt_document(&source, &options(EncryptionMethod::Aes256))
            .expect("encrypt")
            .0;
        assert_eq!(
            first, second,
            "identical inputs must produce identical bytes"
        );

        // With no existing /ID the seed drives both the ID and the derived key.
        let seed_source = Document::from_bytes(fixture(false)).expect("source");
        let other = EncryptOptions::new(
            EncryptionMethod::Aes256,
            b"user-secret",
            b"owner-secret",
            Permission::ALL,
        )
        .expect("options")
        .with_id_seed(b"a-different-seed".to_vec());
        let changed = encrypt_document(&seed_source, &other).expect("encrypt").0;
        let baseline = encrypt_document(&seed_source, &options(EncryptionMethod::Aes256))
            .expect("encrypt")
            .0;
        assert_ne!(baseline, changed);
    }

    #[test]
    fn id_derivation_and_existing_id_are_respected() {
        let with_existing = fixture(true);
        let source = Document::from_bytes(with_existing.clone()).expect("source");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        assert_eq!(
            report.id_first,
            [
                0x00, 0x11, 0x22, 0x33, 0x44, 0x55, 0x66, 0x77, 0x88, 0x99, 0xAA, 0xBB, 0xCC, 0xDD,
                0xEE, 0xFF,
            ]
        );
        assert_eq!(trailer_id(&encrypted), Some(report.id_first));

        let without_id = fixture(false);
        let source = Document::from_bytes(without_id).expect("source");
        let expected = derive_id(b"pdfxi-fixed-seed");
        let (encrypted, report) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        assert_eq!(report.id_first, expected);
        assert_eq!(trailer_id(&encrypted), Some(expected));
        assert_ne!(trailer_id(&encrypted), Some(derive_second_id(&expected)));
    }

    #[test]
    fn typed_unsupported_path_errors() {
        let source = Document::from_bytes(linearized_fixture()).expect("linearized source");
        let objects = load_objects(&source).expect("objects");
        assert!(matches!(
            reject_unsupported(&source, &objects),
            Err(SecurityError::UnsupportedDocumentStructure(_))
        ));

        let source = Document::from_bytes(signed_fixture()).expect("signed source");
        let objects = load_objects(&source).expect("objects");
        assert!(matches!(
            reject_unsupported(&source, &objects),
            Err(SecurityError::UnsupportedDocumentStructure(_))
        ));
    }

    #[test]
    fn tampered_encrypted_output_is_rejected() {
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, _) =
            encrypt_document(&source, &options(EncryptionMethod::Aes256)).expect("encrypt");
        let mut tampered = encrypted.clone();
        // Flip one hex digit of the encrypted /Perms value so the file key no
        // longer validates the dictionary.
        let position = tampered
            .windows(8)
            .position(|window| window == b"/Perms <")
            .expect("/Perms entry")
            + 8;
        tampered[position] = if tampered[position] == b'0' {
            b'1'
        } else {
            b'0'
        };
        let err = open_encrypted(&tampered, Some(b"user-secret")).err();
        assert!(
            matches!(
                err,
                Some(SecurityError::WrongPassword)
                    | Some(SecurityError::DecryptionFailed)
                    | Some(SecurityError::EncryptedDocumentInvalid(_))
            ),
            "tampered output was accepted: {err:?}"
        );
    }

    #[test]
    fn encrypt_metadata_false_is_recorded() {
        let opts = EncryptOptions::new(
            EncryptionMethod::Aes256,
            b"user-secret",
            b"owner-secret",
            Permission::ALL,
        )
        .expect("options")
        .with_id_seed(b"metadata-seed".to_vec())
        .with_encrypt_metadata(false);
        let source = Document::from_bytes(fixture(true)).expect("source");
        let (encrypted, _) = encrypt_document(&source, &opts).expect("encrypt");
        let doc = Document::from_bytes(encrypted.clone()).expect("encrypted doc");
        let ids = collect_object_ids(doc.bytes());
        let (_, dict) = find_encrypt_dictionary(&doc, &ids).expect("encrypt dict");
        assert_eq!(
            dict.get("EncryptMetadata").and_then(|v| match v {
                Object::Boolean(flag) => Some(*flag),
                _ => None,
            }),
            Some(false)
        );
        let (opened, _) = open(&encrypted, b"user-secret");
        assert_eq!(opened.page_text(0).expect("text"), "Secret payload");
    }

    #[test]
    fn invalid_options_are_rejected_with_typed_errors() {
        assert_eq!(
            EncryptOptions::new(EncryptionMethod::None, b"user", b"owner", Permission::ALL).err(),
            Some(SecurityError::UnsupportedMethod("none".to_owned()))
        );
        let opts = options(EncryptionMethod::Aes256);
        assert!(opts.clone().with_revision(5).is_ok());
        assert!(opts.with_revision(4).is_err());
        let rendered = format!("{:?}", options(EncryptionMethod::Aes256));
        assert!(rendered.contains("<redacted>"));
        assert!(!rendered.contains("user-secret"));
        assert!(!rendered.contains("owner-secret"));
    }
}
