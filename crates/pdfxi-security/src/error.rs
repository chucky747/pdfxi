//! Error types for the security crate.
//!
//! All security operations return [`Result`], whose error type is
//! [`SecurityError`]. Variants are intentionally specific so that callers can
//! distinguish a wrong password from a tampered document or a revoked
//! certificate without string matching. No documents contents or secrets are
//! ever embedded in an error value.

use std::fmt;

use crate::sanitization::SanitizationCategory;

/// A failure produced by a security operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SecurityError {
    /// The supplied password matched neither the user nor the owner password.
    WrongPassword,
    /// A password was required but none was supplied.
    MissingPassword,
    /// The requested encryption method is not supported by the active backend.
    UnsupportedMethod(String),
    /// A permission mask contained reserved or unknown bits.
    InvalidPermissionBits(u32),
    /// A requested key length did not match the encryption method.
    InvalidKeyLength(u16),
    /// The supplied password configuration described a conflicting state.
    ///
    /// The static label names the invariant that was violated; no password
    /// bytes are ever carried in an error value.
    PasswordConflict(&'static str),
    /// A permission mask contained mutually contradictory grants.
    ///
    /// For example, annotation or page assembly without modification, or
    /// high-quality printing without printing, describes an impossible state.
    /// The static label names the violated dependency; no document contents
    /// are carried in the error.
    PermissionConflict(&'static str),
    /// The caller was denied a named operation by the active authorization.
    ///
    /// The static label names the operation (see
    /// [`crate::authorization::Operation`]); no secrets are carried.
    PermissionDenied(&'static str),
    /// A request would widen permissions granted to a user/open session.
    ///
    /// In particular, an empty user password must never elevate a session to
    /// the owner role or grant permissions beyond the configured mask. The
    /// static label names the attempted widening.
    PermissionWideningRejected(&'static str),
    /// Encryption could not be completed.
    EncryptionFailed,
    /// Decryption failed (for example, invalid key or padding).
    DecryptionFailed,
    /// A document structure that the encrypting save path deliberately refuses.
    ///
    /// Signatures and linearized or cross-reference-stream layouts are out of
    /// scope for the encryption writer; the static message names the reason.
    UnsupportedDocumentStructure(&'static str),
    /// An encrypted document or its `/Encrypt` dictionary was malformed.
    EncryptedDocumentInvalid(String),
    /// A signature was structurally invalid or could not be parsed.
    SignatureInvalid,
    /// No `/ByteRange` signature with a usable `/Contents` window was found.
    SignatureNotFound,
    /// A PEM private key or certificate chain could not be loaded.
    ///
    /// The static label names the failing step; no key or certificate bytes
    /// are carried in the value.
    SigningIdentityInvalid(&'static str),
    /// The signing key is not usable for PDF signatures in this round.
    SigningKeyUnsupported(String),
    /// The reserved `/Contents` window is smaller than the CMS blob.
    SignatureContentsTooSmall {
        /// Hex-decoded bytes reserved in the placeholder.
        reserved: usize,
        /// Bytes the DER CMS blob needs.
        required: usize,
    },
    /// A PDF signature field, page annotation list, or AcroForm is unusable.
    SignatureFieldInvalid(&'static str),
    /// A core document operation failed while staging a signature.
    SignatureDocumentInvalid(String),
    /// Signed content was modified after the signature was created.
    TamperedContent,
    /// A certificate or signature had expired at validation time.
    SignatureExpired,
    /// A signing certificate was revoked by its issuer.
    CertificateRevoked,
    /// A CMS/DER input ended before a complete encoded value was available.
    DerTruncated,
    /// A DER input violated a canonical encoding rule (for example an indefinite
    /// length, a non-minimal integer, or an out-of-range date).
    DerInvalid,
    /// A declared DER length could not be represented or exceeded the input.
    DerLengthOverflow,
    /// A DER value had a different tag than the parser expected.
    ///
    /// Both identifier octets are reported, never the value's contents.
    DerUnexpectedTag {
        /// Identifier octet the parser expected.
        expected: u8,
        /// Identifier octet that was actually present.
        found: u8,
    },
    /// Bytes remained after a complete top-level DER value was parsed.
    DerTrailingBytes,
    /// The CMS digest algorithm is not supported for signing or verification.
    UnsupportedDigestAlgorithm(String),
    /// The CMS signature algorithm is not supported for verification.
    UnsupportedSignatureAlgorithm(String),
    /// A required signed attribute was absent from a `SignedData` signer info.
    MissingSignedAttribute(&'static str),
    /// The signer identifier did not match the supplied signing provider.
    SignerMismatch,
    /// The embedded certificate did not match the supplied signing provider.
    CertificateMismatch,
    /// A `SignedData` did not contain the expected signer certificate.
    CertificateMissing,
    /// An X.509 certificate used a version other than v1 or v3.
    CertificateVersionUnsupported(u64),
    /// The certificate's `notBefore` was later than the validation time.
    CertificateNotYetValid,
    /// The certificate's `notAfter` was earlier than the validation time.
    CertificateExpired,
    /// No issuer certificate could be found for a certificate in the chain.
    CertificateIssuerUnknown,
    /// The chain did not terminate at a caller-supplied trust anchor.
    CertificateUntrusted,
    /// The certificate chain exceeded the caller's maximum depth.
    CertificateChainTooLong,
    /// A certificate signature check was rejected by the caller's verifier.
    CertificateSignatureInvalid,
    /// A certificate's key usage did not permit the requested operation.
    CertificateKeyUsageViolation(&'static str),
    /// An issuing certificate was not marked as a CA in basic constraints.
    CertificateNotCa,
    /// A required extended key usage purpose was absent from the certificate.
    CertificateEkuMissing,
    /// A critical X.509 extension that this subset does not understand was present.
    CertificateUnknownCriticalExtension,
    /// A known extension carried a malformed payload.
    CertificateExtensionInvalid(&'static str),
    /// An optional `TBSCertificate` field appeared more than once.
    CertificateDuplicateField(&'static str),
    /// Two extensions in a certificate declared the same OID.
    CertificateDuplicateExtension,
    /// A digital ID with the same certificate or key handle already exists.
    DigitalIdDuplicate,
    /// No digital ID matched the requested lookup.
    DigitalIdNotFound,
    /// Exported metadata did not match the supplied certificate material.
    DigitalIdMetadataMismatch,
    /// A protector or test double was misconfigured.
    Configuration(String),
    /// A page number was zero; PDF page numbers are 1-based.
    InvalidPageNumber,
    /// A redaction rectangle had zero or negative area.
    EmptyRedactionRegion,
    /// A rectangle or page-bound coordinate was NaN or infinite.
    NonFiniteCoordinate,
    /// Page bounds had a non-finite or non-positive dimension.
    InvalidPageBounds,
    /// A redaction rectangle referenced a page absent from the supplied bounds.
    UnknownPage {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A redaction rectangle extended outside its declared page bounds.
    RedactionRegionOutOfBounds {
        /// The offending 1-based page number.
        page: u32,
    },
    /// A redaction text pattern was empty or whitespace-only.
    EmptyRedactionPattern,
    /// Strict redaction verification found residual content.
    RedactionVerificationFailed {
        /// Page whose text contained the residual.
        page: u32,
        /// Byte offset of the residual within that page's text.
        offset: usize,
    },
    /// Strict sanitization verification found a category still present.
    SanitizationVerificationFailed {
        /// Category that was still present after sanitization.
        category: SanitizationCategory,
    },
    /// An extended hidden-information class selected for removal is still
    /// present after sanitization.
    ///
    /// Carries only the stable class identifier, never document contents.
    HiddenInfoResidual(&'static str),
    /// A metadata field identifier (key, name, prefix, or local name) was empty.
    ScrubInvalidFieldIdentifier,
    /// An XMP namespace prefix was not a valid XML NCName.
    ///
    /// Used when a prefix is non-empty but contains characters that would make
    /// `to_xml` emit malformed markup (for example a quote, space, or colon).
    ScrubInvalidXmpPrefix,
    /// An XMP property local name was not a valid XML NCName.
    ///
    /// Used when a local name is non-empty but contains a colon or another
    /// character outside the accepted NCName subset.
    ScrubInvalidXmpLocalName,
    /// An XMP packet could not be parsed into the canonical structured subset.
    ScrubXmpMalformed,
    /// Strict scrub verification found a removed string still present.
    ScrubVerificationFailed {
        /// Category whose removed value was still present.
        category: crate::scrub::ScrubCategory,
        /// Identifier of the field whose removed value was found.
        target: String,
    },
    /// A CMS token did not carry the RFC 3161 `id-ct-TSTInfo` content type.
    TimestampTokenTypeInvalid,
    /// A `TSTInfo` used a version other than v1.
    TimestampVersionUnsupported(u64),
    /// A required `TSTInfo` field was absent from an otherwise well-formed value.
    TimestampFieldMissing(&'static str),
    /// A certificate-encryption envelope was built with no recipients.
    CertificateEncryptionNoRecipients,
    /// Two recipients in a certificate-encryption envelope shared a label or
    /// certificate identity.
    CertificateEncryptionDuplicateRecipient,
    /// No recipient in the envelope matched the requested label.
    CertificateEncryptionRecipientNotFound,
    /// A recipient index was outside the envelope's recipient list.
    CertificateEncryptionRecipientIndexOutOfRange {
        /// The requested zero-based index.
        index: usize,
    },
    /// A recipient certificate was not valid for key encapsulation.
    CertificateEncryptionInvalidCertificate,
    /// The caller's key-wrap provider failed to wrap a content-encryption key.
    CertificateEncryptionKeyWrapFailed,
    /// The caller's key-wrap provider failed to unwrap a content-encryption key.
    CertificateEncryptionKeyUnwrapFailed,
    /// The unwrapped key did not match the envelope's recipient commitment; the
    /// envelope was opened with the wrong recipient's key.
    CertificateEncryptionWrongRecipient,
    /// The envelope's ciphertext or key commitment was structurally invalid.
    CertificateEncryptionInvalidEnvelope,
    /// The envelope's ciphertext failed content decryption or integrity checks.
    CertificateEncryptionTamperedContent,
    /// The plaintext supplied for certificate encryption was empty.
    CertificateEncryptionEmptyPlaintext,
    /// The content-encryption key length did not match the selected cipher.
    CertificateEncryptionInvalidKeyLength,
    /// The caller was not granted a permission required for this operation.
    CertificateEncryptionPermissionDenied(&'static str),
    /// A supplied certificate revocation list could not be parsed or was
    /// internally inconsistent.
    CrlMalformed,
    /// The CRL's `thisUpdate` was later than its `nextUpdate`.
    CrlInvalidWindow,
    /// A CRL carried no signature, but a signature was required for verification.
    CrlSignatureMissing,
    /// An RFC 3394 AES key-wrap KEK was not 16, 24, or 32 bytes long.
    AesKwInvalidKekLength(usize),
    /// An RFC 3394 AES key-wrap input had an invalid length.
    AesKwInvalidKeyDataLength {
        /// Which input was invalid (`"key data"` or `"wrapped key"`).
        label: &'static str,
        /// The invalid length in bytes.
        len: usize,
    },
    /// The RFC 3394 integrity check register did not match `A6A6A6A6A6A6A6A6`.
    ///
    /// This indicates a wrong KEK or a modified wrapped key.
    AesKwIntegrityCheckFailed,
    /// The core document layer could not be read or written while applying a
    /// sanitizer plan.
    ///
    /// Only a structural reason is carried; document contents are never embedded
    /// in an error value.
    SanitizationCoreFailed(String),
    /// A security envelope was built without any recipients.
    EnvelopeNoRecipients,
    /// A security envelope was built around an empty payload.
    EnvelopeEmptyPayload,
    /// Two envelope recipients shared a label.
    EnvelopeDuplicateRecipient(String),
    /// No envelope recipient matched the requested label.
    EnvelopeRecipientNotFound(String),
    /// A serialized security envelope could not be decoded.
    EnvelopeMalformed(String),
    /// The envelope integrity tag did not match its contents (tampering).
    EnvelopeIntegrityFailed,
    /// This envelope operation requires a non-empty key and an authenticated
    /// envelope.
    ///
    /// Returned when a build/open call is given an empty key, when an
    /// authenticated envelope is opened without a key, or when an unkeyed
    /// envelope is opened with a key. Permissions from an unkeyed envelope are
    /// never trusted for an authenticated operation.
    EnvelopeAuthenticationRequired,
    /// The envelope keyed authentication tag did not match (wrong key or
    /// tampering).
    EnvelopeAuthenticationFailed,
    /// The caller was not granted a permission declared by the envelope.
    EnvelopePermissionDenied(&'static str),
    /// A selected-attachments envelope named no attachments.
    EnvelopeEmptyAttachmentSelection,
    /// Two selected attachments shared a name in an envelope scope.
    EnvelopeDuplicateAttachment(String),
    /// A protection policy had an empty name.
    PolicyEmptyName,
    /// Two protection policies shared a name.
    PolicyDuplicateName(String),
    /// No protection policy matched the requested name.
    PolicyNotFound(String),
    /// A protection policy was validated at or after its expiry time.
    PolicyExpired(String),
    /// The caller was denied an operation by a protection policy.
    PolicyPermissionDenied(&'static str),
    /// A protection policy named an unknown security handler.
    PolicyUnknownHandler(String),
    /// A protection policy's key length did not match its handler.
    PolicyInvalidKeyLength(u16),
    /// A protection policy carried no recipients.
    PolicyNoRecipients,
    /// Two recipients shared a label in a protection policy.
    PolicyDuplicateRecipient(String),
    /// A protection policy contained an empty recipient label.
    PolicyEmptyRecipient,
    /// A caller-supplied PKCS#8-style private key was structurally invalid.
    ///
    /// The static label names the offending field; key bytes are never carried.
    DigitalIdKeyMalformed(&'static str),
    /// A private key's algorithm OID did not match its certificate's public key.
    DigitalIdKeyAlgorithmMismatch,
    /// A password-protected digital-ID store envelope was structurally invalid.
    StoreEnvelopeMalformed(&'static str),
    /// A password-protected store envelope failed its integrity check.
    ///
    /// This is returned both for a wrong password and for tampered bytes; the two
    /// are deliberately indistinguishable so a guess cannot probe the store.
    StoreEnvelopeIntegrityFailed,
    /// A password-protected store envelope used an unsupported version.
    StoreEnvelopeVersionUnsupported(u8),
    /// A timestamp-authority token model field was invalid.
    TsaTokenInvalid(&'static str),
    /// A detached signature's supplied certificate did not match its signer.
    DetachedSignerMismatch,
    /// Protection was requested for bytes that already carry a `/Encrypt` entry.
    ///
    /// The caller must decrypt the document with [`crate::open_encrypted`] first;
    /// encrypting ciphertext again would corrupt the object graph.
    DocumentAlreadyEncrypted,
    /// Protection removal was requested for bytes that carry no `/Encrypt` entry.
    DocumentNotEncrypted,
    /// A full rewrite that protection or protection removal requires failed.
    ///
    /// The message names the failing stage and never contains document content
    /// or password material.
    DocumentRewriteFailed(String),
}

impl fmt::Display for SecurityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::WrongPassword => write!(f, "the supplied password is incorrect"),
            Self::MissingPassword => write!(f, "a password is required but none was supplied"),
            Self::UnsupportedMethod(method) => {
                write!(f, "unsupported encryption method: {method}")
            }
            Self::InvalidPermissionBits(bits) => {
                write!(f, "invalid permission bit mask: {bits:#010x}")
            }
            Self::InvalidKeyLength(bits) => {
                write!(f, "invalid key length: {bits} bits")
            }
            Self::PasswordConflict(reason) => {
                write!(f, "conflicting password configuration: {reason}")
            }
            Self::PermissionConflict(reason) => {
                write!(f, "conflicting permission grants: {reason}")
            }
            Self::PermissionDenied(operation) => {
                write!(f, "permission denied for operation: {operation}")
            }
            Self::PermissionWideningRejected(reason) => {
                write!(f, "permission widening rejected: {reason}")
            }
            Self::EncryptionFailed => write!(f, "encryption failed"),
            Self::DecryptionFailed => write!(f, "decryption failed"),
            Self::UnsupportedDocumentStructure(reason) => {
                write!(f, "document structure cannot be encrypted: {reason}")
            }
            Self::EncryptedDocumentInvalid(reason) => {
                write!(f, "encrypted document is malformed: {reason}")
            }
            Self::SignatureInvalid => write!(f, "signature is invalid"),
            Self::SignatureNotFound => write!(f, "no PDF signature was found"),
            Self::SigningIdentityInvalid(reason) => {
                write!(f, "signing identity could not be loaded: {reason}")
            }
            Self::SigningKeyUnsupported(reason) => {
                write!(f, "signing key is not supported: {reason}")
            }
            Self::SignatureContentsTooSmall { reserved, required } => write!(
                f,
                "reserved /Contents window of {reserved} bytes is smaller than the {required}-byte CMS blob"
            ),
            Self::SignatureFieldInvalid(reason) => {
                write!(f, "PDF signature field is unusable: {reason}")
            }
            Self::SignatureDocumentInvalid(reason) => {
                write!(f, "document could not be signed: {reason}")
            }
            Self::TamperedContent => write!(f, "signed content was tampered with"),
            Self::SignatureExpired => write!(f, "signature or certificate has expired"),
            Self::CertificateRevoked => write!(f, "signing certificate was revoked"),
            Self::DerTruncated => write!(f, "DER input was truncated"),
            Self::DerInvalid => write!(f, "DER input was not canonically encoded"),
            Self::DerLengthOverflow => write!(f, "DER length exceeded representable bounds"),
            Self::DerUnexpectedTag { expected, found } => {
                write!(
                    f,
                    "unexpected DER tag: expected {expected:#04x}, found {found:#04x}"
                )
            }
            Self::DerTrailingBytes => write!(f, "DER input had trailing bytes"),
            Self::UnsupportedDigestAlgorithm(algorithm) => {
                write!(f, "unsupported digest algorithm: {algorithm}")
            }
            Self::UnsupportedSignatureAlgorithm(algorithm) => {
                write!(f, "unsupported signature algorithm: {algorithm}")
            }
            Self::MissingSignedAttribute(name) => {
                write!(f, "missing required signed attribute: {name}")
            }
            Self::SignerMismatch => write!(f, "signer identifier did not match the provider"),
            Self::CertificateMismatch => {
                write!(f, "embedded certificate did not match the provider")
            }
            Self::CertificateMissing => write!(f, "signed data contained no signer certificate"),
            Self::CertificateVersionUnsupported(version) => {
                write!(f, "unsupported X.509 certificate version: {version}")
            }
            Self::CertificateNotYetValid => write!(f, "certificate is not yet valid"),
            Self::CertificateExpired => write!(f, "certificate has expired"),
            Self::CertificateIssuerUnknown => write!(f, "certificate issuer is unknown"),
            Self::CertificateUntrusted => write!(f, "certificate chain has no trusted anchor"),
            Self::CertificateChainTooLong => write!(f, "certificate chain is too long"),
            Self::CertificateSignatureInvalid => write!(f, "certificate signature is invalid"),
            Self::CertificateKeyUsageViolation(usage) => {
                write!(f, "certificate key usage forbids {usage}")
            }
            Self::CertificateNotCa => {
                write!(f, "issuing certificate is not marked as a CA")
            }
            Self::CertificateEkuMissing => {
                write!(f, "certificate lacks the required extended key usage")
            }
            Self::CertificateUnknownCriticalExtension => {
                write!(f, "certificate contains an unknown critical extension")
            }
            Self::CertificateExtensionInvalid(name) => {
                write!(f, "malformed X.509 extension: {name}")
            }
            Self::CertificateDuplicateField(name) => {
                write!(f, "duplicate X.509 TBS field: {name}")
            }
            Self::CertificateDuplicateExtension => {
                write!(f, "certificate contains duplicate extension OIDs")
            }
            Self::DigitalIdDuplicate => write!(f, "a digital ID with this identity already exists"),
            Self::DigitalIdNotFound => write!(f, "no matching digital ID was found"),
            Self::DigitalIdMetadataMismatch => {
                write!(f, "digital ID metadata does not match the certificate")
            }
            Self::Configuration(message) => write!(f, "security configuration error: {message}"),
            Self::InvalidPageNumber => write!(f, "page number must be at least 1"),
            Self::EmptyRedactionRegion => {
                write!(f, "redaction rectangle has zero or negative area")
            }
            Self::NonFiniteCoordinate => write!(f, "redaction coordinate was not finite"),
            Self::InvalidPageBounds => write!(f, "page bounds must be finite and positive"),
            Self::UnknownPage { page } => {
                write!(f, "page {page} is not present in the supplied page bounds")
            }
            Self::RedactionRegionOutOfBounds { page } => {
                write!(f, "redaction rectangle lies outside page {page}")
            }
            Self::EmptyRedactionPattern => write!(f, "redaction text pattern was empty"),
            Self::RedactionVerificationFailed { page, offset } => write!(
                f,
                "redacted content remains on page {page} at byte offset {offset}"
            ),
            Self::SanitizationVerificationFailed { category } => {
                write!(f, "sanitization category still present: {category}")
            }
            Self::HiddenInfoResidual(class) => {
                write!(f, "hidden-information class still present: {class}")
            }
            Self::ScrubInvalidFieldIdentifier => {
                write!(f, "metadata field identifier must not be empty")
            }
            Self::ScrubInvalidXmpPrefix => {
                write!(f, "XMP namespace prefix is not a valid XML NCName")
            }
            Self::ScrubInvalidXmpLocalName => {
                write!(f, "XMP property local name is not a valid XML NCName")
            }
            Self::ScrubXmpMalformed => write!(f, "XMP packet was not well-formed"),
            Self::ScrubVerificationFailed { category, target } => {
                write!(f, "scrub residual in {category} at {target}")
            }
            Self::TimestampTokenTypeInvalid => {
                write!(f, "CMS token is not an RFC 3161 timestamp token")
            }
            Self::TimestampVersionUnsupported(version) => {
                write!(f, "unsupported RFC 3161 TSTInfo version: {version}")
            }
            Self::TimestampFieldMissing(name) => {
                write!(f, "required TSTInfo field missing: {name}")
            }
            Self::CertificateEncryptionNoRecipients => {
                write!(f, "certificate encryption requires at least one recipient")
            }
            Self::CertificateEncryptionDuplicateRecipient => {
                write!(
                    f,
                    "a recipient label or certificate was listed more than once"
                )
            }
            Self::CertificateEncryptionRecipientNotFound => {
                write!(f, "no envelope recipient matched the requested label")
            }
            Self::CertificateEncryptionRecipientIndexOutOfRange { index } => {
                write!(f, "recipient index {index} is outside the envelope")
            }
            Self::CertificateEncryptionInvalidCertificate => {
                write!(
                    f,
                    "recipient certificate is not valid for key encapsulation"
                )
            }
            Self::CertificateEncryptionKeyWrapFailed => {
                write!(
                    f,
                    "key-wrap provider failed to wrap the content-encryption key"
                )
            }
            Self::CertificateEncryptionKeyUnwrapFailed => {
                write!(
                    f,
                    "key-wrap provider failed to unwrap the content-encryption key"
                )
            }
            Self::CertificateEncryptionWrongRecipient => {
                write!(
                    f,
                    "envelope key commitment failed: wrong recipient or wrong key"
                )
            }
            Self::CertificateEncryptionInvalidEnvelope => {
                write!(f, "certificate-encryption envelope is malformed")
            }
            Self::CertificateEncryptionTamperedContent => {
                write!(
                    f,
                    "certificate-encryption envelope content failed integrity checks"
                )
            }
            Self::CertificateEncryptionEmptyPlaintext => {
                write!(f, "certificate encryption requires non-empty plaintext")
            }
            Self::CertificateEncryptionInvalidKeyLength => {
                write!(f, "content-encryption key length does not match the cipher")
            }
            Self::CertificateEncryptionPermissionDenied(operation) => {
                write!(f, "permission denied for operation: {operation}")
            }
            Self::CrlMalformed => write!(f, "certificate revocation list is malformed"),
            Self::CrlInvalidWindow => write!(f, "CRL thisUpdate is later than nextUpdate"),
            Self::CrlSignatureMissing => write!(f, "CRL carries no signature to verify"),
            Self::AesKwInvalidKekLength(len) => {
                write!(
                    f,
                    "AES-KW key-encryption key length must be 16, 24, or 32 bytes (got {len})"
                )
            }
            Self::AesKwInvalidKeyDataLength { label, len } => {
                write!(
                    f,
                    "AES-KW {label} length {len} is invalid (must be a multiple of 8 and long enough)"
                )
            }
            Self::AesKwIntegrityCheckFailed => {
                write!(
                    f,
                    "AES-KW integrity check failed (wrong KEK or modified wrapped key)"
                )
            }
            Self::SanitizationCoreFailed(reason) => {
                write!(f, "core document could not be sanitized: {reason}")
            }
            Self::EnvelopeNoRecipients => {
                write!(f, "a security envelope requires at least one recipient")
            }
            Self::EnvelopeEmptyPayload => {
                write!(f, "a security envelope requires a non-empty payload")
            }
            Self::EnvelopeDuplicateRecipient(label) => {
                write!(f, "duplicate envelope recipient label: {label}")
            }
            Self::EnvelopeRecipientNotFound(label) => {
                write!(f, "no envelope recipient matched label: {label}")
            }
            Self::EnvelopeMalformed(reason) => {
                write!(f, "security envelope is malformed: {reason}")
            }
            Self::EnvelopeIntegrityFailed => {
                write!(
                    f,
                    "security envelope integrity tag did not match its contents"
                )
            }
            Self::EnvelopeAuthenticationRequired => {
                write!(
                    f,
                    "security envelope authentication requires a key and an authenticated envelope"
                )
            }
            Self::EnvelopeAuthenticationFailed => {
                write!(
                    f,
                    "security envelope keyed authentication tag did not match"
                )
            }
            Self::EnvelopePermissionDenied(operation) => {
                write!(f, "envelope permission denied for operation: {operation}")
            }
            Self::EnvelopeEmptyAttachmentSelection => {
                write!(
                    f,
                    "selected-attachments envelope must name at least one attachment"
                )
            }
            Self::EnvelopeDuplicateAttachment(name) => {
                write!(f, "duplicate attachment name in envelope scope: {name}")
            }
            Self::PolicyEmptyName => write!(f, "protection policy name must not be empty"),
            Self::PolicyDuplicateName(name) => {
                write!(f, "duplicate protection policy name: {name}")
            }
            Self::PolicyNotFound(name) => {
                write!(f, "no protection policy matched name: {name}")
            }
            Self::PolicyExpired(name) => {
                write!(f, "protection policy '{name}' has expired")
            }
            Self::PolicyPermissionDenied(operation) => {
                write!(f, "protection policy denied operation: {operation}")
            }
            Self::PolicyUnknownHandler(handler) => {
                write!(
                    f,
                    "unknown security handler in protection policy: {handler}"
                )
            }
            Self::PolicyInvalidKeyLength(bits) => {
                write!(
                    f,
                    "protection policy key length is invalid for its handler: {bits} bits"
                )
            }
            Self::PolicyNoRecipients => {
                write!(f, "a protection policy requires at least one recipient")
            }
            Self::PolicyDuplicateRecipient(label) => {
                write!(f, "duplicate recipient in protection policy: {label}")
            }
            Self::PolicyEmptyRecipient => {
                write!(f, "protection policy recipient label must not be empty")
            }
            Self::DigitalIdKeyMalformed(field) => {
                write!(f, "malformed PKCS#8 private key field: {field}")
            }
            Self::DigitalIdKeyAlgorithmMismatch => {
                write!(f, "private key algorithm does not match the certificate")
            }
            Self::StoreEnvelopeMalformed(field) => {
                write!(f, "digital-ID store envelope is malformed: {field}")
            }
            Self::StoreEnvelopeIntegrityFailed => {
                write!(f, "digital-ID store envelope integrity check failed")
            }
            Self::StoreEnvelopeVersionUnsupported(version) => {
                write!(
                    f,
                    "unsupported digital-ID store envelope version: {version}"
                )
            }
            Self::TsaTokenInvalid(field) => {
                write!(f, "timestamp-authority token field is invalid: {field}")
            }
            Self::DetachedSignerMismatch => {
                write!(
                    f,
                    "detached signature signer does not match the supplied certificate"
                )
            }
            Self::DocumentAlreadyEncrypted => {
                write!(f, "the document is already encrypted")
            }
            Self::DocumentNotEncrypted => write!(f, "the document is not encrypted"),
            Self::DocumentRewriteFailed(reason) => {
                write!(f, "document rewrite failed: {reason}")
            }
        }
    }
}

impl std::error::Error for SecurityError {}

/// Convenience result alias for operations that may fail with [`SecurityError`].
pub type Result<T> = core::result::Result<T, SecurityError>;
