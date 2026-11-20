//! Headless, end-to-end AcroForm fill workflow.
//!
//! [`FillWorkflow`] ties the crate's existing pieces into one deterministic,
//! dependency-free pipeline:
//!
//! 1. A caller supplies a parsed document model as a [`FillDocument`] — either
//!    built from typed [`FieldSpec`]s ([`FillDocument::from_specs`]) or from an
//!    already-loaded [`Form`] plus its matching `/AcroForm` dictionary model
//!    ([`FillDocument::from_parts`]).
//! 2. Values are written through [`FillWorkflow::set`] / [`FillWorkflow::clear`].
//!    Read-only, required, type-shape and unknown-name checks are inherited from
//!    [`Form::set_value`]; content and choice constraints are checked by
//!    [`Form::validate`].
//! 3. [`FillWorkflow::plan`] applies the final values to a *clone* of the
//!    dictionary model with [`fill_acroform`], which regenerates every widget's
//!    `/AP /N` appearance stream, and collects the result into a
//!    [`FilledDocumentPlan`]. The original document model is never mutated by
//!    planning, so a workflow may be planned repeatedly.
//! 4. [`FilledDocumentPlan::serialize`] emits concrete PDF bytes with the
//!    T-0140 [`serialize_form_pdf`](crate::serialize_form_pdf) serializer, and
//!    [`FilledDocumentPlan::verify`] parses them back with
//!    [`parse_pdf`](crate::parse_pdf) and reads the filled `/V` values to prove
//!    the round trip. [`FillWorkflow::finish`] performs validate → plan →
//!    serialize → verify in one call.
//!
//! # Signature boundary
//!
//! Signature fields are **not** fillable through [`FillWorkflow::set`]; that
//! returns [`FillWorkflowError::SignatureMustUseWorkflow`]. They can only be
//! settled by attaching a [`SignatureWorkflow`](crate::SignatureWorkflow) whose
//! state has reached `Signed` or `Valid` via [`FillWorkflow::attach_signature`].
//! An `Unsigned` or `Pending` workflow is rejected with
//! [`FillWorkflowError::SignatureNotReady`]. No signature bytes, `/Contents`
//! value or `/ByteRange` are ever written: the plan records the workflow
//! *state transition* only. Cryptographic signing and verification belong to
//! `pdfxi-security`; this module deliberately performs neither.
//!
//! No filesystem, network, clock or JavaScript access is involved. All output
//! is deterministic and no function here uses `unwrap`, `expect`, `panic!` or
//! unsafe code.

use crate::acrofill::{AcroFillReport, FieldAssignment, fill_acroform};
use crate::acroform::{ObjectRef, PdfValue, coerce_value};
use crate::build::{CreationError, FieldSpec, acroform_dictionary};
use crate::field::{FieldType, FieldValue, ValueKind};
use crate::form::{Form, FormError};
use crate::serialize::{self, MAX_OBJECT_NUMBER, ParseError, SerializeError};
use crate::signature::{
    SignatureGuard, SignatureGuardError, SignatureStatus, SignatureWorkflow, TransitionKind,
};
use crate::validation::Validation;
use std::collections::BTreeMap;
use std::fmt;

/// A typed error produced by a headless fill workflow.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum FillWorkflowError {
    /// The caller-supplied document model had an unexpected structure.
    #[error("malformed fill model: {0}")]
    Malformed(String),
    /// A generic form-model error that is not one of the field-write cases.
    #[error("form model error: {0}")]
    Form(#[from] FormError),
    /// A caller-supplied field specification failed validation.
    #[error("field creation error: {0}")]
    Creation(#[from] CreationError),
    /// No field with the requested fully qualified name exists.
    #[error("no field named '{0}'")]
    FieldNotFound(String),
    /// The target field carries the read-only flag.
    #[error("field '{0}' is read-only")]
    ReadOnlyField(String),
    /// A required field was assigned an empty value.
    #[error("required field '{0}' cannot be cleared")]
    RequiredField(String),
    /// The value's representation does not fit the target field's type.
    #[error("field '{field}' holds a {found:?} value; expected {expected}")]
    TypeMismatch {
        /// Fully qualified name of the target field.
        field: String,
        /// Slash-separated accepted value categories.
        expected: String,
        /// The rejected value's category.
        found: ValueKind,
    },
    /// A signature field was written directly instead of through its workflow.
    #[error("signature field '{0}' cannot be filled directly; use the signature workflow")]
    SignatureMustUseWorkflow(String),
    /// A signature workflow had not reached a signed state yet.
    #[error(
        "signature field '{field}' is not ready (state {state}); unsigned and pending signatures are blocked"
    )]
    SignatureNotReady {
        /// Fully qualified name of the signature field.
        field: String,
        /// The workflow's current state.
        state: SignatureStatus,
    },
    /// A signature lock or certification refused a field write.
    #[error(transparent)]
    Guard(#[from] SignatureGuardError),
    /// No signature field with the requested name exists.
    #[error("no signature field named '{0}'")]
    UnknownSignatureField(String),
    /// A workflow was already attached to the named signature field.
    #[error("a signature workflow is already attached to field '{0}'")]
    DuplicateSignatureWorkflow(String),
    /// The final form state failed validation.
    #[error("validation failed with {count} error(s)")]
    ValidationFailed {
        /// Number of validation errors.
        count: usize,
        /// The full validation result.
        validation: Validation,
    },
    /// The plan could not be serialized into a PDF byte stream.
    #[error("serialization error: {0}")]
    Serialize(#[from] SerializeError),
    /// A serialized PDF could not be parsed back for verification.
    #[error("PDF parse error: {0}")]
    Parse(#[from] ParseError),
    /// A value read back from the PDF did not match the planned value.
    #[error("verification mismatch for '{field}': expected {expected:?}, found {found:?}")]
    VerificationMismatch {
        /// Fully qualified name of the mismatched field.
        field: String,
        /// The value stored in the plan.
        expected: FieldValue,
        /// The value read back from the serialized PDF.
        found: FieldValue,
    },
}

fn map_form_error(error: FormError) -> FillWorkflowError {
    match error {
        FormError::FieldNotFound(name) => FillWorkflowError::FieldNotFound(name),
        FormError::ReadOnlyField(name) => FillWorkflowError::ReadOnlyField(name),
        FormError::RequiredField(name) => FillWorkflowError::RequiredField(name),
        FormError::TypeMismatch {
            field,
            expected,
            found,
        } => FillWorkflowError::TypeMismatch {
            field,
            expected,
            found,
        },
        other => FillWorkflowError::Form(other),
    }
}

/// A caller-supplied, parsed AcroForm document model ready for filling.
///
/// The model pairs the in-memory [`Form`] (names, types, flags, values) with the
/// `/AcroForm` dictionary model ([`PdfValue`]) that carries widget geometry and
/// is the input to appearance generation and serialization. A `page` object
/// reference identifies the real page that owns the widgets.
#[derive(Debug, Clone, PartialEq)]
pub struct FillDocument {
    form: Form,
    acroform: PdfValue,
    page: ObjectRef,
}

impl FillDocument {
    /// Builds a document model from caller-supplied typed field specs.
    ///
    /// This is the convenient path when the caller has already parsed or
    /// authored the form as [`FieldSpec`]s: one pass builds the in-memory form
    /// and the matching `/AcroForm` dictionary model.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::Creation`] when a spec is malformed,
    /// [`FillWorkflowError::Form`] for duplicate field names, or
    /// [`FillWorkflowError::Malformed`] for a bad page reference.
    pub fn from_specs(specs: &[FieldSpec], page: ObjectRef) -> Result<Self, FillWorkflowError> {
        let acroform = acroform_dictionary(specs)?;
        let mut fields = Vec::with_capacity(specs.len());
        for spec in specs {
            fields.push(spec.to_field()?);
        }
        let form = Form::from_fields(fields)?;
        Self::from_parts(form, acroform, page)
    }

    /// Builds a document model from a caller-supplied form and dictionary model.
    ///
    /// Use this when the model comes from another producer (for example a host
    /// document adapter). The dictionary must be a `/AcroForm` dictionary with
    /// a `/Fields` array whose entries each carry a non-empty `/T`, and every
    /// form field must have a matching dictionary entry.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::Malformed`] when the page reference is
    /// invalid, the dictionary has the wrong shape, or a form field has no
    /// matching dictionary entry.
    pub fn from_parts(
        form: Form,
        acroform: PdfValue,
        page: ObjectRef,
    ) -> Result<Self, FillWorkflowError> {
        if page.number == 0 || page.number > MAX_OBJECT_NUMBER {
            return Err(FillWorkflowError::Malformed(
                "invalid owning page reference".to_owned(),
            ));
        }
        let names = dict_field_names(&acroform)?;
        for field in form.fields() {
            if !names.iter().any(|name| name == field.name()) {
                return Err(FillWorkflowError::Malformed(format!(
                    "form field '{}' has no matching AcroForm dictionary entry",
                    field.name()
                )));
            }
        }
        Ok(Self {
            form,
            acroform,
            page,
        })
    }

    /// Returns the in-memory form model.
    pub const fn form(&self) -> &Form {
        &self.form
    }

    /// Returns the `/AcroForm` dictionary model.
    pub const fn acroform(&self) -> &PdfValue {
        &self.acroform
    }

    /// Returns the real page object reference that owns the widgets.
    pub const fn page(&self) -> ObjectRef {
        self.page
    }
}

fn dict_field_names(acroform: &PdfValue) -> Result<Vec<String>, FillWorkflowError> {
    let dict = acroform
        .as_dict()
        .ok_or_else(|| FillWorkflowError::Malformed("AcroForm is not a dictionary".to_owned()))?;
    let fields = dict
        .get("Fields")
        .and_then(PdfValue::as_array)
        .ok_or_else(|| {
            FillWorkflowError::Malformed("AcroForm /Fields is not an array".to_owned())
        })?;
    fields
        .iter()
        .map(|entry| {
            entry
                .as_dict()
                .and_then(|dict| dict.get("T"))
                .and_then(PdfValue::as_text)
                .filter(|name| !name.is_empty())
                .map(str::to_owned)
                .ok_or_else(|| {
                    FillWorkflowError::Malformed("field entry has no non-empty /T text".to_owned())
                })
        })
        .collect()
}

/// One recorded signature state transition in a fill plan.
///
/// This is a *workflow* record: no cryptographic material is carried. It lets a
/// host show that a signature field was settled through the signing workflow
/// and which transition produced that state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePlanRecord {
    /// Fully qualified name of the signature field.
    pub field: String,
    /// The workflow status at planning time.
    pub status: SignatureStatus,
    /// Kind of the most recent recorded transition, if any.
    pub last_kind: Option<TransitionKind>,
    /// Source state of the most recent transition, if any.
    pub last_from: Option<SignatureStatus>,
    /// Target state of the most recent transition, if any.
    pub last_to: Option<SignatureStatus>,
    /// Caller-supplied timestamp of the most recent transition, if any.
    pub last_timestamp: Option<String>,
    /// Name of the participant recorded by the workflow, if any.
    pub participant: Option<String>,
    /// Total number of transitions recorded by the workflow.
    pub transition_count: usize,
}

impl SignaturePlanRecord {
    fn from_workflow(workflow: &SignatureWorkflow) -> Self {
        let report = workflow.report();
        let last = workflow.history().last();
        Self {
            field: report.field,
            status: report.status,
            last_kind: last.map(|transition| transition.kind()),
            last_from: last.map(|transition| transition.from()),
            last_to: last.map(|transition| transition.to()),
            last_timestamp: last.map(|transition| transition.timestamp().to_owned()),
            participant: report.participant_name,
            transition_count: report.transition_count,
        }
    }
}

/// A deterministic plan describing the filled document.
///
/// Produced by [`FillWorkflow::plan`]. Holds the field and widget dictionary
/// models with regenerated appearances, the fill report, the validation
/// snapshot, and the recorded signature workflow transitions.
#[derive(Debug, Clone, PartialEq)]
pub struct FilledDocumentPlan {
    /// Field-node dictionaries, each with a written `/V` and one widget `/Kids`.
    pub fields: Vec<PdfValue>,
    /// Widget dictionaries carrying regenerated `/AP /N` appearance streams.
    pub widgets: Vec<PdfValue>,
    /// Owning page object reference used for serialization.
    pub page: ObjectRef,
    /// Name-sorted report of appearance updates produced by `fill_acroform`.
    pub fill: AcroFillReport,
    /// Validation snapshot of the final form state.
    pub validation: Validation,
    /// Recorded signature transitions, sorted by field name.
    pub signatures: Vec<SignaturePlanRecord>,
    field_types: BTreeMap<String, FieldType>,
    expected: BTreeMap<String, FieldValue>,
}

impl FilledDocumentPlan {
    /// Returns the field type recorded for `name`, if present.
    pub fn field_type(&self, name: &str) -> Option<FieldType> {
        self.field_types.get(name).copied()
    }

    /// Returns the value the plan expects for `name` after filling.
    pub fn expected_value(&self, name: &str) -> Option<&FieldValue> {
        self.expected.get(name)
    }

    /// Returns the recorded signature transition for `name`, if any.
    pub fn signature_record(&self, name: &str) -> Option<&SignaturePlanRecord> {
        self.signatures.iter().find(|record| record.field == name)
    }

    /// Serializes this plan into concrete PDF bytes.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::Serialize`] when the dictionaries are
    /// malformed or the page reference is invalid.
    pub fn serialize(&self) -> Result<Vec<u8>, FillWorkflowError> {
        Ok(serialize::serialize_form_pdf(
            &self.fields,
            &self.widgets,
            Some(self.page),
        )?)
    }

    /// Parses `pdf` back and verifies that every non-signature field's `/V`
    /// matches the value recorded in this plan.
    ///
    /// Signature fields are skipped because no signature bytes are written by
    /// this crate; their state is represented by [`FilledDocumentPlan::signatures`].
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::Parse`] for an unreadable PDF,
    /// [`FillWorkflowError::Malformed`] for an unexpected dictionary shape, or
    /// [`FillWorkflowError::VerificationMismatch`] when a value differs.
    pub fn verify(&self, pdf: &[u8]) -> Result<Verification, FillWorkflowError> {
        let document = serialize::parse_pdf(pdf)?;
        let catalog = document.catalog()?;
        let acroform_ref = catalog
            .get("AcroForm")
            .ok_or_else(|| FillWorkflowError::Malformed("catalog has no /AcroForm".to_owned()))?;
        let acroform = document.resolve(acroform_ref)?;
        let acroform = acroform.as_dict().ok_or_else(|| {
            FillWorkflowError::Malformed("/AcroForm is not a dictionary".to_owned())
        })?;
        let field_refs = acroform
            .get("Fields")
            .and_then(PdfValue::as_array)
            .ok_or_else(|| {
                FillWorkflowError::Malformed("AcroForm /Fields is not an array".to_owned())
            })?;

        let mut checked = 0usize;
        let mut signatures = 0usize;
        for field_ref in field_refs {
            let resolved = document.resolve(field_ref)?;
            let dict = resolved.as_dict().ok_or_else(|| {
                FillWorkflowError::Malformed("field is not a dictionary".to_owned())
            })?;
            let name = dict
                .get("T")
                .and_then(PdfValue::as_text)
                .ok_or_else(|| FillWorkflowError::Malformed("field has no /T text".to_owned()))?;
            if self.field_type(name) == Some(FieldType::Signature) {
                signatures += 1;
                continue;
            }
            // A PDF field with no explicit /V presents its /DV, so fall back to
            // the default exactly as `load_acroform` does. This makes a reset
            // field (current value equals its default, /V still absent) verify.
            let found_value = match dict.get("V") {
                Some(value) if !matches!(value, PdfValue::Null) => value.clone(),
                _ => dict.get("DV").cloned().unwrap_or(PdfValue::Null),
            };
            let field_type = self.field_type(name).unwrap_or(FieldType::Unknown);
            let found = coerce_value(&found_value, field_type).unwrap_or(FieldValue::Empty);
            let expected = self
                .expected
                .get(name)
                .cloned()
                .unwrap_or(FieldValue::Empty);
            if found != expected {
                return Err(FillWorkflowError::VerificationMismatch {
                    field: name.to_owned(),
                    expected,
                    found,
                });
            }
            checked += 1;
        }
        Ok(Verification {
            field_count: field_refs.len(),
            checked,
            signatures,
        })
    }
}

/// The result of parsing back a serialized filled document.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Verification {
    /// Total number of fields in the serialized `/AcroForm`.
    pub field_count: usize,
    /// Number of non-signature fields whose `/V` matched the plan.
    pub checked: usize,
    /// Number of signature fields intentionally skipped (no bytes written).
    pub signatures: usize,
}

impl Verification {
    /// Returns `true` when every field was either verified or a signature field.
    pub const fn is_complete(&self) -> bool {
        self.checked + self.signatures == self.field_count
    }

    /// Returns a stable one-line description of the verification.
    pub fn summary_line(&self) -> String {
        format!(
            "verified {} field(s), {} signature field(s) skipped, {} total",
            self.checked, self.signatures, self.field_count
        )
    }
}

impl fmt::Display for Verification {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary_line())
    }
}

/// A completed, verified headless fill.
#[derive(Debug, Clone, PartialEq)]
pub struct FilledDocument {
    /// The deterministic plan that produced the document.
    pub plan: FilledDocumentPlan,
    /// Concrete serialized PDF bytes.
    pub pdf: Vec<u8>,
    /// The parse-back verification result.
    pub verification: Verification,
}

/// A headless AcroForm filling session.
///
/// Create one from a [`FillDocument`], write ordinary fields with [`Self::set`],
/// attach settled signature workflows with [`Self::attach_signature`], and call
/// [`Self::finish`] to validate, plan, serialize and verify in one step.
#[derive(Debug, Clone, PartialEq)]
pub struct FillWorkflow {
    document: FillDocument,
    signature_workflows: BTreeMap<String, SignatureWorkflow>,
    signature_records: Vec<SignaturePlanRecord>,
    guard: Option<SignatureGuard>,
}

impl FillWorkflow {
    /// Starts a filling session over `document`.
    pub fn new(document: FillDocument) -> Self {
        Self {
            document,
            signature_workflows: BTreeMap::new(),
            signature_records: Vec::new(),
            guard: None,
        }
    }

    /// Returns the underlying document model.
    pub const fn document(&self) -> &FillDocument {
        &self.document
    }

    /// Installs a signature lock/certification guard on this session.
    ///
    /// Every subsequent [`Self::set`], [`Self::clear`] and
    /// [`Self::reset_field`] is checked against the guard. A guard is typically
    /// derived from a [`crate::SignaturePlan`] built from the document's
    /// signature fields. Installing a new guard replaces the previous one.
    pub fn install_signature_guard(&mut self, guard: SignatureGuard) {
        self.guard = Some(guard);
    }

    /// Returns the installed signature guard, if any.
    pub const fn signature_guard(&self) -> Option<&SignatureGuard> {
        self.guard.as_ref()
    }

    fn check_guard(&self, name: &str) -> Result<(), FillWorkflowError> {
        if let Some(guard) = &self.guard {
            guard.check_fill(name)?;
        }
        Ok(())
    }

    /// Returns the current value of a named field.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::FieldNotFound`] for an unknown name.
    pub fn value(&self, name: &str) -> Result<&FieldValue, FillWorkflowError> {
        self.document.form.value(name).map_err(map_form_error)
    }

    /// Returns the workflow attached to `name`, if any.
    pub fn signature_workflow(&self, name: &str) -> Option<&SignatureWorkflow> {
        self.signature_workflows.get(name)
    }

    /// Writes one ordinary field value.
    ///
    /// Signature fields are refused with
    /// [`FillWorkflowError::SignatureMustUseWorkflow`]; use
    /// [`Self::attach_signature`] for those.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown, read-only, required, type-mismatched
    /// or signature field. All checks are delegated to [`Form::set_value`].
    pub fn set(&mut self, name: &str, value: FieldValue) -> Result<(), FillWorkflowError> {
        let field = self
            .document
            .form
            .field(name)
            .ok_or_else(|| FillWorkflowError::FieldNotFound(name.to_owned()))?;
        if field.field_type() == FieldType::Signature {
            return Err(FillWorkflowError::SignatureMustUseWorkflow(name.to_owned()));
        }
        self.check_guard(name)?;
        self.document
            .form
            .set_value(name, value)
            .map_err(map_form_error)
    }

    /// Clears a field by assigning [`FieldValue::Empty`].
    ///
    /// Clearing a required field is rejected; clearing a signature field is
    /// refused just like [`Self::set`].
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::set`].
    pub fn clear(&mut self, name: &str) -> Result<(), FillWorkflowError> {
        self.set(name, FieldValue::Empty)
    }

    /// Restores every writable field to its default and drops all attached
    /// signature workflows, records and signature guard.
    ///
    /// Resetting is an explicit "start over" operation, so it also removes the
    /// lock/certification guard installed by [`Self::install_signature_guard`].
    pub fn reset(&mut self) {
        self.document.form.reset();
        self.signature_workflows.clear();
        self.signature_records.clear();
        self.guard = None;
    }

    /// Restores one field to its default.
    ///
    /// Resetting a signature field also removes its attached workflow and
    /// plan record.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::FieldNotFound`] or
    /// [`FillWorkflowError::ReadOnlyField`].
    pub fn reset_field(&mut self, name: &str) -> Result<(), FillWorkflowError> {
        self.check_guard(name)?;
        let is_signature = self
            .document
            .form
            .field(name)
            .is_some_and(|field| field.field_type() == FieldType::Signature);
        if is_signature {
            self.signature_workflows.remove(name);
            self.signature_records.retain(|record| record.field != name);
        }
        self.document.form.reset_field(name).map_err(map_form_error)
    }

    /// Attaches a settled signature workflow to its signature field.
    ///
    /// The field must exist and be of type [`FieldType::Signature`], no other
    /// workflow may already be attached, and the workflow state must be
    /// `Signed` or `Valid`. `Unsigned` and `Pending` workflows are blocked with
    /// [`FillWorkflowError::SignatureNotReady`]. On success the in-memory field
    /// value becomes the participant's name (a model-level fill only) and a
    /// [`SignaturePlanRecord`] is recorded. No signature bytes are written.
    ///
    /// # Errors
    ///
    /// Returns a typed error for an unknown or non-signature field, a duplicate
    /// workflow, an unsettled workflow, or a read-only field.
    pub fn attach_signature(
        &mut self,
        workflow: SignatureWorkflow,
    ) -> Result<(), FillWorkflowError> {
        let field_name = workflow.field().to_owned();
        let field = self
            .document
            .form
            .field(&field_name)
            .ok_or_else(|| FillWorkflowError::UnknownSignatureField(field_name.clone()))?;
        if field.field_type() != FieldType::Signature {
            return Err(FillWorkflowError::UnknownSignatureField(field_name));
        }
        if self.signature_workflows.contains_key(&field_name) {
            return Err(FillWorkflowError::DuplicateSignatureWorkflow(field_name));
        }
        if !matches!(
            workflow.state(),
            SignatureStatus::Signed | SignatureStatus::Valid
        ) {
            return Err(FillWorkflowError::SignatureNotReady {
                field: field_name,
                state: workflow.state(),
            });
        }
        if let Some(participant) = workflow.participant() {
            self.document
                .form
                .set_value(&field_name, FieldValue::Text(participant.name().to_owned()))
                .map_err(map_form_error)?;
        }
        let record = SignaturePlanRecord::from_workflow(&workflow);
        self.signature_workflows.insert(field_name, workflow);
        self.signature_records.push(record);
        Ok(())
    }

    /// Validates the current form state.
    pub fn validate(&self) -> Validation {
        self.document.form.validate()
    }

    /// Builds a deterministic plan without requiring the form to be valid.
    ///
    /// This is useful for inspecting appearance updates or a validation
    /// snapshot even when fields are still missing. The source document model
    /// is left untouched. Signature fields are never written; read-only fields
    /// keep their caller-supplied dictionary entry and are not re-appeared.
    /// A fillable field whose value equals its default receives an explicit
    /// `/V` copied from `/DV` so strict readers see the value directly.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::Malformed`] when the filled dictionary
    /// model cannot be split into field and widget dictionaries.
    pub fn plan(&self) -> Result<FilledDocumentPlan, FillWorkflowError> {
        let validation = self.document.form.validate();
        let mut assignments = Vec::with_capacity(self.document.form.len());
        let mut field_types = BTreeMap::new();
        let mut expected = BTreeMap::new();
        for field in self.document.form.fields() {
            field_types.insert(field.name().to_owned(), field.field_type());
            expected.insert(field.name().to_owned(), field.value().clone());
            // Signature fields settle only through their workflow (no bytes),
            // and read-only fields keep their caller-supplied dictionary entry.
            if field.field_type() == FieldType::Signature || field.is_read_only() {
                continue;
            }
            assignments.push(FieldAssignment::new(
                field.name().to_owned(),
                field.value().clone(),
            ));
        }

        let mut acroform = self.document.acroform.clone();
        let fill = fill_acroform(&mut acroform, assignments);
        let (mut fields, widgets) = split_acroform(&acroform)?;
        // `fill_acroform` treats a value equal to the field's /DV as an
        // unchanged no-op, leaving /V absent. For a filled field we still want
        // an explicit /V so a strict reader sees the value directly; /DV already
        // holds the correctly normalised PDF representation in that case.
        for field in &mut fields {
            let PdfValue::Dictionary(dict) = field else {
                continue;
            };
            let Some(name) = dict.get("T").and_then(PdfValue::as_text).map(str::to_owned) else {
                continue;
            };
            let Some(form_field) = self.document.form.field(&name) else {
                continue;
            };
            if form_field.field_type() == FieldType::Signature
                || form_field.is_read_only()
                || form_field.value().is_empty()
            {
                continue;
            }
            let has_value =
                matches!(dict.get("V"), Some(value) if !matches!(value, PdfValue::Null));
            if !has_value && let Some(default) = dict.get("DV").cloned() {
                dict.insert("V".to_owned(), default);
            }
        }

        let mut signatures = self.signature_records.clone();
        signatures.sort_by(|left, right| left.field.cmp(&right.field));

        Ok(FilledDocumentPlan {
            fields,
            widgets,
            page: self.document.page,
            fill,
            validation,
            signatures,
            field_types,
            expected,
        })
    }

    /// Validates, plans, serializes and parses back in one call.
    ///
    /// # Errors
    ///
    /// Returns [`FillWorkflowError::ValidationFailed`] when the final form is
    /// invalid, or the plan/serialize/verify errors described on those methods.
    pub fn finish(self) -> Result<FilledDocument, FillWorkflowError> {
        let validation = self.document.form.validate();
        if !validation.is_valid() {
            return Err(FillWorkflowError::ValidationFailed {
                count: validation.error_count(),
                validation,
            });
        }
        let plan = self.plan()?;
        let pdf = plan.serialize()?;
        let verification = plan.verify(&pdf)?;
        Ok(FilledDocument {
            plan,
            pdf,
            verification,
        })
    }
}

fn split_acroform(
    acroform: &PdfValue,
) -> Result<(Vec<PdfValue>, Vec<PdfValue>), FillWorkflowError> {
    let dict = acroform
        .as_dict()
        .ok_or_else(|| FillWorkflowError::Malformed("AcroForm is not a dictionary".to_owned()))?;
    let fields = dict
        .get("Fields")
        .and_then(PdfValue::as_array)
        .ok_or_else(|| {
            FillWorkflowError::Malformed("AcroForm /Fields is not an array".to_owned())
        })?;
    let mut field_dicts = Vec::with_capacity(fields.len());
    let mut widget_dicts = Vec::with_capacity(fields.len());
    for entry in fields {
        let field = entry
            .as_dict()
            .ok_or_else(|| FillWorkflowError::Malformed("field is not a dictionary".to_owned()))?;
        let kids = field
            .get("Kids")
            .and_then(PdfValue::as_array)
            .ok_or_else(|| FillWorkflowError::Malformed("field has no /Kids array".to_owned()))?;
        let widget = kids.first().and_then(PdfValue::as_dict).ok_or_else(|| {
            FillWorkflowError::Malformed("field has no widget dictionary".to_owned())
        })?;
        if widget.get("Subtype").and_then(PdfValue::as_name) != Some("Widget") {
            return Err(FillWorkflowError::Malformed(
                "field kid is not a /Widget annotation".to_owned(),
            ));
        }
        field_dicts.push(entry.clone());
        widget_dicts.push(PdfValue::Dictionary(widget.clone()));
    }
    Ok((field_dicts, widget_dicts))
}

#[cfg(test)]
mod tests;
