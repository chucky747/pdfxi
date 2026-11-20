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
/// Returns [`ByteRangeError`] for an incomplete plan, an out-of-bounds range or
/// an offset that cannot be represented as a `usize`.
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
    if first_start != 0 || first_end > total || second_end > total || second_start > second_end {
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

/// The deterministic outcome of a verification attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VerificationOutcome {
    valid: bool,
    detail: String,
}

impl VerificationOutcome {
    /// Creates an outcome with `valid` and a caller-supplied detail string.
    pub fn new(valid: bool, detail: impl Into<String>) -> Self {
        Self {
            valid,
            detail: detail.into(),
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
/// bytes are all non-empty and returns a stable `structure-ok` outcome. It
/// performs **no cryptographic validation** and must not be used to claim that
/// a signature is authentic; it exists so a caller can exercise the full
/// lifecycle without importing the security crate.
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
            contents: None,
            verification: None,
            failure: None,
        })
    }

    /// Starts a sign request (`unsigned -> pending`) with a byte-range plan.
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
    ) -> Result<(), LifecycleError> {
        if self.workflow.state() != SignatureStatus::Unsigned {
            return Err(LifecycleError::NotUnsigned {
                actual: self.workflow.state(),
            });
        }
        self.workflow.invite(participant, timestamp)?;
        self.byte_range = Some(byte_range);
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
    /// subfilter and attached contents. On acceptance the state becomes `valid`
    /// and the outcome is recorded; on rejection the state becomes `invalid`
    /// and the typed error is recorded.
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
    /// `/ByteRange` is the complete `[a b c d]` array from the plan. This is
    /// *serialization*, not signing: no byte is hashed or transformed except
    /// hex encoding. The placeholder metadata (`/Name`, `/Reason`, `/Lock`,
    /// `/Reference`, …) is preserved.
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
            PdfValue::Array(
                range
                    .iter()
                    .map(|value| PdfValue::Number(*value as f64))
                    .collect(),
            ),
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
            .request_sign(signer(), "2026-11-19T09:00:00Z", plan)
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
                PdfValue::Number(0.0),
                PdfValue::Number(100.0),
                PdfValue::Number(118.0),
                PdfValue::Number(82.0),
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

    #[test]
    fn verifier_is_invoked_and_outcome_is_recorded() {
        let mut lifecycle = signed_lifecycle("sig");
        let document = vec![b'x'; 200];
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
        let mut lifecycle = signed_lifecycle("sig");
        let document = vec![b'x'; 200];
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
        let mut lifecycle = SignatureLifecycle::new("sig", SignatureDictionary::new()).unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Unsigned);
        lifecycle
            .request_sign(signer(), "2026-11-19T09:00:00Z", planned_range(10, 18, 100))
            .unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Pending);
        lifecycle
            .attach_signature_bytes(signer(), "2026-11-19T10:00:00Z", vec![1, 2, 3])
            .unwrap();
        assert_eq!(lifecycle.state(), SignatureStatus::Signed);
        lifecycle
            .verify(&StructureOnlyVerifier, &[b'z'; 100], "2026-11-19T11:00:00Z")
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
            lifecycle.report().to_lines()[6..].to_vec()
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
}
