//! Electronic signature fields and local signing workflows.
//!
//! This module models the *field and workflow* side of electronic signatures.
//! It deliberately performs **no cryptography**: it never hashes bytes, never
//! builds a `/Contents` value that looks like a real signature, and never
//! claims to validate a cryptographic signature. Cryptographic signing and
//! verification belong to `pdfxi-security`; this crate only supplies the
//! deterministic object-model and state-machine primitives a caller needs.
//!
//! Three concerns are separated:
//!
//! - [`SignatureField`] describes a terminal `/FT /Sig` field and its widget.
//!   It emits a `/V` signature-dictionary *placeholder* carrying the signer
//!   metadata (`/Name`, `/Reason`, `/Location`, `/ContactInfo`, `/M`), an
//!   optional [`SignatureLock`] (`/Lock`) and an optional [`Certification`]
//!   (`/Reference` DocMDP transform). The `/Ff` flags carry the required bit.
//! - [`SignatureWorkflow`] is a deterministic local state machine
//!   `unsigned -> pending -> signed -> valid | invalid | expired`, with
//!   explicit `decline` and `cancel` branches. All timestamps and participant
//!   identities are caller-supplied opaque strings; this module never reads
//!   the system clock. Every accepted transition is recorded in
//!   [`Transition`] history, and [`WorkflowReport`] is a stable snapshot.
//! - [`SignatureSlot`] is the byte-range planning boundary reserved for a
//!   future signer. It exposes a fixed placeholder size, a hex-zero
//!   `/Contents` placeholder and enough metadata to compute the eventual
//!   `/ByteRange` once the file layout is known. It never forges a signature.
//!
//! Everything here is dependency-free apart from `thiserror` (already used by
//! this crate) and the crate's own appearance/object-model modules. All output
//! is deterministic: dictionaries use a [`BTreeMap`], reports use a fixed field
//! order, and no wall-clock or random source is consulted.

use crate::acroform::{ObjectRef, PdfValue};
use crate::appearance::{
    AppearanceDictionary, AppearanceError, WidgetAppearance, WidgetRect, build_appearance,
};
use crate::field::{Field, FieldFlags, FieldType, FieldValue};
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// Default `/Filter` for a newly created signature dictionary.
pub const DEFAULT_FILTER: SigFilter = SigFilter::PpkLite;
/// Default `/SubFilter` for a newly created signature dictionary.
pub const DEFAULT_SUBFILTER: SubFilter = SubFilter::AdbePkcs7Detached;
/// Default number of hex characters reserved for a `/Contents` placeholder.
pub const DEFAULT_PLACEHOLDER_SIZE: usize = 8192;
/// Largest accepted `/Contents` placeholder, in hex characters (1 MiB).
///
/// Bounding this value prevents a hostile caller from forcing an unbounded
/// allocation when a [`SignatureSlot`] is created.
pub const MAX_PLACEHOLDER_SIZE: usize = 1 << 20;

/// The PDF `/Filter` name identifying the signature handler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SigFilter {
    /// `Adobe.PPKLite` — the common crypt filter for detached PKCS#7.
    PpkLite,
    /// `Adobe.PPKMS` — the public-key messaging filter.
    PpkMs,
    /// `Adobe.PubSec` — the older public-security handler.
    PubSec,
}

impl SigFilter {
    /// Returns the PDF name for this filter, without the leading slash.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PpkLite => "Adobe.PPKLite",
            Self::PpkMs => "Adobe.PPKMS",
            Self::PubSec => "Adobe.PubSec",
        }
    }
}

impl fmt::Display for SigFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The PDF `/SubFilter` name describing the signature encoding.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SubFilter {
    /// `adbe.pkcs7.detached` — detached CMS/PKCS#7 signature.
    AdbePkcs7Detached,
    /// `ETSI.CAdES.detached` — detached CAdES signature.
    EtsiCadesDetached,
    /// `adbe.x509.rsa_sha1` — legacy RSA SHA-1 signature.
    AdbeX509RsaSha1,
}

impl SubFilter {
    /// Returns the PDF name for this subfilter, without the leading slash.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AdbePkcs7Detached => "adbe.pkcs7.detached",
            Self::EtsiCadesDetached => "ETSI.CAdES.detached",
            Self::AdbeX509RsaSha1 => "adbe.x509.rsa_sha1",
        }
    }
}

impl fmt::Display for SubFilter {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The `/Action` of a signature lock dictionary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LockAction {
    /// Only the named fields are locked (`/Include`).
    Include,
    /// All fields except the named ones are locked (`/Exclude`).
    Exclude,
}

impl LockAction {
    /// Returns the PDF name for this action.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Include => "Include",
            Self::Exclude => "Exclude",
        }
    }
}

/// The certification permission level, emitted as the DocMDP `/P` value.
///
/// Per ISO 32000-1 Table 254, the DocMDP transform parameter `/P` is an
/// enumerated value with exactly three legal values, not a free-form bit
/// field: `1` (no changes permitted), `2` (form fill-in and signing) and `3`
/// (form fill-in, signing and annotation changes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CertPermission {
    /// No changes to the document are permitted (`/P 1`).
    NoChanges,
    /// Form fill-in and signing are permitted (`/P 2`).
    FormAndSigning,
    /// Form fill-in, signing and annotation changes are permitted (`/P 3`).
    AnnotationsAndSigning,
}

impl CertPermission {
    /// Returns the exact integer `/P` value defined by ISO 32000-1 Table 254.
    pub const fn bits(self) -> u32 {
        match self {
            Self::NoChanges => 1,
            Self::FormAndSigning => 2,
            Self::AnnotationsAndSigning => 3,
        }
    }
}

/// The role a workflow participant plays in a signature.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParticipantRole {
    /// The person applying the signature.
    Signer,
    /// A person approving the document.
    Approver,
    /// A witness to the signing.
    Witness,
    /// The recipient of the signed document.
    Recipient,
}

impl ParticipantRole {
    /// Returns a stable lowercase label for this role.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Signer => "signer",
            Self::Approver => "approver",
            Self::Witness => "witness",
            Self::Recipient => "recipient",
        }
    }
}

impl fmt::Display for ParticipantRole {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The state of a local signature workflow.
///
/// The happy path is [`Unsigned`](Self::Unsigned) →
/// [`Pending`](Self::Pending) → [`Signed`](Self::Signed) →
/// [`Valid`](Self::Valid). Failure and expiry branch from `Signed` (and
/// `Valid` may later expire); [`Declined`](Self::Declined) and
/// [`Cancelled`](Self::Cancelled) are terminal alternatives from the early
/// states.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SignatureStatus {
    /// No signing has started.
    Unsigned,
    /// A signer has been invited but has not signed.
    Pending,
    /// The signer has applied a signature; cryptographic validation is pending.
    Signed,
    /// The signature was validated successfully.
    Valid,
    /// The signature failed validation.
    Invalid,
    /// The signature or the invitation has expired.
    Expired,
    /// The signer declined to sign.
    Declined,
    /// The workflow was cancelled before signing completed.
    Cancelled,
}

impl SignatureStatus {
    /// Returns `true` when no further transition is permitted from `self`.
    pub const fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Invalid | Self::Expired | Self::Declined | Self::Cancelled
        )
    }

    /// Returns `true` when the transition `self -> next` is allowed.
    ///
    /// This is the single source of truth for the workflow state machine and
    /// is used by [`SignatureWorkflow`] to reject illegal transitions.
    pub const fn can_transition_to(self, next: Self) -> bool {
        use SignatureStatus::*;
        matches!(
            (self, next),
            (Unsigned, Pending)
                | (Unsigned, Signed)
                | (Unsigned, Declined)
                | (Unsigned, Cancelled)
                | (Pending, Signed)
                | (Pending, Declined)
                | (Pending, Cancelled)
                | (Signed, Valid)
                | (Signed, Invalid)
                | (Signed, Expired)
                | (Valid, Expired)
        )
    }
}

impl fmt::Display for SignatureStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Unsigned => "unsigned",
            Self::Pending => "pending",
            Self::Signed => "signed",
            Self::Valid => "valid",
            Self::Invalid => "invalid",
            Self::Expired => "expired",
            Self::Declined => "declined",
            Self::Cancelled => "cancelled",
        };
        f.write_str(label)
    }
}

/// The kind of a recorded workflow transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TransitionKind {
    /// A signer was invited (`Unsigned -> Pending`).
    Invite,
    /// A signature was applied (`Unsigned|Pending -> Signed`).
    Sign,
    /// The signer declined (`Unsigned|Pending -> Declined`).
    Decline,
    /// The workflow was cancelled (`Unsigned|Pending -> Cancelled`).
    Cancel,
    /// Validation succeeded (`Signed -> Valid`).
    Validate,
    /// Validation failed (`Signed -> Invalid`).
    Invalidate,
    /// The workflow expired (`Signed|Valid -> Expired`).
    Expire,
}

impl TransitionKind {
    /// Returns a stable lowercase label for this transition kind.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Invite => "invite",
            Self::Sign => "sign",
            Self::Decline => "decline",
            Self::Cancel => "cancel",
            Self::Validate => "validate",
            Self::Invalidate => "invalidate",
            Self::Expire => "expire",
        }
    }
}

/// A signature lock dictionary (`/Lock`) model.
///
/// A lock binds a set of fields so they cannot change after signing. The
/// emitted dictionary contains `/Type /Sig`, `/Action`, `/Fields` and an
/// optional `/SubFilter`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureLock {
    action: LockAction,
    fields: Vec<String>,
    subfilter: Option<String>,
}

impl SignatureLock {
    /// Creates a lock over `fields` with the given `action`.
    pub fn new<I, S>(action: LockAction, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        Self {
            action,
            fields: fields.into_iter().map(Into::into).collect(),
            subfilter: None,
        }
    }

    /// Sets the lock dictionary's `/SubFilter` entry.
    #[must_use]
    pub fn with_subfilter(mut self, subfilter: impl Into<String>) -> Self {
        self.subfilter = Some(subfilter.into());
        self
    }

    /// Returns the lock action.
    pub const fn action(&self) -> LockAction {
        self.action
    }

    /// Returns the locked field names.
    pub fn fields(&self) -> &[String] {
        &self.fields
    }

    /// Emits the lock as a [`PdfValue::Dictionary`].
    pub fn to_pdf(&self) -> PdfValue {
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Sig".to_owned()));
        dict.insert(
            "Action".to_owned(),
            PdfValue::Name(self.action.as_str().to_owned()),
        );
        dict.insert(
            "Fields".to_owned(),
            PdfValue::Array(self.fields.iter().cloned().map(PdfValue::Text).collect()),
        );
        if let Some(subfilter) = &self.subfilter {
            dict.insert("SubFilter".to_owned(), PdfValue::Name(subfilter.clone()));
        }
        PdfValue::Dictionary(dict)
    }
}

/// The scope of a signature lock enforced on subsequent form fills.
///
/// ISO 32000-1 defines the `/Lock` dictionary's `/Action` as `Include` or
/// `Exclude` over a set of fields. At the *workflow* level the effective scope
/// additionally distinguishes locking every field from locking the document as
/// a whole; this enum makes those three cases explicit so a fill workflow can
/// enforce them without guessing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum LockScope {
    /// Only the explicitly named fields are locked.
    Fields,
    /// Every field is locked; `/Exclude` names stay fillable.
    All,
    /// The whole document is locked; no field may be filled.
    Document,
}

impl LockScope {
    /// Returns a stable lowercase label for this scope.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fields => "fields",
            Self::All => "all",
            Self::Document => "document",
        }
    }

    /// Returns the scope with the stronger restriction.
    ///
    /// `Document` is stronger than `All`, which is stronger than `Fields`.
    pub const fn stricter(self, other: Self) -> Self {
        if (self as u8) >= (other as u8) {
            self
        } else {
            other
        }
    }
}

impl fmt::Display for LockScope {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// A typed reason why a fill was refused by a signature lock or certification.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum SignatureGuardError {
    /// The named field is locked by a signature.
    #[error("field '{0}' is locked by a signature and cannot be filled")]
    FieldLocked(String),
    /// The document is locked by a signature.
    #[error("the document is locked by a signature and cannot be filled")]
    DocumentLocked,
    /// A certification level forbids all document changes.
    #[error("the document certification forbids all changes to form fields")]
    CertificationLocked,
}

/// A deterministic lock and certification guard for a fill workflow.
///
/// A guard is derived from the signature fields of a document (their `/Lock`
/// dictionaries and DocMDP `/Reference` entries) and is installed into a
/// [`crate::FillWorkflow`]. Every subsequent `set`/`clear` is checked against
/// it. The guard performs no cryptography: it only decides whether a field may
/// be filled, based on caller-supplied lock and certification metadata.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureGuard {
    scope: LockScope,
    locked: BTreeSet<String>,
    excluded: BTreeSet<String>,
    certification: Option<CertPermission>,
}

impl SignatureGuard {
    /// Creates a guard with `scope` and an optional certification level and no
    /// named fields.
    pub fn new(scope: LockScope, certification: Option<CertPermission>) -> Self {
        Self {
            scope,
            locked: BTreeSet::new(),
            excluded: BTreeSet::new(),
            certification,
        }
    }

    /// Adds fields that are locked under [`LockScope::Fields`].
    #[must_use]
    pub fn with_locked_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.locked.extend(fields.into_iter().map(Into::into));
        self
    }

    /// Adds fields that remain fillable under [`LockScope::All`] (`/Exclude`).
    #[must_use]
    pub fn with_excluded_fields<I, S>(mut self, fields: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.excluded.extend(fields.into_iter().map(Into::into));
        self
    }

    /// Derives a guard from a PDF [`SignatureLock`] and certification level.
    ///
    /// `Include` maps to [`LockScope::Fields`] with the named fields locked;
    /// `Exclude` maps to [`LockScope::All`] with the named fields exempt.
    pub fn from_lock(lock: &SignatureLock, certification: Option<CertPermission>) -> Self {
        match lock.action() {
            LockAction::Include => Self::new(LockScope::Fields, certification)
                .with_locked_fields(lock.fields().iter().cloned()),
            LockAction::Exclude => Self::new(LockScope::All, certification)
                .with_excluded_fields(lock.fields().iter().cloned()),
        }
    }

    /// Returns the effective lock scope.
    pub const fn scope(&self) -> LockScope {
        self.scope
    }

    /// Returns the certification level, if any.
    pub const fn certification(&self) -> Option<CertPermission> {
        self.certification
    }

    /// Returns the fields locked under [`LockScope::Fields`], in name order.
    pub fn locked_fields(&self) -> impl ExactSizeIterator<Item = &str> {
        self.locked.iter().map(String::as_str)
    }

    /// Returns the fields exempt under [`LockScope::All`], in name order.
    pub fn excluded_fields(&self) -> impl ExactSizeIterator<Item = &str> {
        self.excluded.iter().map(String::as_str)
    }

    /// Returns `Ok(())` when `field` may be filled under this guard.
    ///
    /// A `NoChanges` certification blocks every fill. Otherwise `Document`
    /// blocks every field, `All` blocks every field except exempt ones, and
    /// `Fields` blocks only explicitly named fields.
    ///
    /// # Errors
    ///
    /// Returns [`SignatureGuardError`] describing why the fill is refused.
    pub fn check_fill(&self, field: &str) -> Result<(), SignatureGuardError> {
        if self.certification == Some(CertPermission::NoChanges) {
            return Err(SignatureGuardError::CertificationLocked);
        }
        match self.scope {
            LockScope::Document => Err(SignatureGuardError::DocumentLocked),
            LockScope::All => {
                if self.excluded.contains(field) {
                    Ok(())
                } else {
                    Err(SignatureGuardError::FieldLocked(field.to_owned()))
                }
            }
            LockScope::Fields => {
                if self.locked.contains(field) {
                    Err(SignatureGuardError::FieldLocked(field.to_owned()))
                } else {
                    Ok(())
                }
            }
        }
    }

    /// Returns stable report lines describing this guard.
    pub fn report_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("lock.scope={}", self.scope),
            format!(
                "lock.certification={}",
                self.certification
                    .map(|permission| permission.bits().to_string())
                    .unwrap_or_else(|| "none".to_owned())
            ),
        ];
        for field in &self.locked {
            lines.push(format!("lock.field={field}"));
        }
        for field in &self.excluded {
            lines.push(format!("lock.excluded={field}"));
        }
        lines
    }
}

/// A DocMDP certification, emitted as a `/Reference` entry in `/V`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Certification {
    permission: CertPermission,
}

impl Certification {
    /// Creates a certification with the given permission level.
    pub const fn new(permission: CertPermission) -> Self {
        Self { permission }
    }

    /// Returns the certification permission level.
    pub const fn permission(&self) -> CertPermission {
        self.permission
    }

    /// Emits the `/Reference` array with one DocMDP transform.
    pub fn reference_array(&self) -> PdfValue {
        let mut transform_params = BTreeMap::new();
        transform_params.insert(
            "Type".to_owned(),
            PdfValue::Name("TransformParams".to_owned()),
        );
        transform_params.insert(
            "P".to_owned(),
            PdfValue::Number(f64::from(self.permission.bits())),
        );
        transform_params.insert("V".to_owned(), PdfValue::Number(1.2));

        let mut reference = BTreeMap::new();
        reference.insert(
            "TransformMethod".to_owned(),
            PdfValue::Name("DocMDP".to_owned()),
        );
        reference.insert(
            "TransformParams".to_owned(),
            PdfValue::Dictionary(transform_params),
        );
        PdfValue::Array(vec![PdfValue::Dictionary(reference)])
    }
}

/// A `/V` signature dictionary *placeholder*.
///
/// It holds only signer metadata and configuration. The cryptographic payload
/// (`/Contents`) and `/ByteRange` are intentionally absent: those are supplied
/// later by a security-layer signer using a [`SignatureSlot`].
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureDictionary {
    filter: SigFilter,
    subfilter: SubFilter,
    name: Option<String>,
    reason: Option<String>,
    location: Option<String>,
    contact: Option<String>,
    signing_time: Option<String>,
    lock: Option<SignatureLock>,
    certification: Option<Certification>,
}

impl SignatureDictionary {
    /// Creates a placeholder with the default filter and subfilter.
    pub fn new() -> Self {
        Self {
            filter: DEFAULT_FILTER,
            subfilter: DEFAULT_SUBFILTER,
            name: None,
            reason: None,
            location: None,
            contact: None,
            signing_time: None,
            lock: None,
            certification: None,
        }
    }

    /// Sets the `/Filter`.
    #[must_use]
    pub const fn with_filter(mut self, filter: SigFilter) -> Self {
        self.filter = filter;
        self
    }

    /// Sets the `/SubFilter`.
    #[must_use]
    pub const fn with_subfilter(mut self, subfilter: SubFilter) -> Self {
        self.subfilter = subfilter;
        self
    }

    /// Sets the signer `/Name`.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.name = Some(name.into());
        self
    }

    /// Sets the signing `/Reason`.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.reason = Some(reason.into());
        self
    }

    /// Sets the signing `/Location`.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.location = Some(location.into());
        self
    }

    /// Sets the `/ContactInfo`.
    #[must_use]
    pub fn with_contact(mut self, contact: impl Into<String>) -> Self {
        self.contact = Some(contact.into());
        self
    }

    /// Sets the signing `/M` time as a caller-supplied opaque string.
    #[must_use]
    pub fn with_signing_time(mut self, time: impl Into<String>) -> Self {
        self.signing_time = Some(time.into());
        self
    }

    /// Sets the lock dictionary.
    #[must_use]
    pub fn with_lock(mut self, lock: SignatureLock) -> Self {
        self.lock = Some(lock);
        self
    }

    /// Sets the DocMDP certification.
    #[must_use]
    pub const fn with_certification(mut self, certification: Certification) -> Self {
        self.certification = Some(certification);
        self
    }

    /// Returns the signer name, if set.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// Returns the signing reason, if set.
    pub fn reason(&self) -> Option<&str> {
        self.reason.as_deref()
    }

    /// Returns the signing location, if set.
    pub fn location(&self) -> Option<&str> {
        self.location.as_deref()
    }

    /// Returns the contact information, if set.
    pub fn contact(&self) -> Option<&str> {
        self.contact.as_deref()
    }

    /// Emits the placeholder signature dictionary.
    pub fn to_pdf(&self) -> PdfValue {
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Sig".to_owned()));
        dict.insert(
            "Filter".to_owned(),
            PdfValue::Name(self.filter.as_str().to_owned()),
        );
        dict.insert(
            "SubFilter".to_owned(),
            PdfValue::Name(self.subfilter.as_str().to_owned()),
        );
        if let Some(name) = &self.name {
            dict.insert("Name".to_owned(), PdfValue::Text(name.clone()));
        }
        if let Some(reason) = &self.reason {
            dict.insert("Reason".to_owned(), PdfValue::Text(reason.clone()));
        }
        if let Some(location) = &self.location {
            dict.insert("Location".to_owned(), PdfValue::Text(location.clone()));
        }
        if let Some(contact) = &self.contact {
            dict.insert("ContactInfo".to_owned(), PdfValue::Text(contact.clone()));
        }
        if let Some(time) = &self.signing_time {
            dict.insert("M".to_owned(), PdfValue::Text(time.clone()));
        }
        if let Some(lock) = &self.lock {
            dict.insert("Lock".to_owned(), lock.to_pdf());
        }
        if let Some(certification) = &self.certification {
            dict.insert("Reference".to_owned(), certification.reference_array());
        }
        PdfValue::Dictionary(dict)
    }
}

impl Default for SignatureDictionary {
    fn default() -> Self {
        Self::new()
    }
}

/// A typed error produced while creating or emitting a [`SignatureField`] or a
/// [`SignatureSlot`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SignatureError {
    /// The signature field name was empty.
    #[error("signature field name must not be empty")]
    EmptyName,
    /// The widget rectangle contained a non-finite coordinate.
    #[error("signature widget rectangle has a non-finite coordinate")]
    NonFiniteRect,
    /// The widget rectangle had a zero or negative extent.
    #[error("signature widget rectangle has a zero or negative extent")]
    ZeroSizedRect,
    /// The page index could not be mapped to a PDF object number.
    #[error("page index {0} cannot be mapped to a PDF object number")]
    PageOverflow(u32),
    /// The `/Contents` placeholder size was zero.
    #[error("signature placeholder size must be greater than zero")]
    InvalidPlaceholderSize,
    /// The `/Contents` placeholder size was not an even number of hex digits.
    #[error("signature placeholder size {0} is not an even number of hex digits")]
    OddPlaceholderSize(usize),
    /// The `/Contents` placeholder size exceeded the configured maximum.
    #[error("signature placeholder size {size} exceeds the maximum of {max}")]
    PlaceholderTooLarge {
        /// The requested size.
        size: usize,
        /// The maximum accepted size.
        max: usize,
    },
    /// Appearance generation failed.
    #[error(transparent)]
    Appearance(#[from] AppearanceError),
}

/// A terminal `/FT /Sig` signature field with its widget and metadata.
///
/// Build with [`SignatureField::new`] and the `with_*` methods, then emit a
/// field dictionary ([`SignatureField::field_dictionary`]), an in-memory
/// [`Field`] ([`SignatureField::to_field`]), or a signature-line appearance
/// ([`SignatureField::appearance`]).
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureField {
    name: String,
    rect: WidgetRect,
    page: u32,
    flags: FieldFlags,
    dictionary: SignatureDictionary,
}

impl SignatureField {
    /// Creates a signature field on `page` at `rect` with default metadata.
    pub fn new(name: impl Into<String>, rect: WidgetRect, page: u32) -> Self {
        Self {
            name: name.into(),
            rect,
            page,
            flags: FieldFlags::NONE,
            dictionary: SignatureDictionary::new(),
        }
    }

    /// Marks the field required (sets `/Ff` bit 1).
    #[must_use]
    pub fn required(mut self) -> Self {
        self.flags = self.flags.union(FieldFlags::REQUIRED);
        self
    }

    /// Sets the raw `/Ff` flags, replacing the current set.
    #[must_use]
    pub const fn with_flags(mut self, flags: FieldFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Adds flags to the current `/Ff` set.
    #[must_use]
    pub const fn with_added_flags(mut self, flags: FieldFlags) -> Self {
        self.flags = self.flags.union(flags);
        self
    }

    /// Sets the signer `/Name`.
    #[must_use]
    pub fn with_name(mut self, name: impl Into<String>) -> Self {
        self.dictionary = self.dictionary.with_name(name);
        self
    }

    /// Sets the signing `/Reason`.
    #[must_use]
    pub fn with_reason(mut self, reason: impl Into<String>) -> Self {
        self.dictionary = self.dictionary.with_reason(reason);
        self
    }

    /// Sets the signing `/Location`.
    #[must_use]
    pub fn with_location(mut self, location: impl Into<String>) -> Self {
        self.dictionary = self.dictionary.with_location(location);
        self
    }

    /// Sets the `/ContactInfo`.
    #[must_use]
    pub fn with_contact(mut self, contact: impl Into<String>) -> Self {
        self.dictionary = self.dictionary.with_contact(contact);
        self
    }

    /// Sets the caller-supplied signing `/M` timestamp.
    #[must_use]
    pub fn with_signing_time(mut self, time: impl Into<String>) -> Self {
        self.dictionary = self.dictionary.with_signing_time(time);
        self
    }

    /// Sets the `/Filter`.
    #[must_use]
    pub fn with_filter(mut self, filter: SigFilter) -> Self {
        self.dictionary = self.dictionary.with_filter(filter);
        self
    }

    /// Sets the `/SubFilter`.
    #[must_use]
    pub fn with_subfilter(mut self, subfilter: SubFilter) -> Self {
        self.dictionary = self.dictionary.with_subfilter(subfilter);
        self
    }

    /// Attaches a lock dictionary.
    #[must_use]
    pub fn with_lock(mut self, lock: SignatureLock) -> Self {
        self.dictionary = self.dictionary.with_lock(lock);
        self
    }

    /// Attaches a DocMDP certification.
    #[must_use]
    pub fn with_certification(mut self, certification: Certification) -> Self {
        self.dictionary = self.dictionary.with_certification(certification);
        self
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the widget rectangle.
    pub const fn rect(&self) -> WidgetRect {
        self.rect
    }

    /// Returns the zero-based owning page index.
    pub const fn page(&self) -> u32 {
        self.page
    }

    /// Returns the `/Ff` flags.
    pub const fn flags(&self) -> FieldFlags {
        self.flags
    }

    /// Returns `true` when the required flag is set.
    pub fn is_required(&self) -> bool {
        self.flags.is_required()
    }

    /// Returns the signing reason, if set.
    pub fn reason(&self) -> Option<&str> {
        self.dictionary.reason()
    }

    /// Returns the signing location, if set.
    pub fn location(&self) -> Option<&str> {
        self.dictionary.location()
    }

    /// Returns the contact information, if set.
    pub fn contact(&self) -> Option<&str> {
        self.dictionary.contact()
    }

    /// Returns the signature lock dictionary, if configured.
    pub fn lock(&self) -> Option<&SignatureLock> {
        self.dictionary.lock.as_ref()
    }

    /// Returns the DocMDP certification, if configured.
    pub const fn certification(&self) -> Option<Certification> {
        self.dictionary.certification
    }

    /// Validates the field's name, geometry and page index.
    ///
    /// # Errors
    ///
    /// Returns a [`SignatureError`] for an empty name, non-finite or zero-sized
    /// rectangle, or an unmappable page index.
    pub fn validate(&self) -> Result<(), SignatureError> {
        if self.name.is_empty() {
            return Err(SignatureError::EmptyName);
        }
        if !self.rect.is_finite() || !self.rect.right().is_finite() || !self.rect.top().is_finite()
        {
            return Err(SignatureError::NonFiniteRect);
        }
        if self.rect.is_zero_sized() {
            return Err(SignatureError::ZeroSizedRect);
        }
        if self.page.checked_add(1).is_none() {
            return Err(SignatureError::PageOverflow(self.page));
        }
        Ok(())
    }

    /// Builds the in-memory [`Field`] described by this signature field.
    ///
    /// The resulting field has type [`FieldType::Signature`]; its value is the
    /// signer name as [`FieldValue::Text`] when configured, otherwise empty.
    ///
    /// # Errors
    ///
    /// Returns a [`SignatureError`] when [`SignatureField::validate`] fails.
    pub fn to_field(&self) -> Result<Field, SignatureError> {
        self.validate()?;
        let value = self
            .dictionary
            .name()
            .map(|name| FieldValue::Text(name.to_owned()))
            .unwrap_or(FieldValue::Empty);
        Ok(Field::new(self.name.clone(), FieldType::Signature)
            .with_flags(self.flags)
            .with_value(value))
    }

    /// Returns the configured `/V` signature dictionary placeholder.
    pub fn signature_dictionary(&self) -> PdfValue {
        self.dictionary.to_pdf()
    }

    /// Builds a deterministic, human-readable signature line.
    ///
    /// The line always begins with `Digitally signed by <name>` (or
    /// `Digitally signed` when no name is set) and appends `Reason:` and
    /// `Location:` clauses in that fixed order when configured.
    pub fn signature_line(&self) -> String {
        let mut parts = Vec::new();
        match self.dictionary.name() {
            Some(name) => parts.push(format!("Digitally signed by {name}")),
            None => parts.push("Digitally signed".to_owned()),
        }
        if let Some(reason) = self.dictionary.reason() {
            parts.push(format!("Reason: {reason}"));
        }
        if let Some(location) = self.dictionary.location() {
            parts.push(format!("Location: {location}"));
        }
        parts.join("; ")
    }

    /// Generates the signature-line appearance for this field.
    ///
    /// The appearance is produced by the crate's deterministic
    /// [`build_appearance`] using a [`FieldType::Signature`] field and
    /// [`SignatureField::signature_line`] as the rendered value.
    ///
    /// # Errors
    ///
    /// Returns [`SignatureError::Appearance`] when `widget` fails validation.
    pub fn appearance(
        &self,
        widget: &WidgetAppearance,
    ) -> Result<AppearanceDictionary, SignatureError> {
        let field = Field::new(self.name.clone(), FieldType::Signature).with_flags(self.flags);
        let value = FieldValue::Text(self.signature_line());
        Ok(build_appearance(&field, widget, &value)?)
    }

    /// Emits the field-node dictionary for this signature field.
    ///
    /// Contains `/Type /Annot`, `/FT /Sig`, `/T`, `/Ff`, the `/V` placeholder
    /// from [`SignatureField::signature_dictionary`] and a single widget in
    /// `/Kids`. The page reference in the widget is a placeholder derived from
    /// the zero-based page index; a host writer must replace it with the real
    /// page object reference.
    ///
    /// # Errors
    ///
    /// Returns a [`SignatureError`] when [`SignatureField::validate`] fails.
    pub fn field_dictionary(&self) -> Result<PdfValue, SignatureError> {
        self.validate()?;
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
        dict.insert("FT".to_owned(), PdfValue::Name("Sig".to_owned()));
        dict.insert("T".to_owned(), PdfValue::Text(self.name.clone()));
        dict.insert(
            "Ff".to_owned(),
            PdfValue::Number(f64::from(self.flags.bits())),
        );
        dict.insert("V".to_owned(), self.signature_dictionary());
        dict.insert(
            "Kids".to_owned(),
            PdfValue::Array(vec![self.widget_dictionary()?]),
        );
        Ok(PdfValue::Dictionary(dict))
    }

    fn widget_dictionary(&self) -> Result<PdfValue, SignatureError> {
        let mut dict = BTreeMap::new();
        dict.insert("Type".to_owned(), PdfValue::Name("Annot".to_owned()));
        dict.insert("Subtype".to_owned(), PdfValue::Name("Widget".to_owned()));
        dict.insert(
            "Rect".to_owned(),
            PdfValue::Array(vec![
                PdfValue::Number(f64::from(self.rect.x)),
                PdfValue::Number(f64::from(self.rect.y)),
                PdfValue::Number(f64::from(self.rect.right())),
                PdfValue::Number(f64::from(self.rect.top())),
            ]),
        );
        dict.insert("F".to_owned(), PdfValue::Number(4.0));
        let page_object = self
            .page
            .checked_add(1)
            .ok_or(SignatureError::PageOverflow(self.page))?;
        dict.insert(
            "P".to_owned(),
            PdfValue::Reference(ObjectRef::new(page_object, 0)),
        );
        Ok(PdfValue::Dictionary(dict))
    }
}

/// A planned `/ByteRange` for a signature placeholder.
///
/// A PDF `/ByteRange` has the form `[a b c d]`: sign the bytes `[a, a+b)` and
/// `[c, c+d)`, leaving the `/Contents` hex string in the gap. Because the final
/// file length is generally unknown until the document is written,
/// [`SignatureSlot::plan`] leaves `total_length` unset; a caller that knows the
/// final length can use [`SignatureSlot::plan_with_total`] to obtain a complete
/// four-element array via [`ByteRangePlan::as_array`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ByteRangePlan {
    /// Start of the first signed range (always zero).
    pub first_start: u64,
    /// Length of the first signed range (the offset of the `/Contents` string).
    pub first_length: u64,
    /// Start of the second signed range (just after the closing `>`).
    pub second_start: u64,
    /// Total file length, when known.
    pub total_length: Option<u64>,
}

impl ByteRangePlan {
    /// Returns the length of the second signed range when the total file
    /// length is known.
    pub fn second_length(&self) -> Option<u64> {
        self.total_length
            .map(|total| total.saturating_sub(self.second_start))
    }

    /// Returns the complete `[a b c d]` array when the total file length is
    /// known, otherwise `None`.
    pub fn as_array(&self) -> Option<[u64; 4]> {
        Some([
            self.first_start,
            self.first_length,
            self.second_start,
            self.second_length()?,
        ])
    }
}

/// A planning slot for a future signature `/Contents` placeholder.
///
/// The slot reserves an even number of hex characters (filled with `'0'`) and
/// computes where the two signed byte ranges fall relative to that gap. It
/// does **not** produce cryptographic material: the hex zeros are a structural
/// placeholder only, and the security layer must replace them with a real
/// signature before the ranges are used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureSlot {
    placeholder_size: usize,
    contents_placeholder: Vec<u8>,
}

impl SignatureSlot {
    /// Creates a slot reserving `placeholder_size` hex characters.
    ///
    /// The size must be positive, even and no greater than
    /// [`MAX_PLACEHOLDER_SIZE`].
    ///
    /// # Errors
    ///
    /// Returns a [`SignatureError`] for an invalid placeholder size.
    pub fn new(placeholder_size: usize) -> Result<Self, SignatureError> {
        if placeholder_size == 0 {
            return Err(SignatureError::InvalidPlaceholderSize);
        }
        if !placeholder_size.is_multiple_of(2) {
            return Err(SignatureError::OddPlaceholderSize(placeholder_size));
        }
        if placeholder_size > MAX_PLACEHOLDER_SIZE {
            return Err(SignatureError::PlaceholderTooLarge {
                size: placeholder_size,
                max: MAX_PLACEHOLDER_SIZE,
            });
        }
        Ok(Self {
            placeholder_size,
            contents_placeholder: vec![b'0'; placeholder_size],
        })
    }

    /// Creates a slot with the [`DEFAULT_PLACEHOLDER_SIZE`].
    ///
    /// # Errors
    ///
    /// Returns a [`SignatureError`] if the default is invalid (it is not in a
    /// correct build, but the signature keeps the constructor total).
    pub fn default_size() -> Result<Self, SignatureError> {
        Self::new(DEFAULT_PLACEHOLDER_SIZE)
    }

    /// Returns the number of hex characters inside `/Contents <...>`.
    pub const fn placeholder_size(&self) -> usize {
        self.placeholder_size
    }

    /// Returns the total number of bytes the placeholder occupies, including
    /// the opening `<` and closing `>` delimiters.
    pub fn reserved_bytes(&self) -> u64 {
        self.placeholder_size as u64 + 2
    }

    /// Returns the hex-zero `/Contents` placeholder bytes.
    ///
    /// The slice length is exactly [`SignatureSlot::placeholder_size`].
    pub fn contents_placeholder(&self) -> &[u8] {
        &self.contents_placeholder
    }

    /// Plans the `/ByteRange` given the file offset where `/Contents` begins.
    pub fn plan(&self, contents_offset: u64) -> ByteRangePlan {
        ByteRangePlan {
            first_start: 0,
            first_length: contents_offset,
            second_start: contents_offset.saturating_add(self.reserved_bytes()),
            total_length: None,
        }
    }

    /// Plans the `/ByteRange` given the `/Contents` offset and the final file
    /// length, producing a plan with a complete four-element array.
    pub fn plan_with_total(&self, contents_offset: u64, total_length: u64) -> ByteRangePlan {
        let mut plan = self.plan(contents_offset);
        plan.total_length = Some(total_length);
        plan
    }
}

/// A participant in a local signing workflow.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Participant {
    name: String,
    email: Option<String>,
    role: ParticipantRole,
}

impl Participant {
    /// Creates a participant with the given name and role.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::EmptyName`] when `name` is empty.
    pub fn new(name: impl Into<String>, role: ParticipantRole) -> Result<Self, WorkflowError> {
        let name = name.into();
        if name.is_empty() {
            return Err(WorkflowError::EmptyName);
        }
        Ok(Self {
            name,
            email: None,
            role,
        })
    }

    /// Sets the participant's email address.
    #[must_use]
    pub fn with_email(mut self, email: impl Into<String>) -> Self {
        self.email = Some(email.into());
        self
    }

    /// Returns the participant's name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the participant's email, if set.
    pub fn email(&self) -> Option<&str> {
        self.email.as_deref()
    }

    /// Returns the participant's role.
    pub const fn role(&self) -> ParticipantRole {
        self.role
    }
}

/// A typed error produced by a [`SignatureWorkflow`] operation.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum WorkflowError {
    /// The workflow field name was empty.
    #[error("workflow field name must not be empty")]
    EmptyField,
    /// A participant name was empty.
    #[error("participant name must not be empty")]
    EmptyName,
    /// A caller-supplied timestamp was empty or whitespace.
    #[error("workflow timestamp must not be empty")]
    EmptyTimestamp,
    /// A transition was not allowed by the state machine.
    #[error("invalid workflow transition: {from} -> {to}")]
    InvalidTransition {
        /// The current state.
        from: SignatureStatus,
        /// The requested state.
        to: SignatureStatus,
    },
    /// An operation required a participant but none was recorded.
    #[error("workflow requires a participant for this operation")]
    MissingParticipant,
}

/// One recorded workflow transition.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    kind: TransitionKind,
    from: SignatureStatus,
    to: SignatureStatus,
    timestamp: String,
    note: Option<String>,
}

impl Transition {
    fn new(
        kind: TransitionKind,
        from: SignatureStatus,
        to: SignatureStatus,
        timestamp: &str,
        note: Option<&str>,
    ) -> Self {
        Self {
            kind,
            from,
            to,
            timestamp: timestamp.to_owned(),
            note: note.map(str::to_owned),
        }
    }

    /// Returns the transition kind.
    pub const fn kind(&self) -> TransitionKind {
        self.kind
    }

    /// Returns the state before the transition.
    pub const fn from(&self) -> SignatureStatus {
        self.from
    }

    /// Returns the state after the transition.
    pub const fn to(&self) -> SignatureStatus {
        self.to
    }

    /// Returns the caller-supplied timestamp.
    pub fn timestamp(&self) -> &str {
        &self.timestamp
    }

    /// Returns the optional note, if any.
    pub fn note(&self) -> Option<&str> {
        self.note.as_deref()
    }
}

/// A deterministic local signature workflow state machine.
///
/// All timestamps are opaque, caller-supplied strings; no system clock is
/// read. Every accepted transition is appended to [`SignatureWorkflow::history`]
/// and the current state is updated. Illegal transitions return
/// [`WorkflowError::InvalidTransition`] and leave the workflow unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignatureWorkflow {
    field: String,
    state: SignatureStatus,
    participant: Option<Participant>,
    reason: Option<String>,
    location: Option<String>,
    contact: Option<String>,
    invited_at: Option<String>,
    signed_at: Option<String>,
    expires_at: Option<String>,
    history: Vec<Transition>,
}

impl SignatureWorkflow {
    /// Creates a workflow for `field` in the [`SignatureStatus::Unsigned`]
    /// state.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::EmptyField`] when `field` is empty.
    pub fn new(field: impl Into<String>) -> Result<Self, WorkflowError> {
        let field = field.into();
        if field.is_empty() {
            return Err(WorkflowError::EmptyField);
        }
        Ok(Self {
            field,
            state: SignatureStatus::Unsigned,
            participant: None,
            reason: None,
            location: None,
            contact: None,
            invited_at: None,
            signed_at: None,
            expires_at: None,
            history: Vec::new(),
        })
    }

    /// Returns the field name this workflow tracks.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the current state.
    pub const fn state(&self) -> SignatureStatus {
        self.state
    }

    /// Returns the recorded participant, if any.
    pub fn participant(&self) -> Option<&Participant> {
        self.participant.as_ref()
    }

    /// Returns the recorded transitions in chronological order.
    pub fn history(&self) -> &[Transition] {
        &self.history
    }

    /// Sets a caller-supplied expiry timestamp.
    ///
    /// This only records metadata; use [`SignatureWorkflow::expire_if_due`] to
    /// actually transition to [`SignatureStatus::Expired`].
    pub fn set_expires_at(&mut self, timestamp: impl Into<String>) {
        self.expires_at = Some(timestamp.into());
    }

    fn transition(
        &mut self,
        to: SignatureStatus,
        kind: TransitionKind,
        timestamp: &str,
        note: Option<&str>,
    ) -> Result<(), WorkflowError> {
        if timestamp.trim().is_empty() {
            return Err(WorkflowError::EmptyTimestamp);
        }
        if !self.state.can_transition_to(to) {
            return Err(WorkflowError::InvalidTransition {
                from: self.state,
                to,
            });
        }
        self.history
            .push(Transition::new(kind, self.state, to, timestamp, note));
        self.state = to;
        Ok(())
    }

    /// Invites a participant (`Unsigned -> Pending`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn invite(
        &mut self,
        participant: Participant,
        timestamp: &str,
    ) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Pending,
            TransitionKind::Invite,
            timestamp,
            None,
        )?;
        self.invited_at = Some(timestamp.to_owned());
        self.participant = Some(participant);
        Ok(())
    }

    /// Applies a signature (`Unsigned|Pending -> Signed`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition, empty timestamp,
    /// or empty signer name.
    pub fn sign(
        &mut self,
        participant: Participant,
        timestamp: &str,
        reason: Option<&str>,
        location: Option<&str>,
        contact: Option<&str>,
    ) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Signed,
            TransitionKind::Sign,
            timestamp,
            None,
        )?;
        self.participant = Some(participant);
        self.signed_at = Some(timestamp.to_owned());
        self.reason = reason.map(str::to_owned);
        self.location = location.map(str::to_owned);
        self.contact = contact.map(str::to_owned);
        Ok(())
    }

    /// Declines the signature (`Unsigned|Pending -> Declined`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn decline(&mut self, timestamp: &str, note: Option<&str>) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Declined,
            TransitionKind::Decline,
            timestamp,
            note,
        )
    }

    /// Cancels the workflow (`Unsigned|Pending -> Cancelled`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn cancel(&mut self, timestamp: &str, note: Option<&str>) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Cancelled,
            TransitionKind::Cancel,
            timestamp,
            note,
        )
    }

    /// Records a successful validation (`Signed -> Valid`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn mark_valid(&mut self, timestamp: &str) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Valid,
            TransitionKind::Validate,
            timestamp,
            None,
        )
    }

    /// Records a failed validation (`Signed -> Invalid`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn mark_invalid(
        &mut self,
        timestamp: &str,
        note: Option<&str>,
    ) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Invalid,
            TransitionKind::Invalidate,
            timestamp,
            note,
        )
    }

    /// Records an expiry (`Signed|Valid -> Expired`).
    ///
    /// # Errors
    ///
    /// Returns a [`WorkflowError`] for an invalid transition or empty timestamp.
    pub fn mark_expired(&mut self, timestamp: &str) -> Result<(), WorkflowError> {
        self.transition(
            SignatureStatus::Expired,
            TransitionKind::Expire,
            timestamp,
            None,
        )
    }

    /// Expires the workflow when a caller-supplied `now` reaches `expires_at`.
    ///
    /// Comparison is a plain lexical ordering of the caller-supplied strings,
    /// which is deterministic and correct for fixed-width RFC 3339 UTC stamps.
    /// Returns `Ok(true)` when an expiry transition was applied.
    ///
    /// # Errors
    ///
    /// Returns [`WorkflowError::EmptyTimestamp`] when `now` is empty.
    pub fn expire_if_due(&mut self, now: &str) -> Result<bool, WorkflowError> {
        if now.trim().is_empty() {
            return Err(WorkflowError::EmptyTimestamp);
        }
        let due = self
            .expires_at
            .as_deref()
            .is_some_and(|expires| now >= expires);
        if due && self.state.can_transition_to(SignatureStatus::Expired) {
            self.mark_expired(now)?;
            return Ok(true);
        }
        Ok(false)
    }

    /// Produces a deterministic snapshot of the workflow.
    pub fn report(&self) -> WorkflowReport {
        WorkflowReport {
            field: self.field.clone(),
            status: self.state,
            participant_name: self.participant.as_ref().map(|p| p.name.clone()),
            participant_role: self.participant.as_ref().map(Participant::role),
            reason: self.reason.clone(),
            location: self.location.clone(),
            contact: self.contact.clone(),
            invited_at: self.invited_at.clone(),
            signed_at: self.signed_at.clone(),
            expires_at: self.expires_at.clone(),
            transition_count: self.history.len(),
        }
    }
}

/// A deterministic status report for a [`SignatureWorkflow`].
///
/// [`WorkflowReport::to_lines`] and [`fmt::Display`] always emit the same keys
/// in the same order, with `—` for absent optional values, so reports are
/// stable across runs and safe to compare or log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowReport {
    /// The tracked field name.
    pub field: String,
    /// The current status.
    pub status: SignatureStatus,
    /// The participant name, if any.
    pub participant_name: Option<String>,
    /// The participant role, if any.
    pub participant_role: Option<ParticipantRole>,
    /// The signing reason, if any.
    pub reason: Option<String>,
    /// The signing location, if any.
    pub location: Option<String>,
    /// The contact information, if any.
    pub contact: Option<String>,
    /// The caller-supplied invitation timestamp, if any.
    pub invited_at: Option<String>,
    /// The caller-supplied signing timestamp, if any.
    pub signed_at: Option<String>,
    /// The caller-supplied expiry timestamp, if any.
    pub expires_at: Option<String>,
    /// The number of recorded transitions.
    pub transition_count: usize,
}

impl WorkflowReport {
    /// Returns the report as stable `key: value` lines.
    pub fn to_lines(&self) -> Vec<String> {
        let absent = "—";
        vec![
            format!("field: {}", self.field),
            format!("status: {}", self.status),
            format!(
                "participant: {}",
                self.participant_name.as_deref().unwrap_or(absent)
            ),
            format!(
                "role: {}",
                self.participant_role
                    .map(ParticipantRole::as_str)
                    .unwrap_or(absent)
            ),
            format!("reason: {}", self.reason.as_deref().unwrap_or(absent)),
            format!("location: {}", self.location.as_deref().unwrap_or(absent)),
            format!("contact: {}", self.contact.as_deref().unwrap_or(absent)),
            format!(
                "invited_at: {}",
                self.invited_at.as_deref().unwrap_or(absent)
            ),
            format!("signed_at: {}", self.signed_at.as_deref().unwrap_or(absent)),
            format!(
                "expires_at: {}",
                self.expires_at.as_deref().unwrap_or(absent)
            ),
            format!("transitions: {}", self.transition_count),
        ]
    }
}

impl fmt::Display for WorkflowReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_lines().join("\n"))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::WidgetAppearance;

    fn rect() -> WidgetRect {
        WidgetRect::new(10.0, 20.0, 180.0, 40.0)
    }

    fn signer() -> Participant {
        Participant::new("Ada Lovelace", ParticipantRole::Signer).unwrap()
    }

    #[test]
    fn signature_field_creation_emits_sig_and_v_placeholder() {
        let field = SignatureField::new("order.signature", rect(), 2);
        let dict = field.field_dictionary().unwrap();
        let entries = dict.as_dict().unwrap();

        assert_eq!(entries.get("FT"), Some(&PdfValue::Name("Sig".into())));
        assert_eq!(
            entries.get("T"),
            Some(&PdfValue::Text("order.signature".into()))
        );
        assert_eq!(entries.get("Ff"), Some(&PdfValue::Number(0.0)));
        let v = entries.get("V").unwrap().as_dict().unwrap();
        assert_eq!(v.get("Type"), Some(&PdfValue::Name("Sig".into())));
        assert_eq!(
            v.get("Filter"),
            Some(&PdfValue::Name("Adobe.PPKLite".into()))
        );
        assert_eq!(
            v.get("SubFilter"),
            Some(&PdfValue::Name("adbe.pkcs7.detached".into()))
        );
        // No forged payload or byte range in the placeholder.
        assert!(!v.contains_key("Contents"));
        assert!(!v.contains_key("ByteRange"));
        // Widget page reference is the one-based placeholder.
        let kids = entries.get("Kids").unwrap().as_array().unwrap();
        let widget = kids[0].as_dict().unwrap();
        assert_eq!(
            widget.get("P"),
            Some(&PdfValue::Reference(ObjectRef::new(3, 0)))
        );

        let in_memory = field.to_field().unwrap();
        assert_eq!(in_memory.field_type(), FieldType::Signature);
        assert!(in_memory.value().is_empty());
    }

    #[test]
    fn lock_and_certification_flags_are_emitted() {
        let lock = SignatureLock::new(LockAction::Include, ["order.total", "order.signature"])
            .with_subfilter("adbe.pkcs7.detached");
        let cert = Certification::new(CertPermission::FormAndSigning);
        let field = SignatureField::new("order.signature", rect(), 0)
            .required()
            .with_lock(lock)
            .with_certification(cert);

        assert!(field.is_required());
        assert_eq!(field.flags(), FieldFlags::REQUIRED);

        let entries = field.field_dictionary().unwrap();
        let entries = entries.as_dict().unwrap();
        assert_eq!(entries.get("Ff"), Some(&PdfValue::Number(1.0)));
        let v = entries.get("V").unwrap().as_dict().unwrap();

        let lock_value = v.get("Lock").unwrap().as_dict().unwrap();
        assert_eq!(
            lock_value.get("Action"),
            Some(&PdfValue::Name("Include".into()))
        );
        assert_eq!(
            lock_value.get("Fields"),
            Some(&PdfValue::Array(vec![
                PdfValue::Text("order.total".into()),
                PdfValue::Text("order.signature".into()),
            ]))
        );

        let reference = v.get("Reference").unwrap().as_array().unwrap();
        let transform = reference[0].as_dict().unwrap();
        assert_eq!(
            transform.get("TransformMethod"),
            Some(&PdfValue::Name("DocMDP".into()))
        );
        let params = transform.get("TransformParams").unwrap().as_dict().unwrap();
        assert_eq!(params.get("P"), Some(&PdfValue::Number(2.0)));
    }

    #[test]
    fn cert_permission_maps_to_exact_iso_values() {
        // ISO 32000-1 Table 254: /P is exactly 1, 2 or 3.
        assert_eq!(CertPermission::NoChanges.bits(), 1);
        assert_eq!(CertPermission::FormAndSigning.bits(), 2);
        assert_eq!(CertPermission::AnnotationsAndSigning.bits(), 3);

        // The emitted /TransformParams /P uses those exact integers.
        for permission in [
            CertPermission::NoChanges,
            CertPermission::FormAndSigning,
            CertPermission::AnnotationsAndSigning,
        ] {
            let reference = Certification::new(permission).reference_array();
            let reference = reference.as_array().unwrap();
            let transform = reference[0].as_dict().unwrap();
            let params = transform.get("TransformParams").unwrap().as_dict().unwrap();
            assert_eq!(
                params.get("P"),
                Some(&PdfValue::Number(f64::from(permission.bits())))
            );
        }
    }

    #[test]
    fn reason_location_and_contact_are_emitted() {
        let field = SignatureField::new("sig", rect(), 0)
            .with_name("Ada Lovelace")
            .with_reason("Approved for release")
            .with_location("London")
            .with_contact("ada@example.test")
            .with_signing_time("2026-11-19T10:00:00Z");

        assert_eq!(field.reason(), Some("Approved for release"));
        assert_eq!(field.location(), Some("London"));
        assert_eq!(field.contact(), Some("ada@example.test"));

        let v = field.signature_dictionary();
        let v = v.as_dict().unwrap();
        assert_eq!(v.get("Name"), Some(&PdfValue::Text("Ada Lovelace".into())));
        assert_eq!(
            v.get("Reason"),
            Some(&PdfValue::Text("Approved for release".into()))
        );
        assert_eq!(v.get("Location"), Some(&PdfValue::Text("London".into())));
        assert_eq!(
            v.get("ContactInfo"),
            Some(&PdfValue::Text("ada@example.test".into()))
        );
        assert_eq!(
            v.get("M"),
            Some(&PdfValue::Text("2026-11-19T10:00:00Z".into()))
        );
    }

    #[test]
    fn workflow_transitions_follow_unsigned_pending_signed_valid() {
        let mut workflow = SignatureWorkflow::new("sig").unwrap();
        assert_eq!(workflow.state(), SignatureStatus::Unsigned);

        workflow.invite(signer(), "2026-11-19T09:00:00Z").unwrap();
        assert_eq!(workflow.state(), SignatureStatus::Pending);

        workflow
            .sign(
                signer(),
                "2026-11-19T10:00:00Z",
                Some("Approved"),
                None,
                None,
            )
            .unwrap();
        assert_eq!(workflow.state(), SignatureStatus::Signed);

        workflow.mark_valid("2026-11-19T10:01:00Z").unwrap();
        assert_eq!(workflow.state(), SignatureStatus::Valid);
        assert_eq!(workflow.history().len(), 3);
        assert_eq!(workflow.history()[0].kind(), TransitionKind::Invite);
        assert_eq!(workflow.history()[2].kind(), TransitionKind::Validate);
    }

    #[test]
    fn invalid_transition_is_rejected() {
        let mut workflow = SignatureWorkflow::new("sig").unwrap();
        // Cannot validate an unsigned workflow.
        let err = workflow.mark_valid("2026-11-19T10:01:00Z").unwrap_err();
        assert_eq!(
            err,
            WorkflowError::InvalidTransition {
                from: SignatureStatus::Unsigned,
                to: SignatureStatus::Valid,
            }
        );
        // Failed transition left state and history untouched.
        assert_eq!(workflow.state(), SignatureStatus::Unsigned);
        assert!(workflow.history().is_empty());

        // A terminal state rejects all further transitions.
        let mut signed = SignatureWorkflow::new("sig").unwrap();
        signed
            .sign(signer(), "2026-11-19T10:00:00Z", None, None, None)
            .unwrap();
        signed.mark_invalid("2026-11-19T10:02:00Z", None).unwrap();
        assert!(signed.state().is_terminal());
        assert!(signed.mark_valid("2026-11-19T10:03:00Z").is_err());
    }

    #[test]
    fn decline_and_cancel_transitions() {
        let mut declined = SignatureWorkflow::new("sig").unwrap();
        declined
            .decline("2026-11-19T09:30:00Z", Some("Not authorized"))
            .unwrap();
        assert_eq!(declined.state(), SignatureStatus::Declined);
        assert_eq!(declined.history()[0].note(), Some("Not authorized"));

        let mut cancelled = SignatureWorkflow::new("sig").unwrap();
        cancelled.invite(signer(), "2026-11-19T09:00:00Z").unwrap();
        cancelled.cancel("2026-11-19T09:15:00Z", None).unwrap();
        assert_eq!(cancelled.state(), SignatureStatus::Cancelled);
        assert!(cancelled.state().is_terminal());
    }

    #[test]
    fn deterministic_report() {
        fn build() -> SignatureWorkflow {
            let mut workflow = SignatureWorkflow::new("sig").unwrap();
            workflow.invite(signer(), "2026-11-19T09:00:00Z").unwrap();
            workflow
                .sign(
                    signer(),
                    "2026-11-19T10:00:00Z",
                    Some("Approved"),
                    Some("London"),
                    None,
                )
                .unwrap();
            workflow.mark_valid("2026-11-19T10:01:00Z").unwrap();
            workflow
        }

        let a = build();
        let b = build();
        assert_eq!(a.report(), b.report());
        assert_eq!(a.report().to_string(), b.report().to_string());
        assert_eq!(a.report().to_lines(), b.report().to_lines());
        let lines = a.report().to_lines();
        assert_eq!(lines.first().unwrap(), "field: sig");
        assert_eq!(lines[1], "status: valid");
        assert_eq!(lines.last().unwrap(), "transitions: 3");
    }

    #[test]
    fn byte_range_slot_planning() {
        let slot = SignatureSlot::new(8192).unwrap();
        assert_eq!(slot.reserved_bytes(), 8194);

        let plan = slot.plan(1024);
        assert_eq!(plan.first_start, 0);
        assert_eq!(plan.first_length, 1024);
        assert_eq!(plan.second_start, 1024 + 8194);
        assert_eq!(plan.second_length(), None);
        assert_eq!(plan.as_array(), None);

        let total = 1024 + 8194 + 2048;
        let complete = slot.plan_with_total(1024, total as u64);
        assert_eq!(complete.as_array(), Some([0, 1024, 9218, 2048]));
    }

    #[test]
    fn placeholder_content_sizing() {
        let slot = SignatureSlot::default_size().unwrap();
        assert_eq!(slot.placeholder_size(), DEFAULT_PLACEHOLDER_SIZE);
        assert_eq!(slot.contents_placeholder().len(), DEFAULT_PLACEHOLDER_SIZE);
        assert!(slot.contents_placeholder().iter().all(|b| *b == b'0'));
        assert_eq!(slot.reserved_bytes(), DEFAULT_PLACEHOLDER_SIZE as u64 + 2);
    }

    #[test]
    fn signed_appearance_presence() {
        let field = SignatureField::new("sig", rect(), 0)
            .with_name("Ada Lovelace")
            .with_reason("Approved");
        let widget = WidgetAppearance::new(rect());
        let appearance = field.appearance(&widget).unwrap();

        assert!(appearance.normal_stream().is_some());
        let stream = appearance.normal_stream().unwrap();
        assert!(!stream.content.is_empty());
        assert_eq!(stream.bbox.width, rect().width);
        assert_eq!(stream.bbox.height, rect().height);
        assert_eq!(
            field.signature_line(),
            "Digitally signed by Ada Lovelace; Reason: Approved"
        );
    }

    #[test]
    fn invalid_placeholder_sizes_are_rejected() {
        assert_eq!(
            SignatureSlot::new(0),
            Err(SignatureError::InvalidPlaceholderSize)
        );
        assert_eq!(
            SignatureSlot::new(7),
            Err(SignatureError::OddPlaceholderSize(7))
        );
        assert_eq!(
            SignatureSlot::new(MAX_PLACEHOLDER_SIZE + 2),
            Err(SignatureError::PlaceholderTooLarge {
                size: MAX_PLACEHOLDER_SIZE + 2,
                max: MAX_PLACEHOLDER_SIZE,
            })
        );
    }

    #[test]
    fn workflow_expiry_transition() {
        let mut workflow = SignatureWorkflow::new("sig").unwrap();
        workflow
            .sign(signer(), "2026-11-19T10:00:00Z", None, None, None)
            .unwrap();
        workflow.set_expires_at("2026-11-20T00:00:00Z");

        assert!(!workflow.expire_if_due("2026-11-19T12:00:00Z").unwrap());
        assert_eq!(workflow.state(), SignatureStatus::Signed);

        assert!(workflow.expire_if_due("2026-11-20T00:00:00Z").unwrap());
        assert_eq!(workflow.state(), SignatureStatus::Expired);
        assert_eq!(
            workflow.history().last().unwrap().kind(),
            TransitionKind::Expire
        );
    }

    #[test]
    fn invalid_field_geometry_is_a_typed_error() {
        let empty = SignatureField::new("", rect(), 0);
        assert_eq!(empty.validate(), Err(SignatureError::EmptyName));

        let zero = SignatureField::new("sig", WidgetRect::new(0.0, 0.0, 0.0, 10.0), 0);
        assert_eq!(zero.validate(), Err(SignatureError::ZeroSizedRect));

        let nan = SignatureField::new("sig", WidgetRect::new(f32::NAN, 0.0, 10.0, 10.0), 0);
        assert_eq!(nan.validate(), Err(SignatureError::NonFiniteRect));

        let overflow = SignatureField::new("sig", rect(), u32::MAX);
        assert_eq!(
            overflow.validate(),
            Err(SignatureError::PageOverflow(u32::MAX))
        );
    }

    #[test]
    fn empty_timestamps_and_participants_are_rejected() {
        let mut workflow = SignatureWorkflow::new("sig").unwrap();
        assert_eq!(
            workflow.invite(signer(), "  "),
            Err(WorkflowError::EmptyTimestamp)
        );
        assert_eq!(
            Participant::new("", ParticipantRole::Signer),
            Err(WorkflowError::EmptyName)
        );
        assert_eq!(SignatureWorkflow::new(""), Err(WorkflowError::EmptyField));
    }
}
