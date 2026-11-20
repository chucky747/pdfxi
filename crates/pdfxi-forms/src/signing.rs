//! Signer-facing byte-range planning, verification boundary and lifecycle.
//!
//! This module closes the *library-level* electronic-signature workflow. It
//! adds three things that the object-model-only [`crate::signature`] module does
//! not provide:
//!
//! - A deterministic [`plan_byte_range`] contract: given serialized PDF bytes
//!   containing a known `/Contents` placeholder, compute the exact
//!   `/ByteRange [a b c d]` a signer must cover, [`validate_byte_range`] it, and
//!   reconstruct the exact bytes to be signed with [`signed_bytes`]. No hash is
//!   computed here; callers hand the returned bytes to the security layer.
//! - A caller-supplied [`SignatureVerifier`] trait and a structure-only default
//!   stub ([`StructureOnlyVerifier`]). **No cryptography lives in this crate.**
//!   The security lane owns hashing, key handling and cryptographic validation;
//!   this crate only checks that a verification package is structurally sound
//!   and records the outcome deterministically.
//! - A [`SignatureLifecycle`] state machine
//!   `create -> sign-request -> attach bytes -> verify -> report`. Signature
//!   bytes are supplied by the caller, placed verbatim into the emitted `/V`
//!   dictionary by [`SignatureLifecycle::completed_v`], and validated only
//!   through a caller-supplied verifier.
//!
//! All output is deterministic: byte ranges are pure functions of the input
//! bytes, reports use fixed key order, and no clock, filesystem, network or
//! random source is consulted. No function here uses `unwrap`, `expect`,
//! `panic!` or `unsafe`.

use crate::acroform::PdfValue;
use crate::signature::{
    ByteRangePlan, Participant, SignatureDictionary, SignatureStatus, SignatureWorkflow,
    Transition, WorkflowError,
};
use std::fmt;

/// A typed error produced while planning or validating a `/ByteRange`.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ByteRangeError {
    /// The caller-supplied placeholder was empty, odd-length or not hex.
    #[error("signature /Contents placeholder is not a non-empty even-length hex string")]
    InvalidPlaceholder,
    /// The placeholder token was not found in the document.
    #[error("signature /Contents placeholder was not found in the document")]
    PlaceholderNotFound,
    /// The placeholder token appeared more than once; the range is ambiguous.
    #[error("signature /Contents placeholder appeared {0} times in the document")]
    AmbiguousPlaceholder(usize),
    /// A plan without a known total length cannot form a four-element range.
    #[error("byte range has no known total length")]
    IncompleteRange,
    /// The supplied plan did not match the document's computed range.
    #[error("byte range mismatch: expected {expected:?}, found {actual:?}")]
    InvalidRange {
        /// The `[a b c d]` computed from the document.
        expected: [u64; 4],
        /// The `[a b c d]` supplied by the caller.
        actual: [u64; 4],
    },
    /// One of the two intervals had zero length.
    #[error("byte range contains an empty interval")]
    EmptyRange,
    /// The two intervals were inverted or overlapped.
    ///
    /// A valid plan has `first_start <= first_end <= second_start <= second_end`.
    #[error("byte range intervals are inverted or overlap")]
    InvalidRangeOrder,
    /// A range index fell outside the document or the first range did not start
    /// at offset zero.
    #[error("byte range is outside the document bounds")]
    OutOfBounds,
    /// A byte offset could not be represented as a `u64`.
    #[error("byte offset overflowed u64")]
    Overflow,
}

fn find_occurrences(haystack: &[u8], needle: &[u8]) -> Vec<usize> {
    let mut positions = Vec::new();
    if needle.is_empty() || needle.len() > haystack.len() {
        return positions;
    }
    for (start, window) in haystack.windows(needle.len()).enumerate() {
        if window == needle {
            positions.push(start);
        }
    }
    positions
}

fn is_hex_placeholder(placeholder: &[u8]) -> bool {
    !placeholder.is_empty()
        && placeholder.len().is_multiple_of(2)
        && placeholder.iter().all(|byte| byte.is_ascii_hexdigit())
}

/// Computes a deterministic `/ByteRange` from serialized PDF bytes.
///
/// `placeholder_hex` is the raw hex text of the reserved `/Contents` value
/// (for example the bytes from [`crate::SignatureSlot::contents_placeholder`]).
/// The function locates the unique `<placeholder>` token in `document`, treats
/// that token (including its `<` and `>` delimiters) as the gap, and returns a
/// complete [`ByteRangePlan`] whose four-element array covers every byte
/// *outside* the gap.
///
/// # Errors
///
/// Returns [`ByteRangeError`] when the placeholder is invalid, absent or
/// ambiguous, or when an offset cannot be represented as a `u64`.
pub fn plan_byte_range(
    document: &[u8],
    placeholder_hex: &[u8],
) -> Result<ByteRangePlan, ByteRangeError> {
    if !is_hex_placeholder(placeholder_hex) {
        return Err(ByteRangeError::InvalidPlaceholder);
    }
    let mut needle = Vec::with_capacity(placeholder_hex.len() + 2);
    needle.push(b'<');
    needle.extend_from_slice(placeholder_hex);
    needle.push(b'>');

    let positions = find_occurrences(document, &needle);
    match positions.as_slice() {
        [] => Err(ByteRangeError::PlaceholderNotFound),
        [only] => {
            let offset = u64::try_from(*only).map_err(|_| ByteRangeError::Overflow)?;
            let total = u64::try_from(document.len()).map_err(|_| ByteRangeError::Overflow)?;
            let gap =
                u64::try_from(placeholder_hex.len()).map_err(|_| ByteRangeError::Overflow)? + 2;
            Ok(ByteRangePlan {
                first_start: 0,
                first_length: offset,
                second_start: offset.checked_add(gap).ok_or(ByteRangeError::Overflow)?,
                total_length: Some(total),
            })
        }
        many => Err(ByteRangeError::AmbiguousPlaceholder(many.len())),
    }
}

/// Validates that `plan` is exactly the range computed for `document`.
///
/// This is the signer-side guard: it rejects an incomplete plan (unknown total
/// length) or one whose offsets do not match the serialized placeholder.
///
/// # Errors
///
/// Returns [`ByteRangeError`] for an invalid placeholder, a missing or
/// ambiguous token, an incomplete plan or a mismatched range.
pub fn validate_byte_range(
    document: &[u8],
    plan: &ByteRangePlan,
    placeholder_hex: &[u8],
) -> Result<(), ByteRangeError> {
    let expected = plan_byte_range(document, placeholder_hex)?;
    let expected_array = expected.as_array().ok_or(ByteRangeError::IncompleteRange)?;
    let actual_array = plan.as_array().ok_or(ByteRangeError::IncompleteRange)?;
    if actual_array == expected_array {
        Ok(())
    } else {
        Err(ByteRangeError::InvalidRange {
            expected: expected_array,
            actual: actual_array,
        })
    }
}

/// Reconstructs the exact byte sequence covered by `plan`.
///
/// This is the concatenation `document[a..a+b] ++ document[c..c+d]` — the bytes
/// a caller would pass to the security layer for digesting. It deliberately
/// excludes the `/Contents` gap. No hashing is performed here.
///
/// # Errors
///
/// Returns [`ByteRangeError`] for an incomplete plan, an empty or inverted
/// interval, a range outside the document or an offset that cannot be
/// represented as a `usize`.
///
/// The two intervals must satisfy
/// `first_start < first_end <= second_start < second_end <= total`, with
/// `first_start == 0`. The ordering and emptiness checks are deliberately
/// separate from the bounds check so callers receive a precise typed error.
pub fn signed_bytes(document: &[u8], plan: &ByteRangePlan) -> Result<Vec<u8>, ByteRangeError> {
    let [first_start, first_length, second_start, second_length] =
        plan.as_array().ok_or(ByteRangeError::IncompleteRange)?;
    let total = u64::try_from(document.len()).map_err(|_| ByteRangeError::Overflow)?;
    let second_end = second_start
        .checked_add(second_length)
        .ok_or(ByteRangeError::OutOfBounds)?;
    let first_end = first_start
        .checked_add(first_length)
        .ok_or(ByteRangeError::OutOfBounds)?;
    // Inversion / overlap. The `first_start > first_end` and
    // `second_start > second_end` arms are defensive: `as_array` derives both
    // ends with unsigned addition, but a caller-supplied plan is still checked
    // explicitly so the invariant is enforced in one place.
    if first_start > first_end || second_start > second_end || second_start < first_end {
        return Err(ByteRangeError::InvalidRangeOrder);
    }
    if first_start == first_end || second_start == second_end {
        return Err(ByteRangeError::EmptyRange);
    }
    if first_start != 0 || first_end > total || second_end > total {
        return Err(ByteRangeError::OutOfBounds);
    }
    let from = usize::try_from(first_start).map_err(|_| ByteRangeError::Overflow)?;
    let to = usize::try_from(first_end).map_err(|_| ByteRangeError::Overflow)?;
    let c = usize::try_from(second_start).map_err(|_| ByteRangeError::Overflow)?;
    let d = usize::try_from(second_end).map_err(|_| ByteRangeError::Overflow)?;
    let mut out = Vec::with_capacity((to - from) + (d - c));
    out.extend_from_slice(&document[from..to]);
    out.extend_from_slice(&document[c..d]);
    Ok(out)
}

/// A typed error produced by a [`SignatureVerifier`].
///
/// This crate defines only *structural* failures. Cryptographic verification
/// failures are produced by the security lane and surfaced to callers through
/// [`VerificationError::Rejected`] with an opaque, deterministic detail string.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum VerificationError {
    /// The signature handler filter was empty.
    #[error("signature filter is empty")]
    EmptyFilter,
    /// The signature subfilter was empty.
    #[error("signature subfilter is empty")]
    EmptySubFilter,
    /// The byte sequence to be verified was empty.
    #[error("signed byte sequence is empty")]
    EmptySignedBytes,
    /// The caller-supplied signature bytes were empty.
    #[error("signature contents are empty")]
    EmptyContents,
    /// A caller-supplied verifier rejected the package for another reason.
    #[error("signature verification rejected: {0}")]
    Rejected(String),
}

/// The structural input handed to a [`SignatureVerifier`].
///
/// `signed_bytes` is the output of [`signed_bytes`]; `contents` is the raw
/// caller-supplied signature payload. No cryptography is implied by carrying
/// these slices — the verifier decides what, if anything, to do with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VerificationRequest<'a> {
    /// The `/Filter` name, without the leading slash.
    pub filter: &'a str,
    /// The `/SubFilter` name, without the leading slash.
    pub subfilter: &'a str,
    /// The exact byte sequence covered by the `/ByteRange`.
    pub signed_bytes: &'a [u8],
    /// The caller-supplied `/Contents` bytes.
    pub contents: &'a [u8],
}

impl<'a> VerificationRequest<'a> {
    /// Creates a verification request from the four structural inputs.
    pub const fn new(
        filter: &'a str,
        subfilter: &'a str,
        signed_bytes: &'a [u8],
        contents: &'a [u8],
    ) -> Self {
        Self {
            filter,
            subfilter,
            signed_bytes,
            contents,
        }
    }
}

/// The cryptographic authenticity level of a verification result.
///
/// A verifier that only checks structure can honestly return
/// [`Authenticity::StructureOnly`], but a caller must never present the
/// resulting [`SignatureStatus::Valid`] to a user as an authentic, validated
/// signature. Only [`Authenticity::Cryptographic`] — produced by a caller that
/// actually verified the signature bytes and key material — may be shown as
/// cryptographically authentic.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Authenticity {
    /// Only the signature package structure was inspected; no cryptographic
    /// claim is made.
    ///
    /// This is the safe default: an outcome constructed without an explicit
    /// level can never be mistaken for a cryptographic verification.
    #[default]
    StructureOnly,
    /// A caller-supplied cryptographic verifier validated the signature.
    Cryptographic,
}

impl Authenticity {
    /// Returns the stable token emitted in [`LifecycleReport`] lines.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::StructureOnly => "structure-only",
            Self::Cryptographic => "cryptographic",
        }
    }
}

impl fmt::Display for Authenticity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The deterministic outcome of a verification attempt.
///
/// Every outcome carries an explicit [`Authenticity`] level. A `valid` outcome
/// with [`Authenticity::StructureOnly`] is *not* a cryptographic validation and
/// must not be displayed as an authentic signature.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationOutcome {
    valid: bool,
    detail: String,
    authenticity: Authenticity,
}

impl VerificationOutcome {
    /// Creates an outcome with the safe [`Authenticity::StructureOnly`] level.
    ///
    /// Cryptographic verifiers should use [`Self::with_authenticity`] instead
    /// so the result cannot be mistaken for a structure-only check.
    pub fn new(valid: bool, detail: impl Into<String>) -> Self {
        Self::with_authenticity(valid, detail, Authenticity::StructureOnly)
    }

    /// Creates an outcome with an explicit authenticity level.
    pub fn with_authenticity(
        valid: bool,
        detail: impl Into<String>,
        authenticity: Authenticity,
    ) -> Self {
        Self {
            valid,
            detail: detail.into(),
            authenticity,
        }
    }

    /// Returns whether the verifier accepted the package.
    pub const fn is_valid(&self) -> bool {
        self.valid
    }

    /// Returns the verifier's deterministic detail string.
    pub fn detail(&self) -> &str {
        &self.detail
    }

    /// Returns the cryptographic authenticity level of this outcome.
    pub const fn authenticity(&self) -> Authenticity {
        self.authenticity
    }
}

/// A caller-supplied verifier for a signature package.
///
/// # Cryptography boundary
///
/// This trait is intentionally crypto-agnostic. Implementations in
/// `pdfxi-security` may hash bytes, check certificates or validate CMS/PKCS#7;
/// this crate neither performs nor models those operations. A verifier may
/// fail with any typed [`VerificationError`], and the [`SignatureLifecycle`]
/// records the result without interpreting it. Implementations must be
/// deterministic for a given request, must not read the clock, and must not
/// perform I/O.
pub trait SignatureVerifier {
    /// Verifies `request`, returning a deterministic outcome or typed error.
    ///
    /// # Errors
    ///
    /// Returns a [`VerificationError`] describing why the package was refused.
    fn verify(
        &self,
        request: &VerificationRequest<'_>,
    ) -> Result<VerificationOutcome, VerificationError>;
}

/// The default structure-only verifier.
///
/// It checks that the filter, subfilter, signed byte sequence and `/Contents`
/// bytes are all non-empty and returns a stable `structure-ok` outcome with
/// [`Authenticity::StructureOnly`]. It performs **no cryptographic validation**:
/// a [`VerificationOutcome::is_valid`] of `true` from this verifier must never
/// be displayed to a user as an authentic signature. It exists so a caller can
/// exercise the full lifecycle without importing the security crate; real
/// cryptographic verification belongs to `pdfxi-security` and must report
/// [`Authenticity::Cryptographic`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct StructureOnlyVerifier;

impl SignatureVerifier for StructureOnlyVerifier {
    fn verify(
        &self,
        request: &VerificationRequest<'_>,
    ) -> Result<VerificationOutcome, VerificationError> {
        if request.filter.is_empty() {
            return Err(VerificationError::EmptyFilter);
        }
        if request.subfilter.is_empty() {
            return Err(VerificationError::EmptySubFilter);
        }
        if request.signed_bytes.is_empty() {
            return Err(VerificationError::EmptySignedBytes);
        }
        if request.contents.is_empty() {
            return Err(VerificationError::EmptyContents);
        }
        Ok(VerificationOutcome::new(
            true,
            format!(
                "structure-ok:{}:{}:{}:{}",
                request.filter,
                request.subfilter,
                request.signed_bytes.len(),
                request.contents.len()
            ),
        ))
    }
}

/// A typed error produced by a [`SignatureLifecycle`] operation.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum LifecycleError {
    /// The underlying workflow rejected an operation.
    #[error(transparent)]
    Workflow(#[from] WorkflowError),
    /// Byte-range planning or validation failed.
    #[error(transparent)]
    ByteRange(#[from] ByteRangeError),
    /// The caller-supplied verifier rejected the package.
    #[error(transparent)]
    Verification(#[from] VerificationError),
    /// No byte-range plan was recorded before this operation.
    #[error("no byte range has been planned for this signature")]
    MissingByteRange,
    /// No signature bytes were attached before this operation.
    #[error("no signature bytes have been attached to this signature")]
    MissingContents,
    /// The operation requires the unsigned state.
    #[error("signature lifecycle is {actual}, not unsigned")]
    NotUnsigned {
        /// The actual state.
        actual: SignatureStatus,
    },
    /// The operation requires the pending state.
    #[error("signature lifecycle is {actual}, not pending")]
    NotPending {
        /// The actual state.
        actual: SignatureStatus,
    },
    /// The operation requires the signed state.
    #[error("signature lifecycle is {actual}, not signed")]
    NotSigned {
        /// The actual state.
        actual: SignatureStatus,
    },
    /// The signature dictionary placeholder had an unexpected shape.
    #[error("signature dictionary placeholder is malformed")]
    MalformedDictionary,
}

/// Converts a complete `[a b c d]` byte range into exact integer PDF objects.
///
/// Each `u64` offset is carried by [`PdfValue::Integer`], so offsets above 2^53
/// are never rounded through `f64` and never serialize in scientific notation.
fn byte_range_values(range: [u64; 4]) -> [PdfValue; 4] {
    range.map(PdfValue::Integer)
}

fn hex_encode(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789ABCDEF";
    let mut out = String::with_capacity(bytes.len().saturating_mul(2));
    for &byte in bytes {
        out.push(DIGITS[usize::from(byte >> 4)] as char);
        out.push(DIGITS[usize::from(byte & 0x0F)] as char);
    }
    out
}

/// A complete, deterministic snapshot of a [`SignatureLifecycle`].
#[derive(Debug, Clone, PartialEq)]
pub struct LifecycleReport {
    /// The tracked field name.
    pub field: String,
    /// The current workflow state.
    pub status: SignatureStatus,
    /// The planned `[a b c d]` ranges, when complete.
    pub byte_range: Option<[u64; 4]>,
    /// The number of caller-supplied signature bytes attached.
    pub contents_len: usize,
    /// The verifier outcome, when verification succeeded.
    pub verification: Option<VerificationOutcome>,
    /// The verifier error detail, when verification failed.
    pub failure: Option<String>,
    /// The authenticity level recorded for this lifecycle.
    ///
    /// Defaults to [`Authenticity::StructureOnly`] before verification and
    /// updates only when a verifier reports a cryptographic result. A report
    /// that shows `status=valid` with `authenticity=structure-only` must not be
    /// presented to a user as an authentic signature.
    pub authenticity: Authenticity,
    transitions: Vec<Transition>,
}

impl LifecycleReport {
    /// Returns every recorded transition in chronological order.
    pub fn transitions(&self) -> &[Transition] {
        &self.transitions
    }

    /// Returns stable `key=value` lines, including one per transition.
    pub fn to_lines(&self) -> Vec<String> {
        let mut lines = vec![
            format!("lifecycle.field={}", self.field),
            format!("lifecycle.status={}", self.status),
            format!(
                "lifecycle.byte_range={}",
                self.byte_range
                    .map(|range| format!("{:?}", range))
                    .unwrap_or_else(|| "absent".to_owned())
            ),
            format!("lifecycle.contents_len={}", self.contents_len),
            format!(
                "lifecycle.verification={}",
                self.verification
                    .as_ref()
                    .map(VerificationOutcome::detail)
                    .unwrap_or("absent")
            ),
            format!(
                "lifecycle.failure={}",
                self.failure.as_deref().unwrap_or("absent")
            ),
            format!("lifecycle.authenticity={}", self.authenticity.as_str()),
        ];
        for (index, transition) in self.transitions.iter().enumerate() {
            let note = transition.note().unwrap_or("-");
            lines.push(format!(
                "transition[{index}]={}:{}->{}@{} ({note})",
                transition.kind().as_str(),
                transition.from(),
                transition.to(),
                transition.timestamp()
            ));
        }
        lines
    }
}

impl fmt::Display for LifecycleReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.to_lines().join("\n"))
    }
}

/// Returns deterministic lines describing all states and transitions of a
/// workflow, independent of any lifecycle.
///
/// Each line has the form
/// `transition[i]=kind:from->to@timestamp (note)`, with `-` for a missing note.
pub fn workflow_transition_lines(workflow: &SignatureWorkflow) -> Vec<String> {
    workflow
        .history()
        .iter()
        .enumerate()
        .map(|(index, transition)| {
            format!(
                "transition[{index}]={}:{}->{}@{} ({})",
                transition.kind().as_str(),
                transition.from(),
                transition.to(),
                transition.timestamp(),
                transition.note().unwrap_or("-")
            )
        })
        .collect()
}

/// A caller-driven signature lifecycle:
/// `create -> sign-request -> attach bytes -> verify -> report`.
///
/// The lifecycle composes the existing [`SignatureWorkflow`] state machine with
/// a caller-supplied byte-range plan and signature bytes. It performs no
/// cryptography: bytes are stored and hex-encoded verbatim, and verification is
/// delegated to a [`SignatureVerifier`].
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureLifecycle {
    workflow: SignatureWorkflow,
    dictionary: SignatureDictionary,
    byte_range: Option<ByteRangePlan>,
    /// The placeholder hex the plan was computed against, retained so
    /// [`Self::verify`] can re-derive and validate the plan.
    placeholder_hex: Vec<u8>,
    contents: Option<Vec<u8>>,
    verification: Option<VerificationOutcome>,
    failure: Option<VerificationError>,
}

impl SignatureLifecycle {
    /// Creates a lifecycle in the unsigned state with the given `/V`
    /// placeholder metadata.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::Workflow`] when the field name is empty.
    pub fn new(
        field: impl Into<String>,
        dictionary: SignatureDictionary,
    ) -> Result<Self, LifecycleError> {
        Ok(Self {
            workflow: SignatureWorkflow::new(field)?,
            dictionary,
            byte_range: None,
            placeholder_hex: Vec::new(),
            contents: None,
            verification: None,
            failure: None,
        })
    }

    /// Starts a sign request (`unsigned -> pending`) with a byte-range plan.
    ///
    /// `placeholder_hex` is the raw `/Contents` placeholder token the plan was
    /// computed against. It is retained so [`Self::verify`] can re-derive the
    /// plan from the document and reject a caller-supplied plan that does not
    /// match it.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::NotUnsigned`] when not in the unsigned state,
    /// or [`LifecycleError::Workflow`] for an empty timestamp.
    pub fn request_sign(
        &mut self,
        participant: Participant,
        timestamp: &str,
        byte_range: ByteRangePlan,
        placeholder_hex: &[u8],
    ) -> Result<(), LifecycleError> {
        if self.workflow.state() != SignatureStatus::Unsigned {
            return Err(LifecycleError::NotUnsigned {
                actual: self.workflow.state(),
            });
        }
        self.workflow.invite(participant, timestamp)?;
        self.byte_range = Some(byte_range);
        self.placeholder_hex = placeholder_hex.to_vec();
        Ok(())
    }

    /// Attaches caller-supplied signature bytes (`pending -> signed`).
    ///
    /// A byte-range plan must already be installed by [`Self::request_sign`].
    /// The bytes are stored verbatim; no hashing or signature construction is
    /// performed.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::NotPending`], [`LifecycleError::MissingByteRange`]
    /// or [`LifecycleError::Workflow`] for an empty timestamp.
    pub fn attach_signature_bytes(
        &mut self,
        participant: Participant,
        timestamp: &str,
        contents: Vec<u8>,
    ) -> Result<(), LifecycleError> {
        if self.workflow.state() != SignatureStatus::Pending {
            return Err(LifecycleError::NotPending {
                actual: self.workflow.state(),
            });
        }
        if self.byte_range.is_none() {
            return Err(LifecycleError::MissingByteRange);
        }
        self.workflow
            .sign(participant, timestamp, None, None, None)?;
        self.contents = Some(contents);
        Ok(())
    }

    /// Verifies the attached bytes through a caller-supplied verifier.
    ///
    /// The `document` slice must be the serialized bytes from which the planned
    /// byte range was computed; [`signed_bytes`] reconstructs the covered bytes
    /// and they are passed to `verifier` together with the placeholder filter,
    /// subfilter and attached contents. Before reconstructing bytes the plan is
    /// validated against `document` with [`validate_byte_range`]; a plan that
    /// does not match the document is rejected with a typed byte-range error
    /// and does not change state. On acceptance the state becomes `valid` and
    /// the outcome is recorded; on rejection the state becomes `invalid` and
    /// the typed error is recorded.
    ///
    /// A `valid` state produced here is only as trustworthy as the verifier's
    /// authenticity level; see [`VerificationOutcome::authenticity`].
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::NotSigned`], [`LifecycleError::MissingByteRange`],
    /// [`LifecycleError::MissingContents`], a [`LifecycleError::ByteRange`] error,
    /// [`LifecycleError::Verification`] when the verifier refuses the package,
    /// or [`LifecycleError::Workflow`] for an empty timestamp.
    pub fn verify(
        &mut self,
        verifier: &dyn SignatureVerifier,
        document: &[u8],
        timestamp: &str,
    ) -> Result<VerificationOutcome, LifecycleError> {
        if self.workflow.state() != SignatureStatus::Signed {
            return Err(LifecycleError::NotSigned {
                actual: self.workflow.state(),
            });
        }
        let plan = self.byte_range.ok_or(LifecycleError::MissingByteRange)?;
        let contents = self
            .contents
            .clone()
            .ok_or(LifecycleError::MissingContents)?;
        // Re-derive the plan from the document before trusting it. A
        // caller-supplied plan can be structurally complete yet describe ranges
        // that do not belong to `document`; `validate_byte_range` returns a
        // typed `ByteRangeError` in that case (wrapped as `LifecycleError::ByteRange`).
        let placeholder = self.placeholder_hex.clone();
        validate_byte_range(document, &plan, &placeholder)?;
        let covered = signed_bytes(document, &plan)?;
        let filter = self.dictionary.filter().as_str();
        let subfilter = self.dictionary.subfilter().as_str();
        let request = VerificationRequest::new(filter, subfilter, &covered, &contents);
        match verifier.verify(&request) {
            Ok(outcome) => {
                self.workflow.mark_valid(timestamp)?;
                self.verification = Some(outcome.clone());
                Ok(outcome)
            }
            Err(error) => {
                self.workflow
                    .mark_invalid(timestamp, Some(&error.to_string()))?;
                self.failure = Some(error.clone());
                Err(LifecycleError::Verification(error))
            }
        }
    }

    /// Emits the completed `/V` dictionary with caller-supplied `/Contents`.
    ///
    /// The `/Contents` value is the hex encoding of the attached bytes and
    /// `/ByteRange` is the complete `[a b c d]` array from the plan, emitted as
    /// exact [`PdfValue::Integer`] objects so large offsets never lose precision
    /// or become scientific notation. This is *serialization*, not signing: no
    /// byte is hashed or transformed except hex encoding. The placeholder
    /// metadata (`/Name`, `/Reason`, `/Lock`, `/Reference`, …) is preserved.
    ///
    /// # Errors
    ///
    /// Returns [`LifecycleError::MissingContents`],
    /// [`LifecycleError::MissingByteRange`], [`LifecycleError::ByteRange`] for
    /// an incomplete plan, or [`LifecycleError::MalformedDictionary`].
    pub fn completed_v(&self) -> Result<PdfValue, LifecycleError> {
        let contents = self
            .contents
            .as_ref()
            .ok_or(LifecycleError::MissingContents)?;
        let range = self
            .byte_range
            .ok_or(LifecycleError::MissingByteRange)?
            .as_array()
            .ok_or(ByteRangeError::IncompleteRange)?;
        let mut dict = self
            .dictionary
            .to_pdf()
            .as_dict()
            .cloned()
            .ok_or(LifecycleError::MalformedDictionary)?;
        dict.insert("Contents".to_owned(), PdfValue::Text(hex_encode(contents)));
        dict.insert(
            "ByteRange".to_owned(),
            PdfValue::Array(byte_range_values(range).to_vec()),
        );
        Ok(PdfValue::Dictionary(dict))
    }

    /// Returns the underlying workflow.
    pub const fn workflow(&self) -> &SignatureWorkflow {
        &self.workflow
    }

    /// Returns the placeholder metadata dictionary.
    pub const fn dictionary(&self) -> &SignatureDictionary {
        &self.dictionary
    }

    /// Returns the planned byte range, if any.
    pub const fn byte_range(&self) -> Option<ByteRangePlan> {
        self.byte_range
    }

    /// Returns the attached signature bytes, if any.
    pub fn contents(&self) -> Option<&[u8]> {
        self.contents.as_deref()
    }

    /// Returns the verifier outcome, if verification succeeded.
    pub const fn verification(&self) -> Option<&VerificationOutcome> {
        self.verification.as_ref()
    }

    /// Returns the verifier error, if verification failed.
    pub const fn failure(&self) -> Option<&VerificationError> {
        self.failure.as_ref()
    }

    /// Returns the current lifecycle state.
    pub const fn state(&self) -> SignatureStatus {
        self.workflow.state()
    }

    /// Returns the tracked field name.
    pub fn field(&self) -> &str {
        self.workflow.field()
    }

    /// Produces a deterministic snapshot including every transition.
    pub fn report(&self) -> LifecycleReport {
        LifecycleReport {
            field: self.field().to_owned(),
            status: self.state(),
            byte_range: self.byte_range.and_then(|plan| plan.as_array()),
            contents_len: self.contents.as_ref().map_or(0, Vec::len),
            verification: self.verification.clone(),
            failure: self.failure.as_ref().map(VerificationError::to_string),
            authenticity: self.verification.as_ref().map_or(
                Authenticity::StructureOnly,
                VerificationOutcome::authenticity,
            ),
            transitions: self.workflow.history().to_vec(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::acroform::ObjectRef;
    use crate::appearance::WidgetRect;
    use crate::build::FieldSpec;
    use crate::fillworkflow::{FillDocument, FillWorkflow, FillWorkflowError};
    use crate::signature::{CertPermission, LockScope, ParticipantRole, SignatureGuard};
    use std::cell::Cell;
    use std::collections::BTreeMap;

    fn rect() -> WidgetRect {
        WidgetRect::new(10.0, 20.0, 180.0, 40.0)
    }

    fn signer() -> Participant {
        Participant::new("Ada", ParticipantRole::Signer).unwrap()
    }

    fn document_with_placeholder(placeholder: &[u8]) -> Vec<u8> {
        let mut document = Vec::new();
        document.extend_from_slice(b"%PDF-1.7\n");
        document.extend_from_slice(b"/Contents <");
        document.extend_from_slice(placeholder);
        document.extend_from_slice(b">\n%%EOF\n");
        document
    }

    fn planned_range(contents_offset: u64, gap: u64, total: u64) -> ByteRangePlan {
        ByteRangePlan {
            first_start: 0,
            first_length: contents_offset,
            second_start: contents_offset + gap,
            total_length: Some(total),
        }
    }

    #[test]
    fn byte_range_math_is_computed_from_serialized_document() {
        let placeholder = crate::SignatureSlot::new(16).unwrap();
        let placeholder_bytes = placeholder.contents_placeholder();
        let document = document_with_placeholder(placeholder_bytes);
        let plan = plan_byte_range(&document, placeholder_bytes).unwrap();

        let gap = placeholder_bytes.len() as u64 + 2;
        let array = plan.as_array().unwrap();
        assert_eq!(array[0], 0);
        assert_eq!(array[2], array[1] + gap);
        assert_eq!(array[3], document.len() as u64 - array[2]);
        assert_eq!(array[1], 19); // "/Contents <" begins at offset 9, '<' at 19.
        assert_eq!(plan.total_length, Some(document.len() as u64));
        assert_eq!(
            signed_bytes(&document, &plan).unwrap().len(),
            array[1] as usize + array[3] as usize
        );
    }

    #[test]
    fn byte_range_round_trip_excludes_placeholder() {
        let slot = crate::SignatureSlot::default_size().unwrap();
        let placeholder = slot.contents_placeholder();
        let document = document_with_placeholder(placeholder);
        let plan = plan_byte_range(&document, placeholder).unwrap();
        validate_byte_range(&document, &plan, placeholder).unwrap();

        let covered = signed_bytes(&document, &plan).unwrap();
        let array = plan.as_array().unwrap();
        let mut expected = Vec::new();
        expected.extend_from_slice(&document[..array[1] as usize]);
        expected.extend_from_slice(&document[array[2] as usize..]);
        assert_eq!(covered, expected);
        assert_eq!(
            covered.len() + slot.reserved_bytes() as usize,
            document.len()
        );
    }

    #[test]
    fn placeholder_sizing_drives_byte_range_offsets() {
        for size in [2usize, 16, 64, crate::DEFAULT_PLACEHOLDER_SIZE] {
            let slot = crate::SignatureSlot::new(size).unwrap();
            assert_eq!(slot.placeholder_size(), size);
            assert_eq!(slot.reserved_bytes(), size as u64 + 2);
            let plan = slot.plan(500);
            assert_eq!(plan.first_length, 500);
            assert_eq!(plan.second_start, 500 + size as u64 + 2);
            assert_eq!(plan.as_array(), None);
        }
        assert_eq!(
            crate::SignatureSlot::new(7),
            Err(crate::SignatureError::OddPlaceholderSize(7))
        );
    }

    #[test]
    fn malformed_or_ambiguous_placeholder_is_a_typed_error() {
        let placeholder = crate::SignatureSlot::new(8).unwrap();
        let bytes = placeholder.contents_placeholder();
        assert_eq!(
            plan_byte_range(b"no placeholder here", bytes),
            Err(ByteRangeError::PlaceholderNotFound)
        );
        assert_eq!(
            plan_byte_range(b"anything", b""),
            Err(ByteRangeError::InvalidPlaceholder)
        );
        assert_eq!(
            plan_byte_range(b"anything", b"xyz0"),
            Err(ByteRangeError::InvalidPlaceholder)
        );
        let mut doubled = Vec::new();
        for _ in 0..2 {
            doubled.push(b'<');
            doubled.extend_from_slice(bytes);
            doubled.push(b'>');
        }
        assert_eq!(
            plan_byte_range(&doubled, bytes),
            Err(ByteRangeError::AmbiguousPlaceholder(2))
        );
    }

    #[test]
    fn inconsistent_byte_range_is_rejected() {
        let placeholder = crate::SignatureSlot::new(16).unwrap();
        let bytes = placeholder.contents_placeholder();
        let document = document_with_placeholder(bytes);
        let good = plan_byte_range(&document, bytes).unwrap();
        let bad = planned_range(1, 4, document.len() as u64);
        assert!(matches!(
            validate_byte_range(&document, &bad, bytes),
            Err(ByteRangeError::InvalidRange { .. })
        ));
        let incomplete = crate::SignatureSlot::new(16).unwrap().plan(19);
        assert_eq!(
            validate_byte_range(&document, &incomplete, bytes),
            Err(ByteRangeError::IncompleteRange)
        );
        assert_eq!(
            signed_bytes(&document, &incomplete),
            Err(ByteRangeError::IncompleteRange)
        );
        assert!(good.as_array().is_some());
    }

    fn hex_placeholder() -> Vec<u8> {
        vec![b'0'; 16]
    }

    fn signed_lifecycle(field: &str) -> SignatureLifecycle {
        let mut lifecycle = SignatureLifecycle::new(
            field,
            SignatureDictionary::new()
                .with_name("Ada")
                .with_reason("OK"),
        )
        .unwrap();
        let plan = planned_range(100, 18, 200);
        lifecycle
            .request_sign(signer(), "2026-11-19T09:00:00Z", plan, &hex_placeholder())
            .unwrap();
        lifecycle
            .attach_signature_bytes(signer(), "2026-11-19T10:00:00Z", vec![0x30, 0x45])
            .unwrap();
        lifecycle
    }

    /// A signed lifecycle whose plan is derived from a real document so that
    /// `verify` can re-derive and validate the plan against the same bytes.
    fn signed_lifecycle_for(
        document: &[u8],
        placeholder: &[u8],
        field: &str,
    ) -> SignatureLifecycle {
        let plan = plan_byte_range(document, placeholder).unwrap();
        let mut lifecycle = SignatureLifecycle::new(field, SignatureDictionary::new()).unwrap();
        lifecycle
            .request_sign(signer(), "2026-11-19T09:00:00Z", plan, placeholder)
            .unwrap();
        lifecycle
            .attach_signature_bytes(signer(), "2026-11-19T10:00:00Z", vec![0x30, 0x45])
            .unwrap();
        lifecycle
    }

    #[test]
    fn attach_bytes_updates_completed_v() {
        let lifecycle = signed_lifecycle("sig");
        assert_eq!(lifecycle.state(), SignatureStatus::Signed);
        assert_eq!(lifecycle.contents(), Some(&[0x30, 0x45][..]));
        let v = lifecycle.completed_v().unwrap();
        let dict = v.as_dict().unwrap();
        assert_eq!(dict.get("Contents").unwrap().as_text(), Some("3045"));
        let range = dict.get("ByteRange").unwrap().as_array().unwrap();
        assert_eq!(
            range,
            &vec![
                PdfValue::Integer(0),
                PdfValue::Integer(100),
                PdfValue::Integer(118),
                PdfValue::Integer(82),
            ]
        );
        assert_eq!(dict.get("Name").unwrap().as_text(), Some("Ada"));
        assert_eq!(dict.get("Reason").unwrap().as_text(), Some("OK"));
    }

    #[derive(Default)]
    struct CountingVerifier {
        calls: Cell<usize>,
    }

    impl SignatureVerifier for CountingVerifier {
        fn verify(
            &self,
            request: &VerificationRequest<'_>,
        ) -> Result<VerificationOutcome, VerificationError> {
            self.calls.set(self.calls.get() + 1);
            StructureOnlyVerifier.verify(request)
        }
    }

    struct FailingVerifier;

    impl SignatureVerifier for FailingVerifier {
        fn verify(
            &self,
            _request: &VerificationRequest<'_>,
        ) -> Result<VerificationOutcome, VerificationError> {
            Err(VerificationError::EmptyContents)
        }
    }

    /// A stand-in for a real cryptographic verifier from the security lane.
    struct CryptographicVerifier;

    impl SignatureVerifier for CryptographicVerifier {
        fn verify(
            &self,
            _request: &VerificationRequest<'_>,
        ) -> Result<VerificationOutcome, VerificationError> {
            Ok(VerificationOutcome::with_authenticity(
                true,
                "crypto-ok",
                Authenticity::Cryptographic,
            ))
        }
    }

    #[test]
    fn verifier_is_invoked_and_outcome_is_recorded() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");
        let verifier = CountingVerifier::default();
        let outcome = lifecycle
            .verify(&verifier, &document, "2026-11-19T11:00:00Z")
            .unwrap();
        assert!(outcome.is_valid());
        assert_eq!(verifier.calls.get(), 1);
        assert_eq!(lifecycle.state(), SignatureStatus::Valid);
        assert_eq!(lifecycle.verification(), Some(&outcome));
        assert!(lifecycle.report().to_string().contains("structure-ok:"));
    }

    #[test]
    fn verification_failure_is_a_typed_error_and_marks_invalid() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");
        let error = lifecycle
            .verify(&FailingVerifier, &document, "2026-11-19T11:00:00Z")
            .unwrap_err();
        assert_eq!(
            error,
            LifecycleError::Verification(VerificationError::EmptyContents)
        );
        assert_eq!(lifecycle.state(), SignatureStatus::Invalid);
        assert_eq!(lifecycle.failure(), Some(&VerificationError::EmptyContents));
        assert!(lifecycle.report().failure.is_some());
        assert!(lifecycle.state().is_terminal());
    }

    fn fill_workflow_with_signature() -> FillWorkflow {
        let specs = [
            FieldSpec::text("notes", rect(), 0),
            FieldSpec::signature("sig", rect(), 0),
        ];
        let document = FillDocument::from_specs(&specs, ObjectRef::new(1, 0)).unwrap();
        FillWorkflow::new(document)
    }

    fn settled_workflow(field: &str) -> SignatureWorkflow {
        let mut workflow = SignatureWorkflow::new(field).unwrap();
        workflow
            .sign(signer(), "2026-11-19T10:00:00Z", None, None, None)
            .unwrap();
        workflow
    }

    #[test]
    fn lock_enforcement_covers_fill_and_attach_paths() {
        let guard =
            SignatureGuard::new(LockScope::Fields, None).with_locked_fields(["notes", "sig"]);
        let mut workflow = fill_workflow_with_signature();
        workflow.install_signature_guard(guard);

        assert!(matches!(
            workflow.set("notes", crate::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(_))
        ));
        assert!(matches!(
            workflow.clear("notes"),
            Err(FillWorkflowError::Guard(_))
        ));
        assert!(matches!(
            workflow.attach_signature(settled_workflow("sig")),
            Err(FillWorkflowError::Guard(_))
        ));
    }

    #[test]
    fn certification_enforcement_covers_every_mutation_path() {
        let guard = SignatureGuard::new(LockScope::Fields, Some(CertPermission::NoChanges));
        let mut workflow = fill_workflow_with_signature();
        workflow.install_signature_guard(guard);

        assert!(matches!(
            workflow.set("notes", crate::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(_))
        ));
        assert!(matches!(
            workflow.reset_field("notes"),
            Err(FillWorkflowError::Guard(_))
        ));
        assert!(matches!(
            workflow.attach_signature(settled_workflow("sig")),
            Err(FillWorkflowError::Guard(_))
        ));
    }

    #[test]
    fn workflow_transitions_cover_all_states() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = SignatureLifecycle::new("sig", SignatureDictionary::new()).unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Unsigned);
        let plan = plan_byte_range(&document, &placeholder).unwrap();
        lifecycle
            .request_sign(signer(), "2026-11-19T09:00:00Z", plan, &placeholder)
            .unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Pending);
        lifecycle
            .attach_signature_bytes(signer(), "2026-11-19T10:00:00Z", vec![0x30, 0x45])
            .unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Signed);
        lifecycle
            .verify(&StructureOnlyVerifier, &document, "2026-11-19T11:00:00Z")
            .unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Valid);

        let transitions = lifecycle.report().transitions().to_vec();
        assert_eq!(transitions.len(), 3);
        assert_eq!(transitions[0].from(), SignatureStatus::Unsigned);
        assert_eq!(transitions[0].to(), SignatureStatus::Pending);
        assert_eq!(transitions[1].to(), SignatureStatus::Signed);
        assert_eq!(transitions[2].to(), SignatureStatus::Valid);
        assert_eq!(
            workflow_transition_lines(lifecycle.workflow()),
            lifecycle.report().to_lines()[7..].to_vec()
        );
    }

    #[test]
    fn lifecycle_report_is_deterministic() {
        let a = signed_lifecycle("sig");
        let b = signed_lifecycle("sig");
        assert_eq!(a.report(), b.report());
        assert_eq!(a.report().to_lines(), b.report().to_lines());
        assert_eq!(a.report().to_string(), b.report().to_string());
        assert_eq!(a.report().to_lines()[0], "lifecycle.field=sig");
        assert_eq!(a.report().to_lines()[1], "lifecycle.status=signed");
        assert_eq!(a.report().transitions().len(), 2);
    }

    #[test]
    fn missing_field_and_stage_errors_are_typed() {
        let mut workflow = fill_workflow_with_signature();
        let error = workflow
            .attach_signature(settled_workflow("absent"))
            .unwrap_err();
        assert_eq!(
            error,
            FillWorkflowError::UnknownSignatureField("absent".to_owned())
        );

        let mut lifecycle = SignatureLifecycle::new("sig", SignatureDictionary::new()).unwrap();
        assert_eq!(
            lifecycle.attach_signature_bytes(signer(), "t", vec![1]),
            Err(LifecycleError::NotPending {
                actual: SignatureStatus::Unsigned
            })
        );
        assert_eq!(
            lifecycle.verify(&StructureOnlyVerifier, b"doc", "t"),
            Err(LifecycleError::NotSigned {
                actual: SignatureStatus::Unsigned
            })
        );
        assert_eq!(
            lifecycle.completed_v(),
            Err(LifecycleError::MissingContents)
        );
    }

    // --- Regression tests: signed_bytes range validation (T-0185) ---

    #[test]
    fn signed_bytes_rejects_overlapping_ranges() {
        let document = vec![b'x'; 200];
        let plan = ByteRangePlan {
            first_start: 0,
            first_length: 100,
            second_start: 50, // begins before the first range ends
            total_length: Some(200),
        };
        assert_eq!(
            signed_bytes(&document, &plan),
            Err(ByteRangeError::InvalidRangeOrder)
        );
    }

    #[test]
    fn signed_bytes_rejects_empty_first_range() {
        let document = vec![b'x'; 100];
        let plan = ByteRangePlan {
            first_start: 0,
            first_length: 0,
            second_start: 10,
            total_length: Some(100),
        };
        assert_eq!(
            signed_bytes(&document, &plan),
            Err(ByteRangeError::EmptyRange)
        );
    }

    #[test]
    fn signed_bytes_rejects_empty_second_range() {
        let document = vec![b'x'; 100];
        let plan = ByteRangePlan {
            first_start: 0,
            first_length: 10,
            second_start: 20,
            total_length: Some(20), // second length saturates to zero
        };
        assert_eq!(
            signed_bytes(&document, &plan),
            Err(ByteRangeError::EmptyRange)
        );
    }

    #[test]
    fn signed_bytes_rejects_ranges_beyond_the_document() {
        let document = vec![b'x'; 50];
        let beyond_second = ByteRangePlan {
            first_start: 0,
            first_length: 10,
            second_start: 20,
            total_length: Some(100),
        };
        assert_eq!(
            signed_bytes(&document, &beyond_second),
            Err(ByteRangeError::OutOfBounds)
        );
        let beyond_first = ByteRangePlan {
            first_start: 0,
            first_length: 60,
            second_start: 70,
            total_length: Some(100),
        };
        assert_eq!(
            signed_bytes(&document, &beyond_first),
            Err(ByteRangeError::OutOfBounds)
        );
    }

    #[test]
    fn signed_bytes_rejects_nonzero_first_start() {
        let document = vec![b'x'; 100];
        let plan = ByteRangePlan {
            first_start: 5,
            first_length: 10,
            second_start: 20,
            total_length: Some(100),
        };
        assert_eq!(
            signed_bytes(&document, &plan),
            Err(ByteRangeError::OutOfBounds)
        );
    }

    // --- Regression tests: verify validates the caller-supplied plan (T-0185) ---

    #[test]
    fn verify_rejects_a_plan_that_does_not_match_the_document() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");

        // Prepending bytes moves the placeholder, so the stored plan no longer
        // describes this document even though it is structurally complete.
        let mut other = b"%PDF-1.7\n% appended header\n".to_vec();
        other.extend_from_slice(&document);
        let error = lifecycle
            .verify(&StructureOnlyVerifier, &other, "2026-11-19T11:00:00Z")
            .unwrap_err();
        assert!(matches!(
            error,
            LifecycleError::ByteRange(ByteRangeError::InvalidRange { .. })
        ));
        assert_eq!(lifecycle.state(), SignatureStatus::Signed);
    }

    #[test]
    fn verify_rejects_a_document_without_the_placeholder() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");
        let error = lifecycle
            .verify(&StructureOnlyVerifier, b"no placeholder here", "t")
            .unwrap_err();
        assert_eq!(
            error,
            LifecycleError::ByteRange(ByteRangeError::PlaceholderNotFound)
        );
        assert_eq!(lifecycle.state(), SignatureStatus::Signed);
    }

    // --- Regression tests: explicit authenticity level (T-0185) ---

    #[test]
    fn authenticity_defaults_to_structure_only() {
        let lifecycle = SignatureLifecycle::new("sig", SignatureDictionary::new()).unwrap();
        assert_eq!(Authenticity::default(), Authenticity::StructureOnly);
        assert_eq!(lifecycle.report().authenticity, Authenticity::StructureOnly);
        assert!(
            lifecycle
                .report()
                .to_lines()
                .contains(&"lifecycle.authenticity=structure-only".to_owned())
        );
    }

    #[test]
    fn structure_only_verifier_reports_structure_only_authenticity() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");
        let outcome = lifecycle
            .verify(&StructureOnlyVerifier, &document, "t")
            .unwrap();
        assert!(outcome.is_valid());
        // A valid structure-only result must not be presented as authentic.
        assert_eq!(outcome.authenticity(), Authenticity::StructureOnly);
        let report = lifecycle.report();
        assert_eq!(report.status, SignatureStatus::Valid);
        assert_eq!(report.authenticity, Authenticity::StructureOnly);
        assert!(
            report
                .to_lines()
                .contains(&"lifecycle.authenticity=structure-only".to_owned())
        );
    }

    #[test]
    fn authenticity_transitions_deterministically_to_cryptographic() {
        let placeholder = hex_placeholder();
        let document = document_with_placeholder(&placeholder);
        let before = signed_lifecycle_for(&document, &placeholder, "sig");
        assert_eq!(before.report().authenticity, Authenticity::StructureOnly);

        let mut lifecycle = signed_lifecycle_for(&document, &placeholder, "sig");
        let outcome = lifecycle
            .verify(&CryptographicVerifier, &document, "t")
            .unwrap();
        assert_eq!(outcome.authenticity(), Authenticity::Cryptographic);
        let report = lifecycle.report();
        assert_eq!(report.authenticity, Authenticity::Cryptographic);
        assert!(
            report
                .to_lines()
                .contains(&"lifecycle.authenticity=cryptographic".to_owned())
        );

        // Repeated runs produce the same report (deterministic transition).
        let mut again = signed_lifecycle_for(&document, &placeholder, "sig");
        again
            .verify(&CryptographicVerifier, &document, "t")
            .unwrap();
        assert_eq!(again.report(), report);
    }

    // --- Regression test: exact integer byte-range emission (T-0185) ---

    #[test]
    fn byte_range_offsets_beyond_f64_exact_limit_are_exact_integers() {
        // 2^53 + 1 is the first integer a `u64 as f64` cast cannot represent.
        let huge = 9_007_199_254_740_993u64;
        let plan = ByteRangePlan {
            first_start: 0,
            first_length: 10,
            second_start: 10 + huge,
            total_length: Some(10 + huge + huge),
        };
        let mut lifecycle = SignatureLifecycle::new("sig", SignatureDictionary::new()).unwrap();
        lifecycle
            .request_sign(signer(), "t", plan, &hex_placeholder())
            .unwrap();
        lifecycle
            .attach_signature_bytes(signer(), "t", vec![0x30, 0x45])
            .unwrap();
        let v = lifecycle.completed_v().unwrap();
        let range = v
            .as_dict()
            .unwrap()
            .get("ByteRange")
            .unwrap()
            .as_array()
            .unwrap();
        assert_eq!(
            range,
            &vec![
                PdfValue::Integer(0),
                PdfValue::Integer(10),
                PdfValue::Integer(10 + huge),
                PdfValue::Integer(huge),
            ]
        );

        // End-to-end: the emitted PDF carries the exact decimal integers and
        // neither a rounded value nor scientific notation.
        let mut field = BTreeMap::new();
        field.insert("T".to_owned(), PdfValue::Text("sig".to_owned()));
        field.insert("FT".to_owned(), PdfValue::Name("Sig".to_owned()));
        field.insert("V".to_owned(), v);
        let mut widget = BTreeMap::new();
        widget.insert("T".to_owned(), PdfValue::Text("sig".to_owned()));
        widget.insert("FT".to_owned(), PdfValue::Name("Sig".to_owned()));
        widget.insert("Subtype".to_owned(), PdfValue::Name("Widget".to_owned()));
        let bytes = crate::serialize_form_pdf(
            &[PdfValue::Dictionary(field)],
            &[PdfValue::Dictionary(widget)],
            Some(ObjectRef::new(1, 0)),
        )
        .unwrap();
        // The PDF header contains a binary marker, so decode lossily.
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            text.contains("/ByteRange [0 10 9007199254741003 9007199254740993]"),
            "expected exact integer emission, got: {text}"
        );
        assert!(
            !text.contains("9007199254740992"),
            "rounded f64 value leaked into output: {text}"
        );
        assert!(
            !text.contains("e15"),
            "scientific notation leaked into output: {text}"
        );
    }
}
