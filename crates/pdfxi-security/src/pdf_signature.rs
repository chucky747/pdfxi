//! Real PDF signatures: CMS `SignedData` over a PDF `/ByteRange`.
//!
//! This module produces the `adbe.pkcs7.detached` signature shape that
//! Acrobat XI and every conforming reader understand:
//!
//! 1. an incremental revision adds a signature dictionary with `/Type /Sig`,
//!    `/Filter /Adobe.PPKLite`, `/SubFilter /adbe.pkcs7.detached`, a
//!    `/ByteRange` array, and a `/Contents` placeholder, plus a signature
//!    widget on a page and an `/AcroForm` entry pointing at it;
//! 2. the placeholder is replaced with the DER CMS blob without changing any
//!    byte length, so every cross-reference offset produced by
//!    [`pdfxi_core::Document::save_incremental`] stays valid;
//! 3. the `/ByteRange` values are written before the digest is computed, so
//!    they are themselves covered by the signature;
//! 4. the CMS `messageDigest` signed attribute is the SHA-256 digest of the
//!    two signed ranges concatenated, and the RSA signature is PKCS#1 v1.5
//!    with SHA-256 over the DER `SET OF` signed attributes.
//!
//! Cryptographic operations use RustCrypto crates only: `sha2` for hashing,
//! `rsa` for PKCS#1 v1.5, `cms`/`x509-cert`/`der`/`spki`/`const-oid` for
//! CMS and X.509 encoding. No key material is ever logged, and
//! `SigningIdentity` has a redacting [`core::fmt::Debug`] implementation.
//!
//! # What this module proves, and what it does not
//!
//! [`verify_signature`] proves four things about the bytes it is given:
//!
//! - the CMS `SignedData` parses as DER and carries exactly one `SignerInfo`;
//! - the RSA signature over the DER signed attributes verifies under the
//!   public key of the certificate named by the `SignerInfo` issuer/serial
//!   identifier;
//! - the signed attributes bind `contentType = id-data` and a
//!   `messageDigest` equal to SHA-256 over the declared ranges;
//! - the ranges are inside the file, the excluded `/Contents` region really
//!   is a hex string bounded by `<` and `>`, and the covered bytes are
//!   byte-for-byte unchanged.
//!
//! It does **not** establish trust. There is no certificate-path building, no
//! trust anchor, no revocation check, no RFC 3161 timestamp, and no policy
//! evaluation. [`SignatureVerification::certificate_chain_trusted`] is
//! `None` and stays `None` until a later round wires the crate's
//! [`crate::x509::verify_chain`] and [`crate::trust_manager`] into this
//! report. The `signingTime` attribute is self-asserted metadata from the
//! signer, not a trusted timestamp. RSA key size is enforced at load time
//! (2048 bits minimum) but not recorded as a policy decision in the report.
//!
//! A successful [`verify_signature`] with `status == Valid` therefore means
//! *cryptographic integrity under the embedded certificate*, nothing more.

use std::fmt;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use cms::builder::{SignedDataBuilder, SignerInfoBuilder};
use cms::cert::{CertificateChoices, IssuerAndSerialNumber};
use cms::content_info::{CmsVersion, ContentInfo};
use cms::signed_data::{EncapsulatedContentInfo, SignedData, SignerIdentifier};
use const_oid::db::{rfc5911, rfc5912};
use der::asn1::{GeneralizedTime, OctetString, SetOfVec, UtcTime};
use der::referenced::OwnedToRef;
use der::{DateTime, Decode, Encode};
use pdfxi_core::{Dictionary, Document, Object, ObjectId};
use rsa::pkcs1::DecodeRsaPrivateKey;
use rsa::pkcs1v15::{Signature as RsaRawSignature, SigningKey};
use rsa::pkcs8::DecodePrivateKey;
use rsa::traits::PublicKeyParts;
use rsa::{Pkcs1v15Sign, RsaPrivateKey, RsaPublicKey};
use sha2::{Digest as _, Sha256};
use spki::AlgorithmIdentifierOwned;
use x509_cert::Certificate as X509Certificate;
use x509_cert::attr::{Attribute, AttributeValue};

use crate::crypto::constant_time_eq;
use crate::error::{Result, SecurityError};
use crate::signature::{DigestAlgorithm, SignatureStatus};

/// Smallest `/Contents` reservation accepted from [`SignOptions`], in bytes of
/// DER (the PDF hex string is twice as long).
pub const MIN_CONTENTS_RESERVATION: usize = 4096;
/// Default `/Contents` reservation in DER bytes.
pub const DEFAULT_CONTENTS_RESERVATION: usize = 8192;
/// Default signature field name used by [`SignOptions::default`].
pub const DEFAULT_FIELD_NAME: &str = "Signature1";
/// Minimum RSA key size accepted by [`SigningIdentity::from_pem`].
const MIN_RSA_BITS: usize = 2048;
/// Width, in decimal digits, of each `/ByteRange` field written by this module.
///
/// Ten digits address a 10 GB file. The placeholder numbers emitted by the PDF
/// writer are exactly this wide, which lets the final values be written in
/// place without changing a single offset.
const BYTE_RANGE_FIELD_WIDTH: usize = 10;
/// Placeholder value serialized for the three mutable `/ByteRange` fields.
const BYTE_RANGE_PLACEHOLDER: i64 = 1_000_000_000;

fn core_error(error: pdfxi_core::Error) -> SecurityError {
    SecurityError::SignatureDocumentInvalid(error.to_string())
}

fn der_error() -> SecurityError {
    SecurityError::DerInvalid
}

fn sha256_algorithm() -> AlgorithmIdentifierOwned {
    AlgorithmIdentifierOwned {
        oid: rfc5912::ID_SHA_256,
        parameters: Some(der::Any::null()),
    }
}

fn now_unix() -> Result<u64> {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs())
        .map_err(|_| {
            SecurityError::Configuration("system clock precedes the Unix epoch".to_owned())
        })
}

/// Format a Unix timestamp as a PDF date string (`D:YYYYMMDDHHmmSSZ`).
fn pdf_date(unix_seconds: u64) -> Result<String> {
    let time = DateTime::from_unix_duration(Duration::from_secs(unix_seconds))
        .map_err(|_| SecurityError::Configuration("signing time is out of range".to_owned()))?;
    Ok(format!(
        "D:{:04}{:02}{:02}{:02}{:02}{:02}Z",
        time.year(),
        time.month(),
        time.day(),
        time.hour(),
        time.minutes(),
        time.seconds()
    ))
}

/// Build the CMS `signingTime` signed attribute from a caller-supplied clock.
///
/// The timestamp is self-asserted metadata, not a trusted timestamp token.
fn signing_time_attribute(unix_seconds: u64) -> Result<Attribute> {
    let time = DateTime::from_unix_duration(Duration::from_secs(unix_seconds))
        .map_err(|_| SecurityError::Configuration("signing time is out of range".to_owned()))?;
    let encoded = if (1950..=2049).contains(&time.year()) {
        UtcTime::from_date_time(time)
            .map_err(|_| der_error())?
            .to_der()
    } else {
        GeneralizedTime::from_date_time(time).to_der()
    }
    .map_err(|_| der_error())?;
    let value = AttributeValue::from_der(&encoded).map_err(|_| der_error())?;
    let mut values = SetOfVec::new();
    values.insert(value).map_err(|_| der_error())?;
    Ok(Attribute {
        oid: rfc5911::ID_SIGNING_TIME,
        values,
    })
}

/// A loaded signing identity: an RSA private key plus its certificate chain.
///
/// The certificate chain is ordered leaf first, as produced by
/// `openssl pkcs12`/`openssl req` and by Windows certificate export. The first
/// certificate must match the private key; loading fails otherwise. Key
/// material stays inside this value and is never exposed or logged.
pub struct SigningIdentity {
    key: SigningKey<Sha256>,
    chain: Vec<X509Certificate>,
}

impl SigningIdentity {
    /// Load an identity from a PEM private key and a PEM certificate chain.
    ///
    /// The key may be PKCS#8 (`BEGIN PRIVATE KEY`) or PKCS#1
    /// (`BEGIN RSA PRIVATE KEY`). The chain may contain one or more
    /// `BEGIN CERTIFICATE` blocks; the first is the signer, the rest are
    /// embedded as intermediates in the CMS `certificates` field.
    ///
    /// # Errors
    ///
    /// - [`SecurityError::SigningIdentityInvalid`] when the key or chain is
    ///   absent, malformed, or not a PEM document of the expected kind;
    /// - [`SecurityError::SigningKeyUnsupported`] for a non-RSA key or an RSA
    ///   key shorter than 2048 bits;
    /// - [`SecurityError::DigitalIdKeyAlgorithmMismatch`] when the first
    ///   certificate's public key does not match the private key.
    ///
    /// Encrypted PEM private keys and PKCS#12 (`.pfx`) containers are not
    /// accepted in this round; see the module documentation.
    pub fn from_pem(private_key_pem: &str, certificate_chain_pem: &str) -> Result<Self> {
        if private_key_pem.trim().is_empty() {
            return Err(SecurityError::SigningIdentityInvalid(
                "private key PEM is empty",
            ));
        }
        let key = RsaPrivateKey::from_pkcs8_pem(private_key_pem)
            .or_else(|_| RsaPrivateKey::from_pkcs1_pem(private_key_pem))
            .map_err(|_| {
                SecurityError::SigningIdentityInvalid(
                    "private key is not an unencrypted PKCS#8 or PKCS#1 RSA PEM",
                )
            })?;
        if key.size() * 8 < MIN_RSA_BITS {
            return Err(SecurityError::SigningKeyUnsupported(format!(
                "RSA key is {} bits; at least {MIN_RSA_BITS} are required",
                key.size() * 8
            )));
        }
        let chain =
            X509Certificate::load_pem_chain(certificate_chain_pem.as_bytes()).map_err(|_| {
                SecurityError::SigningIdentityInvalid("certificate chain is not valid PEM")
            })?;
        let signer = chain.first().ok_or(SecurityError::SigningIdentityInvalid(
            "certificate chain contains no certificate",
        ))?;

        let certificate_key = RsaPublicKey::try_from(
            signer
                .tbs_certificate
                .subject_public_key_info
                .owned_to_ref(),
        )
        .map_err(|_| {
            SecurityError::SigningKeyUnsupported(
                "signer certificate does not carry an RSA public key".to_owned(),
            )
        })?;
        if certificate_key != RsaPublicKey::from(&key) {
            return Err(SecurityError::DigitalIdKeyAlgorithmMismatch);
        }

        Ok(Self {
            key: SigningKey::new(key),
            chain,
        })
    }

    /// The signer certificate (the first certificate in the chain).
    pub fn signer_certificate(&self) -> &X509Certificate {
        // `from_pem` guarantees a non-empty chain.
        &self.chain[0]
    }

    /// Number of certificates carried by this identity.
    pub fn chain_len(&self) -> usize {
        self.chain.len()
    }

    /// Signer subject in RFC 4514 form, or `"<unparsable subject>"`.
    pub fn signer_subject(&self) -> String {
        certificate_subject(self.signer_certificate())
    }
}

impl fmt::Debug for SigningIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SigningIdentity")
            .field("chain_len", &self.chain.len())
            .field("key", &"<redacted>")
            .finish()
    }
}

/// Options for [`sign_pdf`].
#[derive(Debug, Clone)]
pub struct SignOptions {
    /// Zero-based page index that receives the signature widget.
    pub page_index: usize,
    /// AcroForm field name for the signature field.
    pub field_name: String,
    /// Optional `/Reason` entry, stored as a PDF text string.
    pub reason: Option<String>,
    /// Optional `/Location` entry, stored as a PDF text string.
    pub location: Option<String>,
    /// Unix seconds recorded in `/M` and in the CMS `signingTime` attribute.
    /// `None` reads the system clock.
    pub signing_time_unix: Option<u64>,
    /// Bytes reserved for the DER CMS blob before signing. Clamped up to
    /// [`MIN_CONTENTS_RESERVATION`].
    pub contents_reservation: usize,
}

impl Default for SignOptions {
    fn default() -> Self {
        Self {
            page_index: 0,
            field_name: DEFAULT_FIELD_NAME.to_owned(),
            reason: None,
            location: None,
            signing_time_unix: None,
            contents_reservation: DEFAULT_CONTENTS_RESERVATION,
        }
    }
}

/// The signed document bytes and what the signer wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignedDocument {
    bytes: Vec<u8>,
    report: SignatureReport,
}

impl SignedDocument {
    /// The complete signed PDF: the original bytes as an exact prefix followed
    /// by one incremental revision with the CMS blob in place.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Consume the value and return the signed PDF bytes.
    pub fn into_bytes(self) -> Vec<u8> {
        self.bytes
    }

    /// Metadata about the signature that was written.
    pub fn report(&self) -> &SignatureReport {
        &self.report
    }
}

/// Summary of a signature produced by [`sign_pdf`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureReport {
    byte_range: [usize; 4],
    cms_len: usize,
    contents_reservation: usize,
    signer_subject: String,
    signing_time_unix: u64,
    digest_algorithm: DigestAlgorithm,
    field_name: String,
    signature_object: u32,
    widget_object: u32,
    appended_bytes: usize,
}

impl SignatureReport {
    /// The `/ByteRange` written into the signature dictionary.
    pub fn byte_range(&self) -> [usize; 4] {
        self.byte_range
    }

    /// Exact DER length of the CMS blob, without hex encoding or padding.
    pub fn cms_len(&self) -> usize {
        self.cms_len
    }

    /// Bytes reserved for the DER blob.
    pub fn contents_reservation(&self) -> usize {
        self.contents_reservation
    }

    /// Signer subject copied from the certificate.
    pub fn signer_subject(&self) -> &str {
        &self.signer_subject
    }

    /// Unix seconds asserted by the signer.
    pub fn signing_time_unix(&self) -> u64 {
        self.signing_time_unix
    }

    /// Digest algorithm used for the CMS signature (always SHA-256 here).
    pub fn digest_algorithm(&self) -> DigestAlgorithm {
        self.digest_algorithm
    }

    /// AcroForm field name of the new signature field.
    pub fn field_name(&self) -> &str {
        &self.field_name
    }

    /// Object number of the `/Type /Sig` dictionary.
    pub fn signature_object(&self) -> u32 {
        self.signature_object
    }

    /// Object number of the `/Subtype /Widget` annotation.
    pub fn widget_object(&self) -> u32 {
        self.widget_object
    }

    /// Bytes appended to the original file.
    pub fn appended_bytes(&self) -> usize {
        self.appended_bytes
    }
}

/// Result of verifying the last signature in a PDF.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureVerification {
    status: SignatureStatus,
    signature_valid: bool,
    bytes_match: bool,
    covers_entire_file: bool,
    appended_after_signature: bool,
    signer_subject: String,
    signer_fingerprint_sha256: [u8; 32],
    signing_time_unix: Option<u64>,
    digest_algorithm: DigestAlgorithm,
    byte_range: [usize; 4],
    sub_filter: String,
    certificate_chain_trusted: Option<bool>,
}

impl SignatureVerification {
    /// Cryptographic status: `Valid`, `Invalid` (signature does not verify),
    /// or `Tampered` (signature verifies but the covered bytes changed).
    ///
    /// [`SignatureStatus::Expired`], [`SignatureStatus::Revoked`] and
    /// [`SignatureStatus::Unknown`] are never produced in this round: the
    /// verifier does not evaluate certificate validity or revocation.
    pub fn status(&self) -> SignatureStatus {
        self.status
    }

    /// `true` when the RSA signature over the signed attributes verifies.
    pub fn signature_valid(&self) -> bool {
        self.signature_valid
    }

    /// `true` when the SHA-256 digest of the signed ranges equals the
    /// `messageDigest` signed attribute.
    pub fn bytes_match(&self) -> bool {
        self.bytes_match
    }

    /// `true` when the signed ranges end exactly at the end of the file.
    pub fn covers_entire_file(&self) -> bool {
        self.covers_entire_file
    }

    /// `true` when bytes follow the signature. The signature remains valid
    /// for the bytes it covers; it simply does not cover the whole file.
    pub fn appended_after_signature(&self) -> bool {
        self.appended_after_signature
    }

    /// Subject of the certificate named by the `SignerInfo`.
    pub fn signer_subject(&self) -> &str {
        &self.signer_subject
    }

    /// SHA-256 fingerprint of the signer certificate DER.
    pub fn signer_fingerprint_sha256(&self) -> [u8; 32] {
        self.signer_fingerprint_sha256
    }

    /// Lowercase hexadecimal form of the signer certificate fingerprint.
    pub fn signer_fingerprint_hex(&self) -> String {
        let mut out = String::with_capacity(64);
        for byte in self.signer_fingerprint_sha256 {
            out.push_str(&format!("{byte:02x}"));
        }
        out
    }

    /// Self-asserted CMS `signingTime`, in Unix seconds.
    pub fn signing_time_unix(&self) -> Option<u64> {
        self.signing_time_unix
    }

    /// Digest algorithm taken from the `SignerInfo`.
    pub fn digest_algorithm(&self) -> DigestAlgorithm {
        self.digest_algorithm
    }

    /// The `/ByteRange` that was verified.
    pub fn byte_range(&self) -> [usize; 4] {
        self.byte_range
    }

    /// `/SubFilter` found near the signature dictionary, or `"unknown"`.
    pub fn sub_filter(&self) -> &str {
        &self.sub_filter
    }

    /// Trust-store decision.
    ///
    /// Always `None` in this round: `None` means **not evaluated**, never
    /// "trusted". Treat it as unknown, not as success.
    pub fn certificate_chain_trusted(&self) -> Option<bool> {
        self.certificate_chain_trusted
    }

    /// The badge condition for acceptance item A-20: the signature verifies,
    /// the covered bytes are unchanged, and the signature covers the whole
    /// file. It deliberately does not claim that the certificate is trusted.
    pub fn is_fully_valid(&self) -> bool {
        self.signature_valid && self.bytes_match && self.covers_entire_file
    }
}

/// Sign `document` with one invisible signature field on one page.
///
/// The mutation is an incremental append produced by
/// [`pdfxi_core::Document::save_incremental`]: every byte of the original file
/// stays an exact prefix, which is what keeps an earlier signature valid.
///
/// # Reserved `/Contents`
///
/// The CMS blob is padded with zero bytes inside the PDF hex string to the
/// reservation size. The returned [`SignatureReport::cms_len`] is the exact
/// DER length; readers ignore the padding. If the DER exceeds the reservation,
/// signing fails with [`SecurityError::SignatureContentsTooSmall`] rather than
/// truncating.
///
/// # Caller contract
///
/// The returned [`SignedDocument::bytes`] is the authoritative signed file.
/// `document` retains the *placeholder* objects staged for this revision, so
/// calling `save_incremental` again on the same value re-emits the unsigned
/// placeholder. Persist the returned bytes, then reopen them (for example
/// through `pdfxi-document`) for further edits.
///
/// # Errors
///
/// Structural document errors, a missing page, an unusable `/Annots` or
/// `/AcroForm` entry, a CMS build failure, or a `/Contents` overflow.
pub fn sign_pdf(
    document: &mut Document,
    identity: &SigningIdentity,
    options: &SignOptions,
) -> Result<SignedDocument> {
    let page = document.page(options.page_index).map_err(core_error)?;
    let page_object = document.object(page.id).map_err(core_error)?;
    let Object::Dictionary(mut page_dictionary) = page_object else {
        return Err(SecurityError::SignatureFieldInvalid(
            "page object is not a dictionary",
        ));
    };

    let signing_time = match options.signing_time_unix {
        Some(seconds) => seconds,
        None => now_unix()?,
    };
    let reservation = options.contents_reservation.max(MIN_CONTENTS_RESERVATION);
    let field_name = if options.field_name.is_empty() {
        DEFAULT_FIELD_NAME.to_owned()
    } else {
        options.field_name.clone()
    };

    // `allocate_object_number` only advances past objects already staged in
    // `Document::changes`, so each allocation must be followed by a
    // `set_object` before the next one, or both would return the same number
    // and the second would overwrite the first.
    let signature_object = ObjectId::new(document.allocate_object_number().map_err(core_error)?, 0);

    // The signature dictionary. `/ByteRange` must precede `/Contents` in the
    // serialized dictionary so the verifier can find the ranges first; the PDF
    // writer sorts keys, so this order is guaranteed.
    let mut signature = Dictionary::new();
    signature.insert("Type".to_owned(), Object::Name("Sig".to_owned()));
    signature.insert(
        "Filter".to_owned(),
        Object::Name("Adobe.PPKLite".to_owned()),
    );
    signature.insert(
        "SubFilter".to_owned(),
        Object::Name("adbe.pkcs7.detached".to_owned()),
    );
    signature.insert(
        "ByteRange".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(BYTE_RANGE_PLACEHOLDER),
            Object::Integer(BYTE_RANGE_PLACEHOLDER),
            Object::Integer(BYTE_RANGE_PLACEHOLDER),
        ]),
    );
    signature.insert(
        "Contents".to_owned(),
        Object::String(vec![0_u8; reservation]),
    );
    signature.insert(
        "M".to_owned(),
        Object::String(pdf_date(signing_time)?.into_bytes()),
    );
    signature.insert(
        "Name".to_owned(),
        Object::String(identity.signer_subject().into_bytes()),
    );
    if let Some(reason) = &options.reason {
        signature.insert(
            "Reason".to_owned(),
            Object::String(reason.clone().into_bytes()),
        );
    }
    if let Some(location) = &options.location {
        signature.insert(
            "Location".to_owned(),
            Object::String(location.clone().into_bytes()),
        );
    }
    document
        .set_object(signature_object, Object::Dictionary(signature))
        .map_err(core_error)?;

    // The widget number is allocated only after the signature dictionary is
    // staged, so the two object numbers are guaranteed to be distinct.
    let widget_object = ObjectId::new(document.allocate_object_number().map_err(core_error)?, 0);

    // An invisible widget: a zero rectangle and no appearance stream. The
    // signature is still listed in the signature panel and covers the file.
    let mut widget = Dictionary::new();
    widget.insert("Type".to_owned(), Object::Name("Annot".to_owned()));
    widget.insert("Subtype".to_owned(), Object::Name("Widget".to_owned()));
    widget.insert("FT".to_owned(), Object::Name("Sig".to_owned()));
    widget.insert(
        "T".to_owned(),
        Object::String(field_name.clone().into_bytes()),
    );
    widget.insert("F".to_owned(), Object::Integer(132));
    widget.insert(
        "Rect".to_owned(),
        Object::Array(vec![
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
            Object::Integer(0),
        ]),
    );
    widget.insert("P".to_owned(), Object::Reference(page.id));
    widget.insert("V".to_owned(), Object::Reference(signature_object));
    document
        .set_object(widget_object, Object::Dictionary(widget))
        .map_err(core_error)?;

    add_widget_to_page(document, page.id, &mut page_dictionary, widget_object)?;
    add_widget_to_acroform(document, widget_object)?;

    let original_len = document.len();
    let mut bytes = document.save_incremental().map_err(core_error)?;
    if bytes.len() <= original_len {
        return Err(SecurityError::SignatureDocumentInvalid(
            "incremental save appended no bytes".to_owned(),
        ));
    }

    let (_, contents_start, contents_end) =
        locate_own_signature(&bytes, original_len, reservation)?;
    // The real `/ByteRange`: everything before `<`, then everything after `>`.
    // The `<...>` window itself is the only excluded region.
    let byte_range = [0, contents_start, contents_end, bytes.len() - contents_end];
    patch_byte_range(&mut bytes, &byte_range)?;

    let digest = signed_digest(&bytes, byte_range);
    let cms = build_cms(identity, &digest, signing_time)?;
    if cms.len() > reservation {
        return Err(SecurityError::SignatureContentsTooSmall {
            reserved: reservation,
            required: cms.len(),
        });
    }
    write_contents(&mut bytes, contents_start, contents_end, &cms, reservation)?;

    let report = SignatureReport {
        byte_range,
        cms_len: cms.len(),
        contents_reservation: reservation,
        signer_subject: identity.signer_subject(),
        signing_time_unix: signing_time,
        digest_algorithm: DigestAlgorithm::Sha256,
        field_name,
        signature_object: signature_object.number,
        widget_object: widget_object.number,
        appended_bytes: bytes.len() - original_len,
    };
    Ok(SignedDocument { bytes, report })
}

/// Append `widget` to the page `/Annots` array, following a direct or indirect
/// array and staging only the objects that actually change.
fn add_widget_to_page(
    document: &mut Document,
    page_id: ObjectId,
    page_dictionary: &mut Dictionary,
    widget: ObjectId,
) -> Result<()> {
    let annotation = Object::Reference(widget);
    match page_dictionary.get("Annots").cloned() {
        None => {
            page_dictionary.insert("Annots".to_owned(), Object::Array(vec![annotation]));
            document
                .set_object(page_id, Object::Dictionary(page_dictionary.clone()))
                .map_err(core_error)?;
        }
        Some(Object::Array(mut items)) => {
            items.push(annotation);
            page_dictionary.insert("Annots".to_owned(), Object::Array(items));
            document
                .set_object(page_id, Object::Dictionary(page_dictionary.clone()))
                .map_err(core_error)?;
        }
        Some(Object::Reference(array_id)) => {
            let array_object = document.object(array_id).map_err(core_error)?;
            let Object::Array(mut items) = array_object else {
                return Err(SecurityError::SignatureFieldInvalid(
                    "page /Annots reference is not an array",
                ));
            };
            items.push(annotation);
            document
                .set_object(array_id, Object::Array(items))
                .map_err(core_error)?;
        }
        Some(_) => {
            return Err(SecurityError::SignatureFieldInvalid(
                "page /Annots is neither an array nor an array reference",
            ));
        }
    }
    Ok(())
}

/// Append `widget` to the catalog `/AcroForm /Fields` array and set
/// `/SigFlags 3` (SignaturesExist + AppendOnly).
fn add_widget_to_acroform(document: &mut Document, widget: ObjectId) -> Result<()> {
    let root = document.root().map_err(core_error)?;
    let catalog_object = document.object(root).map_err(core_error)?;
    let Object::Dictionary(mut catalog) = catalog_object else {
        return Err(SecurityError::SignatureFieldInvalid(
            "catalog is not a dictionary",
        ));
    };

    match catalog.get("AcroForm").cloned() {
        None => {
            let acroform_id =
                ObjectId::new(document.allocate_object_number().map_err(core_error)?, 0);
            let mut acroform = Dictionary::new();
            acroform.insert(
                "Fields".to_owned(),
                Object::Array(vec![Object::Reference(widget)]),
            );
            acroform.insert("SigFlags".to_owned(), Object::Integer(3));
            document
                .set_object(acroform_id, Object::Dictionary(acroform))
                .map_err(core_error)?;
            catalog.insert("AcroForm".to_owned(), Object::Reference(acroform_id));
            document
                .set_object(root, Object::Dictionary(catalog))
                .map_err(core_error)?;
        }
        Some(Object::Reference(acroform_id)) => {
            let acroform_object = document.object(acroform_id).map_err(core_error)?;
            let Object::Dictionary(mut acroform) = acroform_object else {
                return Err(SecurityError::SignatureFieldInvalid(
                    "catalog /AcroForm reference is not a dictionary",
                ));
            };
            append_field(&mut acroform, widget)?;
            document
                .set_object(acroform_id, Object::Dictionary(acroform))
                .map_err(core_error)?;
        }
        Some(Object::Dictionary(mut acroform)) => {
            append_field(&mut acroform, widget)?;
            catalog.insert("AcroForm".to_owned(), Object::Dictionary(acroform));
            document
                .set_object(root, Object::Dictionary(catalog))
                .map_err(core_error)?;
        }
        Some(_) => {
            return Err(SecurityError::SignatureFieldInvalid(
                "catalog /AcroForm is neither a dictionary nor a reference",
            ));
        }
    }
    Ok(())
}

/// Append one reference to an AcroForm `/Fields` array.
fn append_field(acroform: &mut Dictionary, widget: ObjectId) -> Result<()> {
    match acroform.get("Fields").cloned() {
        None => {
            acroform.insert(
                "Fields".to_owned(),
                Object::Array(vec![Object::Reference(widget)]),
            );
        }
        Some(Object::Array(mut items)) => {
            items.push(Object::Reference(widget));
            acroform.insert("Fields".to_owned(), Object::Array(items));
        }
        Some(_) => {
            return Err(SecurityError::SignatureFieldInvalid(
                "AcroForm /Fields is not a direct array",
            ));
        }
    }
    let sig_flags = acroform
        .get("SigFlags")
        .and_then(Object::as_i64)
        .unwrap_or(0);
    acroform.insert("SigFlags".to_owned(), Object::Integer(sig_flags | 3));
    Ok(())
}

/// Build the detached CMS `SignedData` blob for `content_digest`.
fn build_cms(
    identity: &SigningIdentity,
    content_digest: &[u8],
    signing_time: u64,
) -> Result<Vec<u8>> {
    let signer = identity.signer_certificate();
    let encapsulated = EncapsulatedContentInfo {
        econtent_type: rfc5911::ID_DATA,
        econtent: None,
    };
    let sid = SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
        issuer: signer.tbs_certificate.issuer.clone(),
        serial_number: signer.tbs_certificate.serial_number.clone(),
    });

    let mut signer_info = SignerInfoBuilder::new(
        &identity.key,
        sid,
        sha256_algorithm(),
        &encapsulated,
        Some(content_digest),
    )
    .map_err(|_| SecurityError::Configuration("CMS signer info could not be built".to_owned()))?;
    signer_info
        .add_signed_attribute(signing_time_attribute(signing_time)?)
        .map_err(|_| SecurityError::Configuration("CMS attribute could not be added".to_owned()))?;

    let mut builder = SignedDataBuilder::new(&encapsulated);
    builder
        .add_digest_algorithm(sha256_algorithm())
        .map_err(|_| SecurityError::Configuration("CMS digest algorithm rejected".to_owned()))?;
    for certificate in &identity.chain {
        builder
            .add_certificate(CertificateChoices::Certificate(certificate.clone()))
            .map_err(|_| SecurityError::Configuration("CMS certificate rejected".to_owned()))?;
    }
    builder
        .add_signer_info::<SigningKey<Sha256>, RsaRawSignature>(signer_info)
        .map_err(|_| SecurityError::Configuration("CMS signature failed".to_owned()))?;
    builder
        .build()
        .map_err(|_| SecurityError::Configuration("CMS SignedData build failed".to_owned()))?
        .to_der()
        .map_err(|_| der_error())
}

/// Locate this module's signature dictionary in the appended revision.
///
/// Returns the `/ByteRange` values, the offset of `<`, and the offset just
/// past `>` of the `/Contents` hex string.
fn locate_own_signature(
    bytes: &[u8],
    appended_from: usize,
    reservation: usize,
) -> Result<([usize; 4], usize, usize)> {
    let marker = b"/ByteRange";
    let position = (appended_from..bytes.len())
        .rev()
        .find(|&index| bytes[index..].starts_with(marker))
        .ok_or(SecurityError::SignatureDocumentInvalid(
            "the appended revision has no /ByteRange".to_owned(),
        ))?;
    let values = parse_byte_range(bytes, position + marker.len()).ok_or(
        SecurityError::SignatureDocumentInvalid("/ByteRange is not four integers".to_owned()),
    )?;

    let object_end = find_from(bytes, position, b"endobj").unwrap_or(bytes.len());
    let contents_key = find_range(bytes, position, object_end, b"/Contents").ok_or(
        SecurityError::SignatureDocumentInvalid(
            "the signature dictionary has no /Contents".to_owned(),
        ),
    )?;
    let mut cursor = skip_pdf_space(bytes, contents_key + b"/Contents".len());
    if bytes.get(cursor) != Some(&b'<') {
        return Err(SecurityError::SignatureDocumentInvalid(
            "/Contents is not a hex string".to_owned(),
        ));
    }
    let contents_start = cursor;
    cursor += 1;
    let contents_end = bytes[cursor..object_end]
        .iter()
        .position(|&byte| byte == b'>')
        .map(|offset| cursor + offset)
        .ok_or(SecurityError::SignatureDocumentInvalid(
            "/Contents hex string is not terminated".to_owned(),
        ))?;
    if contents_end + 1 - contents_start != 2 * reservation + 2 {
        return Err(SecurityError::SignatureDocumentInvalid(
            "/Contents length does not match the reservation".to_owned(),
        ));
    }
    if values[1] != BYTE_RANGE_PLACEHOLDER as usize
        || values[2] != BYTE_RANGE_PLACEHOLDER as usize
        || values[3] != BYTE_RANGE_PLACEHOLDER as usize
    {
        return Err(SecurityError::SignatureDocumentInvalid(
            "/ByteRange placeholder changed before signing".to_owned(),
        ));
    }
    Ok((values, contents_start, contents_end + 1))
}

/// Overwrite the three mutable `/ByteRange` fields without changing lengths.
///
/// Each field was serialized as a ten-digit placeholder; the final value is
/// right-aligned in the same ten bytes and padded with spaces, which is legal
/// PDF whitespace inside an array. Field 0 is always `0` and is left alone.
fn patch_byte_range(bytes: &mut [u8], values: &[usize; 4]) -> Result<()> {
    let marker = b"/ByteRange";
    let position = (0..bytes.len())
        .rev()
        .find(|&index| bytes[index..].starts_with(marker))
        .ok_or(SecurityError::SignatureDocumentInvalid(
            "/ByteRange disappeared before patching".to_owned(),
        ))?;
    let mut cursor = skip_pdf_space(bytes, position + marker.len());
    if bytes.get(cursor) != Some(&b'[') {
        return Err(SecurityError::SignatureDocumentInvalid(
            "/ByteRange is not an array".to_owned(),
        ));
    }
    cursor += 1;
    // Skip the immutable leading `0`.
    cursor = skip_pdf_space(bytes, cursor);
    let first_start = cursor;
    while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
        cursor += 1;
    }
    if first_start == cursor {
        return Err(SecurityError::SignatureDocumentInvalid(
            "/ByteRange has no leading offset".to_owned(),
        ));
    }
    for value in values.iter().skip(1) {
        cursor = skip_pdf_space(bytes, cursor);
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor - start != BYTE_RANGE_FIELD_WIDTH {
            return Err(SecurityError::SignatureDocumentInvalid(
                "/ByteRange placeholder has an unexpected width".to_owned(),
            ));
        }
        let text = format!("{value:>width$}", width = BYTE_RANGE_FIELD_WIDTH);
        if text.len() != BYTE_RANGE_FIELD_WIDTH {
            return Err(SecurityError::SignatureDocumentInvalid(
                "a byte-range offset exceeds the reserved field width".to_owned(),
            ));
        }
        bytes[start..cursor].copy_from_slice(text.as_bytes());
    }
    Ok(())
}

/// SHA-256 over the two `/ByteRange` ranges concatenated.
fn signed_digest(bytes: &[u8], byte_range: [usize; 4]) -> Vec<u8> {
    let mut hasher = Sha256::new();
    hasher.update(&bytes[..byte_range[1]]);
    hasher.update(&bytes[byte_range[2]..byte_range[2] + byte_range[3]]);
    hasher.finalize().to_vec()
}

/// Hex-write the CMS blob into the reserved `/Contents` window.
fn write_contents(
    bytes: &mut [u8],
    contents_start: usize,
    contents_end: usize,
    cms: &[u8],
    reservation: usize,
) -> Result<()> {
    let hex = hex_encode(cms);
    let window = contents_end - contents_start - 2;
    if hex.len() > window {
        return Err(SecurityError::SignatureContentsTooSmall {
            reserved: reservation,
            required: cms.len(),
        });
    }
    bytes[contents_start + 1..contents_start + 1 + hex.len()].copy_from_slice(hex.as_bytes());
    for byte in &mut bytes[contents_start + 1 + hex.len()..contents_end - 1] {
        *byte = b'0';
    }
    Ok(())
}

fn hex_encode(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(hex_digit(byte >> 4));
        out.push(hex_digit(byte & 0x0f));
    }
    out
}

fn hex_digit(nibble: u8) -> char {
    match nibble {
        0..=9 => char::from(b'0' + nibble),
        _ => char::from(b'A' + (nibble - 10)),
    }
}

fn hex_value(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

/// Verify the last signature in `pdf`.
///
/// The verifier scans raw bytes for the last `/ByteRange`, takes the
/// `/Contents` window from the declared ranges, trims the zero padding after
/// the DER value, and verifies the CMS signature under the certificate named
/// by the `SignerInfo`.
///
/// # Errors
///
/// - [`SecurityError::SignatureNotFound`] when no `/ByteRange` with a valid
///   `/Contents` window exists;
/// - DER, CMS, or algorithm errors when the blob is structurally invalid or
///   uses an algorithm this round does not support.
///
/// Tampering and signature failures are reported through
/// [`SignatureVerification::status`] rather than as errors: `Ok` means the
/// signature was parsed, not that it is valid. Check
/// [`SignatureVerification::is_fully_valid`] for the badge condition.
pub fn verify_signature(pdf: &[u8]) -> Result<SignatureVerification> {
    let (position, values) = locate_any_signature(pdf)?;
    let [_, contents_start, contents_end, remaining] = values;
    let hex = &pdf[contents_start + 1..contents_end - 1];
    let decoded = decode_hex(hex)?;
    let cms = trim_to_der(&decoded)?;

    let content_digest = signed_digest(pdf, values);
    let parsed = parse_cms(cms)?;

    let mut digest_matches = false;
    if let Some(message_digest) = parsed.message_digest.as_deref() {
        digest_matches = constant_time_eq(&content_digest, message_digest);
    }

    let signature_valid = match &parsed.signer_certificate {
        Some(certificate) => verify_rsa_signature(&parsed, certificate),
        None => false,
    };

    let covered_end = contents_end + remaining;
    let covers_entire_file = covered_end == pdf.len();
    let appended_after_signature = covered_end < pdf.len();

    let status = if !signature_valid {
        SignatureStatus::Invalid
    } else if !digest_matches {
        SignatureStatus::Tampered
    } else {
        SignatureStatus::Valid
    };

    let (signer_subject, fingerprint) = match &parsed.signer_certificate {
        Some(certificate) => {
            let der = certificate.to_der().map_err(|_| der_error())?;
            (
                certificate_subject_from_der(&der),
                Sha256::digest(&der).into(),
            )
        }
        None => ("<no signer certificate>".to_owned(), [0_u8; 32]),
    };

    Ok(SignatureVerification {
        status,
        signature_valid,
        bytes_match: digest_matches,
        covers_entire_file,
        appended_after_signature,
        signer_subject,
        signer_fingerprint_sha256: fingerprint,
        signing_time_unix: parsed.signing_time,
        digest_algorithm: parsed.digest_algorithm,
        byte_range: values,
        sub_filter: find_name_near(pdf, position).unwrap_or_else(|| "unknown".to_owned()),
        certificate_chain_trusted: None,
    })
}

/// Locate the last `/ByteRange` that has a plausible `/Contents` window.
fn locate_any_signature(pdf: &[u8]) -> Result<(usize, [usize; 4])> {
    let marker = b"/ByteRange";
    let mut best = None;
    for index in 0..pdf.len() {
        if !pdf[index..].starts_with(marker) {
            continue;
        }
        let Some(values) = parse_byte_range(pdf, index + marker.len()) else {
            continue;
        };
        let [start, contents_start, contents_end, remaining] = values;
        if start != 0
            || contents_end < contents_start + 2
            || contents_end > pdf.len()
            || pdf[contents_start] != b'<'
            || pdf[contents_end - 1] != b'>'
            || contents_end + remaining > pdf.len()
        {
            continue;
        }
        best = Some((index, values));
    }
    best.ok_or(SecurityError::SignatureNotFound)
}

/// Parse four non-negative integers from a PDF array at `from`.
fn parse_byte_range(bytes: &[u8], from: usize) -> Option<[usize; 4]> {
    let mut cursor = skip_pdf_space(bytes, from);
    if bytes.get(cursor) != Some(&b'[') {
        return None;
    }
    cursor += 1;
    let mut values = [0_usize; 4];
    for value in &mut values {
        cursor = skip_pdf_space(bytes, cursor);
        let start = cursor;
        while bytes.get(cursor).is_some_and(u8::is_ascii_digit) {
            cursor += 1;
        }
        if cursor == start || cursor - start > 19 {
            return None;
        }
        *value = std::str::from_utf8(&bytes[start..cursor])
            .ok()?
            .parse()
            .ok()?;
    }
    cursor = skip_pdf_space(bytes, cursor);
    if bytes.get(cursor) != Some(&b']') {
        return None;
    }
    Some(values)
}

fn skip_pdf_space(bytes: &[u8], mut cursor: usize) -> usize {
    while bytes
        .get(cursor)
        .is_some_and(|byte| matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' '))
    {
        cursor += 1;
    }
    cursor
}

fn find_from(bytes: &[u8], from: usize, needle: &[u8]) -> Option<usize> {
    if from >= bytes.len() || needle.is_empty() {
        return None;
    }
    bytes[from..]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

fn find_range(bytes: &[u8], from: usize, to: usize, needle: &[u8]) -> Option<usize> {
    let to = to.min(bytes.len());
    if from >= to || needle.is_empty() || needle.len() > to - from {
        return None;
    }
    bytes[from..to]
        .windows(needle.len())
        .position(|window| window == needle)
        .map(|offset| from + offset)
}

/// Find `/SubFilter /Name` inside the signature dictionary that starts near
/// `position`. The `/Contents` hex string sits between `/ByteRange` and
/// `/SubFilter`, so the search is bounded by the object's `endobj`, not by a
/// fixed window.
fn find_name_near(bytes: &[u8], position: usize) -> Option<String> {
    let start = position.saturating_sub(512);
    let end = find_from(bytes, position, b"endobj")
        .unwrap_or_else(|| (position + 65_536).min(bytes.len()));
    let key = find_range(bytes, start, end, b"/SubFilter")?;
    let mut cursor = skip_pdf_space(bytes, key + b"/SubFilter".len());
    if bytes.get(cursor) != Some(&b'/') {
        return None;
    }
    cursor += 1;
    let name_start = cursor;
    while bytes.get(cursor).is_some_and(|byte| {
        !matches!(
            byte,
            b' ' | b'\t' | b'\r' | b'\n' | b'/' | b'<' | b'>' | b'[' | b']' | b'(' | b')'
        )
    }) {
        cursor += 1;
    }
    if cursor == name_start {
        return None;
    }
    std::str::from_utf8(&bytes[name_start..cursor])
        .ok()
        .map(str::to_owned)
}

/// Decode a PDF hex string, tolerating embedded ASCII whitespace.
fn decode_hex(hex: &[u8]) -> Result<Vec<u8>> {
    let mut nibbles = Vec::with_capacity(hex.len());
    for &byte in hex {
        if matches!(byte, b'\0' | b'\t' | b'\n' | b'\x0c' | b'\r' | b' ') {
            continue;
        }
        nibbles.push(hex_value(byte).ok_or(SecurityError::SignatureInvalid)?);
    }
    if nibbles.len() % 2 != 0 {
        return Err(SecurityError::SignatureInvalid);
    }
    let mut out = Vec::with_capacity(nibbles.len() / 2);
    for pair in nibbles.as_chunks::<2>().0 {
        out.push((pair[0] << 4) | pair[1]);
    }
    Ok(out)
}

/// Trim zero padding after one complete DER value.
fn trim_to_der(bytes: &[u8]) -> Result<&[u8]> {
    if bytes.first() != Some(&0x30) {
        return Err(SecurityError::SignatureInvalid);
    }
    let first = *bytes.get(1).ok_or(SecurityError::DerTruncated)?;
    let (length, header) = if first & 0x80 == 0 {
        (usize::from(first), 2)
    } else {
        let count = usize::from(first & 0x7f);
        if count == 0 || count > 4 {
            return Err(SecurityError::DerInvalid);
        }
        let mut value = 0_usize;
        for index in 0..count {
            let byte = *bytes.get(2 + index).ok_or(SecurityError::DerTruncated)?;
            value = (value << 8) | usize::from(byte);
        }
        (value, 2 + count)
    };
    let total = header
        .checked_add(length)
        .ok_or(SecurityError::DerLengthOverflow)?;
    if total > bytes.len() {
        return Err(SecurityError::DerTruncated);
    }
    Ok(&bytes[..total])
}

struct ParsedCms {
    message_digest: Option<Vec<u8>>,
    signing_time: Option<u64>,
    digest_algorithm: DigestAlgorithm,
    signed_attributes_der: Vec<u8>,
    signature: Vec<u8>,
    signer_certificate: Option<X509Certificate>,
}

fn parse_cms(cms: &[u8]) -> Result<ParsedCms> {
    let content_info = ContentInfo::from_der(cms).map_err(|_| SecurityError::DerInvalid)?;
    if content_info.content_type != rfc5911::ID_SIGNED_DATA {
        return Err(SecurityError::DerInvalid);
    }
    let signed_data_der = content_info.content.to_der().map_err(|_| der_error())?;
    let signed_data =
        SignedData::from_der(&signed_data_der).map_err(|_| SecurityError::DerInvalid)?;
    if signed_data.signer_infos.0.len() != 1 {
        return Err(SecurityError::SignatureInvalid);
    }
    let signer_info = signed_data
        .signer_infos
        .0
        .iter()
        .next()
        .ok_or(SecurityError::SignatureInvalid)?;
    if signer_info.version != CmsVersion::V1 {
        return Err(SecurityError::SignatureInvalid);
    }
    if signer_info.digest_alg.oid != rfc5912::ID_SHA_256 {
        return Err(SecurityError::UnsupportedDigestAlgorithm(oid_text(
            &signer_info.digest_alg.oid,
        )));
    }
    if signer_info.signature_algorithm.oid != rfc5912::SHA_256_WITH_RSA_ENCRYPTION {
        return Err(SecurityError::UnsupportedSignatureAlgorithm(oid_text(
            &signer_info.signature_algorithm.oid,
        )));
    }

    let attributes = signer_info
        .signed_attrs
        .as_ref()
        .ok_or(SecurityError::MissingSignedAttribute("signedAttrs"))?;
    let signed_attributes_der = attributes.to_der().map_err(|_| der_error())?;

    let mut message_digest = None;
    let mut signing_time = None;
    let mut content_type = None;
    for attribute in attributes.iter() {
        let value = attribute
            .values
            .iter()
            .next()
            .ok_or(SecurityError::SignatureInvalid)?;
        let value_der = value.to_der().map_err(|_| der_error())?;
        if attribute.oid == rfc5911::ID_MESSAGE_DIGEST {
            message_digest = Some(
                OctetString::from_der(&value_der)
                    .map_err(|_| der_error())?
                    .as_bytes()
                    .to_vec(),
            );
        } else if attribute.oid == rfc5911::ID_SIGNING_TIME {
            signing_time = decode_time(&value_der).ok();
        } else if attribute.oid == rfc5911::ID_CONTENT_TYPE {
            content_type = der::asn1::ObjectIdentifier::from_der(&value_der).ok();
        }
    }
    if content_type != Some(rfc5911::ID_DATA) {
        return Err(SecurityError::MissingSignedAttribute("contentType"));
    }
    let message_digest =
        message_digest.ok_or(SecurityError::MissingSignedAttribute("messageDigest"))?;

    let signer_certificate = select_signer_certificate(&signed_data, &signer_info.sid)?;

    Ok(ParsedCms {
        message_digest: Some(message_digest),
        signing_time,
        digest_algorithm: DigestAlgorithm::Sha256,
        signed_attributes_der,
        signature: signer_info.signature.as_bytes().to_vec(),
        signer_certificate,
    })
}

fn select_signer_certificate(
    signed_data: &SignedData,
    sid: &SignerIdentifier,
) -> Result<Option<X509Certificate>> {
    let SignerIdentifier::IssuerAndSerialNumber(sid) = sid else {
        return Err(SecurityError::UnsupportedSignatureAlgorithm(
            "subjectKeyIdentifier signer id".to_owned(),
        ));
    };
    let Some(certificates) = &signed_data.certificates else {
        return Ok(None);
    };
    let issuer_der = sid.issuer.to_der().map_err(|_| der_error())?;
    for choice in certificates.0.iter() {
        let CertificateChoices::Certificate(certificate) = choice else {
            continue;
        };
        let candidate_issuer = certificate
            .tbs_certificate
            .issuer
            .to_der()
            .map_err(|_| der_error())?;
        if candidate_issuer == issuer_der
            && certificate.tbs_certificate.serial_number == sid.serial_number
        {
            return Ok(Some(certificate.clone()));
        }
    }
    Ok(None)
}

fn verify_rsa_signature(parsed: &ParsedCms, certificate: &X509Certificate) -> bool {
    let Ok(public_key) = RsaPublicKey::try_from(
        certificate
            .tbs_certificate
            .subject_public_key_info
            .owned_to_ref(),
    ) else {
        return false;
    };
    let mut hasher = Sha256::new();
    hasher.update(&parsed.signed_attributes_der);
    let digest = hasher.finalize();
    public_key
        .verify(Pkcs1v15Sign::new::<Sha256>(), &digest, &parsed.signature)
        .is_ok()
}

fn decode_time(der: &[u8]) -> Result<u64> {
    match der.first() {
        Some(0x17) => UtcTime::from_der(der)
            .map(|time| time.to_unix_duration().as_secs())
            .map_err(|_| der_error()),
        Some(0x18) => GeneralizedTime::from_der(der)
            .map(|time| time.to_unix_duration().as_secs())
            .map_err(|_| der_error()),
        _ => Err(der_error()),
    }
}

fn oid_text(oid: &der::asn1::ObjectIdentifier) -> String {
    oid.to_string()
}

/// Display the certificate subject through the crate's own X.509 name parser,
/// which is structural, not cryptographic.
fn certificate_subject(certificate: &X509Certificate) -> String {
    match certificate.to_der() {
        Ok(der) => certificate_subject_from_der(&der),
        Err(_) => "<unparsable subject>".to_owned(),
    }
}

fn certificate_subject_from_der(der: &[u8]) -> String {
    match crate::x509::Certificate::parse(der) {
        Ok(parsed) => parsed.subject().display_rfc4514(),
        Err(_) => "<unparsable subject>".to_owned(),
    }
}

#[cfg(test)]
mod tests {
    //! Real-crypto tests for T-0241. A test identity is generated at runtime
    //! (never committed) and used to sign the real Excel fixture; the
    //! independent interop check lives in
    //! `target/lanes/security/t0241-out/independent-check.ps1`.

    use std::path::PathBuf;
    use std::str::FromStr;
    use std::sync::OnceLock;

    use der::EncodePem;
    use der::pem::LineEnding;
    use rsa::pkcs8::EncodePrivateKey;
    use rsa::signature::{Keypair, SignatureEncoding, Signer};
    use spki::SubjectPublicKeyInfoOwned;
    use x509_cert::builder::{Builder as _, CertificateBuilder, Profile};
    use x509_cert::name::Name;
    use x509_cert::serial_number::SerialNumber;
    use x509_cert::time::{Time, Validity};

    use super::*;

    const FIXTURE: &str = "../../fixtures/realworld/microsoft-excel-2019-sample.pdf";
    const SIGNING_TIME: u64 = 1_700_000_000;

    struct IdentityPem {
        key: String,
        chain: String,
    }

    fn build_identity_pem(bits: usize, common_name: &str, serial: u32) -> IdentityPem {
        let mut rng = rand::rngs::OsRng;
        let key = RsaPrivateKey::new(&mut rng, bits).expect("generate test RSA key");
        let signer = SigningKey::<Sha256>::new(key.clone());
        let subject =
            Name::from_str(&format!("CN={common_name},O=PDFXI")).expect("test subject name");
        let spki = SubjectPublicKeyInfoOwned::from_key(signer.verifying_key())
            .expect("test public key info");
        let not_before = Time::UtcTime(
            UtcTime::from_unix_duration(Duration::from_secs(1_600_000_000)).expect("notBefore"),
        );
        let not_after = Time::UtcTime(
            UtcTime::from_unix_duration(Duration::from_secs(2_000_000_000)).expect("notAfter"),
        );
        let builder = CertificateBuilder::new(
            Profile::Leaf {
                issuer: subject.clone(),
                enable_key_agreement: false,
                enable_key_encipherment: false,
            },
            SerialNumber::from(serial),
            Validity {
                not_before,
                not_after,
            },
            subject,
            spki,
            &signer,
        )
        .expect("certificate builder");
        let certificate = builder
            .build::<RsaRawSignature>()
            .expect("self-signed test certificate");
        IdentityPem {
            key: key
                .to_pkcs8_pem(LineEnding::LF)
                .expect("encode test key")
                .to_string(),
            chain: certificate
                .to_pem(LineEnding::LF)
                .expect("encode test cert"),
        }
    }

    fn identity_pem() -> &'static IdentityPem {
        static ONCE: OnceLock<IdentityPem> = OnceLock::new();
        ONCE.get_or_init(|| build_identity_pem(2048, "PDFXI Test Signer", 1))
    }

    fn identity() -> SigningIdentity {
        let pem = identity_pem();
        SigningIdentity::from_pem(&pem.key, &pem.chain).expect("load test identity")
    }

    /// Deterministic signing options: a fixed clock makes `/M` and the CMS
    /// `signingTime` attribute reproducible.
    fn options() -> SignOptions {
        SignOptions {
            signing_time_unix: Some(SIGNING_TIME),
            ..SignOptions::default()
        }
    }

    fn fixture() -> Vec<u8> {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FIXTURE);
        std::fs::read(&path).expect("read Excel fixture")
    }

    fn artifact_dir() -> PathBuf {
        let path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/security/t0241-out");
        std::fs::create_dir_all(&path).expect("create artifact directory");
        path
    }

    fn cms_from_pdf(pdf: &[u8]) -> Vec<u8> {
        let (_, values) = locate_any_signature(pdf).expect("signed pdf has a signature");
        let hex = &pdf[values[1] + 1..values[2] - 1];
        let decoded = decode_hex(hex).expect("valid contents hex");
        trim_to_der(&decoded).expect("valid DER").to_vec()
    }

    fn replace_cms(pdf: &[u8], cms: &[u8]) -> Vec<u8> {
        let (_, values) = locate_any_signature(pdf).expect("signed pdf has a signature");
        let mut out = pdf.to_vec();
        let window = values[2] - values[1] - 2;
        assert!(cms.len() * 2 <= window, "CMS fits the reserved window");
        write_contents(&mut out, values[1], values[2], cms, window / 2).expect("rewrite contents");
        out
    }

    /// Build a CMS over `content_digest` whose signer identifier and embedded
    /// certificate name `claimed`, while the signature itself is produced by
    /// `signing_key`. This models a certificate-substitution attack.
    fn cms_with_claimed_certificate(
        signing_key: &SigningKey<Sha256>,
        claimed: &X509Certificate,
        content_digest: &[u8],
        signing_time: u64,
    ) -> Vec<u8> {
        let encapsulated = EncapsulatedContentInfo {
            econtent_type: rfc5911::ID_DATA,
            econtent: None,
        };
        let mut attributes = SetOfVec::<Attribute>::new();
        attributes
            .insert(
                cms::builder::create_content_type_attribute(rfc5911::ID_DATA)
                    .expect("content type attribute"),
            )
            .expect("insert content type");
        attributes
            .insert(
                cms::builder::create_message_digest_attribute(content_digest)
                    .expect("message digest attribute"),
            )
            .expect("insert message digest");
        attributes
            .insert(signing_time_attribute(signing_time).expect("signing time attribute"))
            .expect("insert signing time");
        let attributes_der = attributes.to_der().expect("signed attributes DER");
        let signature = signing_key.sign(&attributes_der);

        let signer_info = cms::signed_data::SignerInfo {
            version: CmsVersion::V1,
            sid: SignerIdentifier::IssuerAndSerialNumber(IssuerAndSerialNumber {
                issuer: claimed.tbs_certificate.issuer.clone(),
                serial_number: claimed.tbs_certificate.serial_number.clone(),
            }),
            digest_alg: sha256_algorithm(),
            signed_attrs: Some(attributes),
            signature_algorithm: AlgorithmIdentifierOwned {
                oid: rfc5912::SHA_256_WITH_RSA_ENCRYPTION,
                parameters: Some(der::Any::null()),
            },
            signature: OctetString::new(signature.to_bytes().as_ref()).expect("signature octets"),
            unsigned_attrs: None,
        };
        let certificates =
            cms::signed_data::CertificateSet::try_from(vec![CertificateChoices::Certificate(
                claimed.clone(),
            )])
            .expect("certificate set");
        let signed_data = SignedData {
            version: CmsVersion::V1,
            digest_algorithms: SetOfVec::try_from(vec![sha256_algorithm()])
                .expect("digest algorithms"),
            encap_content_info: encapsulated,
            certificates: Some(certificates),
            crls: None,
            signer_infos: cms::signed_data::SignerInfos::try_from(vec![signer_info])
                .expect("signer infos"),
        };
        let signed_data_der = signed_data.to_der().expect("signed data DER");
        let content = der::Any::from(
            der::AnyRef::try_from(signed_data_der.as_slice()).expect("signed data any"),
        );
        ContentInfo {
            content_type: rfc5911::ID_SIGNED_DATA,
            content,
        }
        .to_der()
        .expect("content info DER")
    }

    #[test]
    fn signs_a_real_pdf_and_verifies_the_cms() {
        let original = fixture();
        let mut document = Document::from_bytes(original.clone()).expect("parse fixture");
        let pages = document.page_count().expect("page count");
        let signed = sign_pdf(&mut document, &identity(), &options()).expect("sign fixture");

        assert_eq!(
            &signed.bytes()[..original.len()],
            original.as_slice(),
            "the original bytes must remain an exact prefix"
        );
        assert!(signed.report().cms_len() > 256);
        assert_eq!(signed.report().digest_algorithm(), DigestAlgorithm::Sha256);

        let verification = verify_signature(signed.bytes()).expect("verify signed pdf");
        assert_eq!(verification.status(), SignatureStatus::Valid);
        assert!(verification.signature_valid());
        assert!(verification.bytes_match());
        assert!(verification.covers_entire_file());
        assert!(!verification.appended_after_signature());
        assert!(verification.is_fully_valid());
        assert_eq!(verification.signing_time_unix(), Some(SIGNING_TIME));
        assert_eq!(verification.digest_algorithm(), DigestAlgorithm::Sha256);
        assert_eq!(verification.sub_filter(), "adbe.pkcs7.detached");
        assert_eq!(verification.certificate_chain_trusted(), None);
        assert!(
            verification.signer_subject().contains("PDFXI Test Signer"),
            "unexpected subject: {}",
            verification.signer_subject()
        );

        // The signed file is still a PDF that the core parser accepts.
        let reopened = Document::from_bytes(signed.bytes().to_vec()).expect("reopen signed pdf");
        assert_eq!(reopened.page_count().expect("page count"), pages);

        let dir = artifact_dir();
        let cms = cms_from_pdf(signed.bytes());
        std::fs::write(dir.join("signed.pdf"), signed.bytes()).expect("write signed pdf");
        std::fs::write(dir.join("signed.cms.der"), &cms).expect("write cms");
        let (_, values) = locate_any_signature(signed.bytes()).expect("signature");
        let mut detached = Vec::new();
        detached.extend_from_slice(&signed.bytes()[..values[1]]);
        detached.extend_from_slice(&signed.bytes()[values[2]..values[2] + values[3]]);
        std::fs::write(dir.join("signed.content.bin"), &detached).expect("write content");
        assert!(detached.len() < signed.bytes().len());
    }

    #[test]
    fn a_flipped_byte_inside_the_signed_range_is_reported_as_tampering() {
        let mut document = Document::from_bytes(fixture()).expect("parse fixture");
        let signed = sign_pdf(&mut document, &identity(), &options()).expect("sign fixture");
        let values = signed.report().byte_range();

        let mut tampered = signed.bytes().to_vec();
        let offset = values[1] / 2;
        tampered[offset] ^= 0x01;
        let verification = verify_signature(&tampered).expect("parse tampered pdf");
        assert!(verification.signature_valid());
        assert!(!verification.bytes_match());
        assert_eq!(verification.status(), SignatureStatus::Tampered);
        assert!(!verification.is_fully_valid());
    }

    #[test]
    fn appending_bytes_after_the_signature_is_reported_without_losing_validity() {
        let mut document = Document::from_bytes(fixture()).expect("parse fixture");
        let signed = sign_pdf(&mut document, &identity(), &options()).expect("sign fixture");
        let mut appended = signed.bytes().to_vec();
        appended.extend_from_slice(b"\n% PDFXI appended after signing\n");

        let verification = verify_signature(&appended).expect("verify appended pdf");
        assert!(verification.signature_valid());
        assert!(verification.bytes_match());
        assert!(!verification.covers_entire_file());
        assert!(verification.appended_after_signature());
        assert_eq!(verification.status(), SignatureStatus::Valid);
        assert!(!verification.is_fully_valid());
    }

    #[test]
    fn a_corrupted_signature_value_is_rejected() {
        let mut document = Document::from_bytes(fixture()).expect("parse fixture");
        let signed = sign_pdf(&mut document, &identity(), &options()).expect("sign fixture");
        let mut cms = cms_from_pdf(signed.bytes());
        let last = cms.len() - 1;
        cms[last] ^= 0x01;
        let forged = replace_cms(signed.bytes(), &cms);

        let verification = verify_signature(&forged).expect("parse forged pdf");
        assert!(!verification.signature_valid());
        assert_eq!(verification.status(), SignatureStatus::Invalid);
        assert!(!verification.is_fully_valid());
    }

    #[test]
    fn a_certificate_that_does_not_match_the_signature_is_rejected() {
        let mut document = Document::from_bytes(fixture()).expect("parse fixture");
        let signer = identity();
        let signed = sign_pdf(&mut document, &signer, &options()).expect("sign fixture");

        // A different, cheap-to-generate certificate is claimed by the
        // SignerInfo while the signature stays under the real signing key.
        let other_pem = build_identity_pem(1024, "PDFXI Wrong Signer", 2);
        let other_certificate = X509Certificate::load_pem_chain(other_pem.chain.as_bytes())
            .expect("parse other certificate")
            .remove(0);
        let (_, values) = locate_any_signature(signed.bytes()).expect("signature");
        let digest = signed_digest(signed.bytes(), values);
        let mismatched =
            cms_with_claimed_certificate(&signer.key, &other_certificate, &digest, SIGNING_TIME);
        let forged = replace_cms(signed.bytes(), &mismatched);

        let verification = verify_signature(&forged).expect("parse mismatched pdf");
        assert!(!verification.signature_valid());
        assert_eq!(verification.status(), SignatureStatus::Invalid);
        assert!(
            verification.signer_subject().contains("PDFXI Wrong Signer"),
            "unexpected subject: {}",
            verification.signer_subject()
        );
        assert!(!verification.is_fully_valid());
    }

    #[test]
    fn pem_loading_rejects_bad_and_undersized_keys() {
        let pem = identity_pem();
        let loaded = SigningIdentity::from_pem(&pem.key, &pem.chain).expect("load identity");
        assert_eq!(loaded.chain_len(), 1);
        assert!(loaded.signer_subject().contains("PDFXI Test Signer"));
        assert!(!format!("{loaded:?}").contains("BEGIN"));

        assert_eq!(
            SigningIdentity::from_pem("not a pem document", &pem.chain).map(|_| ()),
            Err(SecurityError::SigningIdentityInvalid(
                "private key is not an unencrypted PKCS#8 or PKCS#1 RSA PEM"
            ))
        );
        assert_eq!(
            SigningIdentity::from_pem(&pem.key, "not a certificate").map(|_| ()),
            Err(SecurityError::SigningIdentityInvalid(
                "certificate chain is not valid PEM"
            ))
        );

        let small_pem = build_identity_pem(1024, "PDFXI Small Key", 3);
        assert_eq!(
            SigningIdentity::from_pem(&small_pem.key, &small_pem.chain).map(|_| ()),
            Err(SecurityError::SigningKeyUnsupported(
                "RSA key is 1024 bits; at least 2048 are required".to_owned()
            ))
        );
    }

    #[test]
    fn a_pdf_without_a_signature_is_a_typed_error() {
        assert_eq!(
            verify_signature(&fixture()).map(|_| ()),
            Err(SecurityError::SignatureNotFound)
        );
    }

    #[test]
    fn reserved_space_that_is_too_small_is_a_typed_error() {
        let mut document = Document::from_bytes(fixture()).expect("parse fixture");
        let mut options = options();
        // The clamp forces 4096 bytes. A single 3072-bit certificate fits, so
        // the identity carries extra embedded certificates (as a real
        // intermediate chain would) to push the DER past the clamped minimum.
        // Signing must fail cleanly rather than truncate the CMS.
        let big = build_identity_pem(3072, "PDFXI Big Key", 4);
        let mut chain = big.chain.clone();
        for index in 0..8_u32 {
            let padding = build_identity_pem(
                1024,
                &format!("PDFXI Padding Certificate {index}"),
                100 + index,
            );
            chain.push_str(&padding.chain);
        }
        let identity = SigningIdentity::from_pem(&big.key, &chain).expect("load big key");
        assert_eq!(identity.chain_len(), 9);
        options.contents_reservation = 0;
        let result = sign_pdf(&mut document, &identity, &options);
        match result {
            Err(SecurityError::SignatureContentsTooSmall { reserved, required }) => {
                assert_eq!(reserved, MIN_CONTENTS_RESERVATION);
                assert!(required > reserved);
            }
            other => panic!("expected a contents overflow, got {other:?}"),
        }
    }
}
