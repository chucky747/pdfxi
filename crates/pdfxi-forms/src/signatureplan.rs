//! Deterministic signature-field planning and workflow enforcement.
//!
//! This module closes the library-level signature workflow at the object-model
//! boundary. [`SignaturePlan`] takes a set of [`SignatureField`]s and produces
//! the *exact* `/FT /Sig` widget and `/V` placeholder dictionary contract a
//! signer expects: required keys are present, and cryptographic material
//! (`/Contents`, `/ByteRange`) is explicitly absent. [`PlaceholderContract`]
//! checks that contract on any dictionary without generating or forging a
//! signature.
//!
//! A plan also derives a [`SignatureGuard`] from the fields' `/Lock` and DocMDP
//! certification entries. Installing that guard into a [`crate::FillWorkflow`]
//! enforces lock scopes (`fields`, `all`, `document`) and certification levels
//! (no changes vs form-fill allowed) on every subsequent fill. All reports are
//! deterministic and no cryptography, clock, filesystem or network is used.

use crate::acroform::PdfValue;
use crate::signature::{
    CertPermission, DEFAULT_PLACEHOLDER_SIZE, LockScope, SignatureError, SignatureField,
    SignatureGuard, SignatureSlot, WorkflowReport,
};
use std::collections::BTreeSet;
use std::fmt;

/// A typed contract violation for a signature placeholder dictionary.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum ContractError {
    /// A required key was absent.
    #[error("signature placeholder is missing required key '/{0}'")]
    MissingKey(&'static str),
    /// A key that must be absent for a placeholder was present.
    #[error("signature placeholder contains forbidden key '/{0}'")]
    ForbiddenKey(&'static str),
    /// A present key had the wrong type or value.
    #[error("signature placeholder key '/{0}' has an unexpected value")]
    WrongValue(&'static str),
}

/// A typed error produced while building a [`SignaturePlan`].
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum SignaturePlanError {
    /// A signature field was malformed.
    #[error("signature field error: {0}")]
    Signature(#[from] SignatureError),
    /// Two signature fields shared a fully qualified name.
    #[error("duplicate signature field name '{0}'")]
    DuplicateField(String),
    /// A generated dictionary violated the placeholder contract.
    #[error("signature placeholder contract violation: {0}")]
    Contract(#[from] ContractError),
}

/// The exact dictionary-key contract for a signature placeholder.
///
/// A `/V` signature dictionary must carry `/Type /Sig`, `/Filter` and
/// `/SubFilter`; it must **not** carry `/Contents` or `/ByteRange`, which a
/// security-layer signer supplies. The field node carries `/FT /Sig`, `/T`,
/// `/Ff`, `/V` and `/Kids`, and its single widget carries `/Type /Annot`,
/// `/Subtype /Widget`, `/Rect` and `/P`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PlaceholderContract;

impl PlaceholderContract {
    /// Keys that must be present in the `/V` placeholder.
    pub const REQUIRED_V_KEYS: [&'static str; 3] = ["Type", "Filter", "SubFilter"];
    /// Cryptographic keys that must be absent from the `/V` placeholder.
    pub const FORBIDDEN_V_KEYS: [&'static str; 2] = ["Contents", "ByteRange"];
    /// Keys that must be present in the signature field node.
    pub const REQUIRED_FIELD_KEYS: [&'static str; 6] = ["Type", "FT", "T", "Ff", "V", "Kids"];
    /// Keys that must be present in the signature widget annotation.
    pub const REQUIRED_WIDGET_KEYS: [&'static str; 4] = ["Type", "Subtype", "Rect", "P"];

    /// Checks the `/V` signature dictionary placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] when a required key is missing, a cryptographic
    /// key is present, or a required name value is malformed.
    pub fn check_v(&self, value: &PdfValue) -> Result<(), ContractError> {
        let dict = value.as_dict().ok_or(ContractError::WrongValue("/V"))?;
        for key in Self::REQUIRED_V_KEYS {
            if !dict.contains_key(key) {
                return Err(ContractError::MissingKey(key));
            }
        }
        if dict.get("Type").and_then(PdfValue::as_name) != Some("Sig") {
            return Err(ContractError::WrongValue("Type"));
        }
        if dict
            .get("Filter")
            .and_then(PdfValue::as_name)
            .is_none_or(str::is_empty)
        {
            return Err(ContractError::WrongValue("Filter"));
        }
        if dict
            .get("SubFilter")
            .and_then(PdfValue::as_name)
            .is_none_or(str::is_empty)
        {
            return Err(ContractError::WrongValue("SubFilter"));
        }
        for key in Self::FORBIDDEN_V_KEYS {
            if dict.contains_key(key) {
                return Err(ContractError::ForbiddenKey(key));
            }
        }
        Ok(())
    }

    /// Checks the signature field node and its single widget placeholder.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] for a missing key, a non-`/Sig` field type or
    /// a nested `/V` or widget that fails its own check.
    pub fn check_field(&self, value: &PdfValue) -> Result<(), ContractError> {
        let dict = value.as_dict().ok_or(ContractError::WrongValue("field"))?;
        for key in Self::REQUIRED_FIELD_KEYS {
            if !dict.contains_key(key) {
                return Err(ContractError::MissingKey(key));
            }
        }
        if dict.get("FT").and_then(PdfValue::as_name) != Some("Sig") {
            return Err(ContractError::WrongValue("FT"));
        }
        let v = dict.get("V").ok_or(ContractError::MissingKey("V"))?;
        self.check_v(v)?;
        let kids = dict
            .get("Kids")
            .and_then(PdfValue::as_array)
            .ok_or(ContractError::WrongValue("Kids"))?;
        let widget = kids.first().ok_or(ContractError::MissingKey("Kids"))?;
        self.check_widget(widget)
    }

    /// Checks the signature widget annotation dictionary.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] for a missing key or a non-`/Widget` subtype.
    pub fn check_widget(&self, value: &PdfValue) -> Result<(), ContractError> {
        let dict = value.as_dict().ok_or(ContractError::WrongValue("widget"))?;
        for key in Self::REQUIRED_WIDGET_KEYS {
            if !dict.contains_key(key) {
                return Err(ContractError::MissingKey(key));
            }
        }
        if dict.get("Subtype").and_then(PdfValue::as_name) != Some("Widget") {
            return Err(ContractError::WrongValue("Subtype"));
        }
        Ok(())
    }

    /// Returns stable lines describing this contract.
    pub fn to_lines(&self) -> Vec<String> {
        let mut lines = Vec::new();
        for key in Self::REQUIRED_V_KEYS {
            lines.push(format!("v.required={key}"));
        }
        for key in Self::FORBIDDEN_V_KEYS {
            lines.push(format!("v.forbidden={key}"));
        }
        for key in Self::REQUIRED_FIELD_KEYS {
            lines.push(format!("field.required={key}"));
        }
        for key in Self::REQUIRED_WIDGET_KEYS {
            lines.push(format!("widget.required={key}"));
        }
        lines
    }
}

/// One planned signature field and its emitted placeholder dictionaries.
#[derive(Debug, Clone, PartialEq)]
pub struct SignatureFieldPlanEntry {
    field: String,
    field_dictionary: PdfValue,
    widget: PdfValue,
    v_placeholder: PdfValue,
    reserved_bytes: u64,
}

impl SignatureFieldPlanEntry {
    /// Returns the fully qualified field name.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the emitted field-node dictionary.
    pub fn field_dictionary(&self) -> &PdfValue {
        &self.field_dictionary
    }

    /// Returns the emitted widget annotation dictionary.
    pub fn widget(&self) -> &PdfValue {
        &self.widget
    }

    /// Returns the `/V` signature dictionary placeholder.
    pub fn v_placeholder(&self) -> &PdfValue {
        &self.v_placeholder
    }

    /// Returns the number of bytes reserved for the eventual `/Contents`.
    pub const fn reserved_bytes(&self) -> u64 {
        self.reserved_bytes
    }
}

fn strictest_certification(
    left: Option<CertPermission>,
    right: Option<CertPermission>,
) -> Option<CertPermission> {
    match (left, right) {
        (Some(a), Some(b)) => Some(if a.bits() <= b.bits() { a } else { b }),
        (Some(a), None) | (None, Some(a)) => Some(a),
        (None, None) => None,
    }
}

/// A deterministic plan over one or more signature fields.
///
/// [`SignaturePlan::new`] emits and checks every placeholder dictionary, sorts
/// entries by field name, and derives a [`SignatureGuard`] from all locks and
/// certifications in the plan.
#[derive(Debug, Clone, PartialEq)]
pub struct SignaturePlan {
    entries: Vec<SignatureFieldPlanEntry>,
    placeholder_size: usize,
    guard: SignatureGuard,
}

impl SignaturePlan {
    /// Builds a plan from signature fields with a `/Contents` placeholder size.
    ///
    /// The default size is [`DEFAULT_PLACEHOLDER_SIZE`] hex characters; the
    /// value must be even, non-zero and no larger than
    /// [`crate::MAX_PLACEHOLDER_SIZE`], as enforced by [`SignatureSlot`].
    ///
    /// # Errors
    ///
    /// Returns a typed [`SignaturePlanError`] for a malformed field, a duplicate
    /// name, an invalid placeholder size, or a contract violation.
    pub fn new(
        fields: &[SignatureField],
        placeholder_size: usize,
    ) -> Result<Self, SignaturePlanError> {
        let slot = SignatureSlot::new(placeholder_size)?;
        let mut seen = BTreeSet::new();
        let mut entries = Vec::with_capacity(fields.len());
        let mut guard = SignatureGuard::new(LockScope::Fields, None);

        for field in fields {
            if !seen.insert(field.name().to_owned()) {
                return Err(SignaturePlanError::DuplicateField(field.name().to_owned()));
            }
            let field_dictionary = field.field_dictionary()?;
            let v_placeholder = field.signature_dictionary();
            let widget = field_dictionary
                .as_dict()
                .and_then(|dict| dict.get("Kids"))
                .and_then(PdfValue::as_array)
                .and_then(|kids| kids.first())
                .cloned()
                .ok_or(ContractError::MissingKey("Kids"))?;
            entries.push(SignatureFieldPlanEntry {
                field: field.name().to_owned(),
                field_dictionary,
                widget,
                v_placeholder,
                reserved_bytes: slot.reserved_bytes(),
            });

            let field_guard = match field.lock() {
                Some(lock) => {
                    SignatureGuard::from_lock(lock, field.certification().map(|c| c.permission()))
                }
                None => SignatureGuard::new(
                    LockScope::Fields,
                    field.certification().map(|c| c.permission()),
                ),
            };
            guard = combine_guards(guard, field_guard);
        }
        entries.sort_by(|a, b| a.field.cmp(&b.field));

        let plan = Self {
            entries,
            placeholder_size,
            guard,
        };
        plan.check_contracts()?;
        Ok(plan)
    }

    /// Builds a plan with the default placeholder size.
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::new`].
    pub fn with_defaults(fields: &[SignatureField]) -> Result<Self, SignaturePlanError> {
        Self::new(fields, DEFAULT_PLACEHOLDER_SIZE)
    }

    /// Returns the planned entries in ascending field-name order.
    pub fn entries(&self) -> &[SignatureFieldPlanEntry] {
        &self.entries
    }

    /// Returns the configured `/Contents` placeholder size.
    pub const fn placeholder_size(&self) -> usize {
        self.placeholder_size
    }

    /// Returns the number of planned signature fields.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when the plan has no fields.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Returns the guard derived from all fields' locks and certifications.
    pub const fn guard(&self) -> &SignatureGuard {
        &self.guard
    }

    /// Verifies the placeholder contract for every planned field.
    ///
    /// # Errors
    ///
    /// Returns [`ContractError`] for the first dictionary that violates the
    /// contract.
    pub fn check_contracts(&self) -> Result<(), ContractError> {
        let contract = PlaceholderContract;
        for entry in &self.entries {
            contract.check_field(&entry.field_dictionary)?;
            contract.check_v(&entry.v_placeholder)?;
            contract.check_widget(&entry.widget)?;
        }
        Ok(())
    }

    /// Returns a deterministic report for this plan.
    pub fn report(&self) -> SignaturePlanReport {
        let mut lines = vec![
            format!("signature.fields={}", self.entries.len()),
            format!("signature.placeholder_size={}", self.placeholder_size),
        ];
        lines.extend(self.guard.report_lines());
        let contract = PlaceholderContract;
        lines.extend(contract.to_lines());
        for entry in &self.entries {
            let v = entry.v_placeholder.as_dict();
            let filter = v
                .and_then(|dict| dict.get("Filter"))
                .and_then(PdfValue::as_name)
                .unwrap_or("unknown");
            let subfilter = v
                .and_then(|dict| dict.get("SubFilter"))
                .and_then(PdfValue::as_name)
                .unwrap_or("unknown");
            lines.push(format!("field.{}.FT=Sig", entry.field));
            lines.push(format!("field.{}.V.Filter={filter}", entry.field));
            lines.push(format!("field.{}.V.SubFilter={subfilter}", entry.field));
            lines.push(format!("field.{}.V.Contents=absent", entry.field));
            lines.push(format!("field.{}.V.ByteRange=absent", entry.field));
            lines.push(format!(
                "field.{}.reserved_bytes={}",
                entry.field, entry.reserved_bytes
            ));
        }
        SignaturePlanReport {
            field_count: self.entries.len(),
            placeholder_size: self.placeholder_size,
            lock_scope: self.guard.scope(),
            certification: self.guard.certification().map(CertPermission::bits),
            lines,
        }
    }
}

fn combine_guards(left: SignatureGuard, right: SignatureGuard) -> SignatureGuard {
    let scope = left.scope().stricter(right.scope());
    let certification = strictest_certification(left.certification(), right.certification());
    SignatureGuard::new(scope, certification)
        .with_locked_fields(left.locked_fields().map(str::to_owned))
        .with_locked_fields(right.locked_fields().map(str::to_owned))
        .with_excluded_fields(left.excluded_fields().map(str::to_owned))
        .with_excluded_fields(right.excluded_fields().map(str::to_owned))
}

/// A deterministic status report for a [`SignaturePlan`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SignaturePlanReport {
    /// Number of planned signature fields.
    pub field_count: usize,
    /// The `/Contents` placeholder size in hex characters.
    pub placeholder_size: usize,
    /// The combined lock scope.
    pub lock_scope: LockScope,
    /// The strictest certification `/P` value, if any.
    pub certification: Option<u32>,
    lines: Vec<String>,
}

impl SignaturePlanReport {
    /// Returns the report as stable `key=value` lines.
    pub fn to_lines(&self) -> Vec<String> {
        self.lines.clone()
    }

    /// Returns a stable one-line summary.
    pub fn summary_line(&self) -> String {
        format!(
            "{} signature field(s), placeholder {}, lock scope {}, certification {}",
            self.field_count,
            self.placeholder_size,
            self.lock_scope,
            self.certification
                .map(|value| value.to_string())
                .unwrap_or_else(|| "none".to_owned())
        )
    }
}

impl fmt::Display for SignaturePlanReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.lines.join("\n"))
    }
}

/// Renders signature workflow reports as deterministic, name-sorted lines.
///
/// The input order does not affect the result; reports are keyed by their
/// `field` name and emitted in ascending order.
pub fn workflow_report_lines(reports: &[WorkflowReport]) -> Vec<String> {
    let mut ordered: Vec<&WorkflowReport> = reports.iter().collect();
    ordered.sort_by(|a, b| a.field.cmp(&b.field));
    let mut lines = vec![format!("workflow.count={}", ordered.len())];
    for report in ordered {
        lines.push(format!(
            "workflow.{}.status={}",
            report.field, report.status
        ));
        lines.push(format!(
            "workflow.{}.transitions={}",
            report.field, report.transition_count
        ));
    }
    lines
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::WidgetRect;
    use crate::build::FieldSpec;
    use crate::fillworkflow::{FillDocument, FillWorkflow, FillWorkflowError};
    use crate::signature::{
        Certification, LockAction, Participant, ParticipantRole, SignatureDictionary,
        SignatureGuardError, SignatureLock, SignatureStatus, SignatureWorkflow,
    };

    fn rect() -> WidgetRect {
        WidgetRect::new(10.0, 20.0, 180.0, 40.0)
    }

    fn fill_workflow() -> FillWorkflow {
        let specs = [
            FieldSpec::text("notes", rect(), 0),
            FieldSpec::text("other", rect(), 0),
        ];
        let document =
            FillDocument::from_specs(&specs, crate::acroform::ObjectRef::new(1, 0)).unwrap();
        FillWorkflow::new(document)
    }

    fn signed_workflow(field: &str) -> SignatureWorkflow {
        let participant = Participant::new("Ada", ParticipantRole::Signer).unwrap();
        let mut workflow = SignatureWorkflow::new(field).unwrap();
        workflow
            .sign(participant, "2026-01-01T00:00:00Z", None, None, None)
            .unwrap();
        workflow
    }

    #[test]
    fn placeholder_contract_accepts_generated_dictionaries() {
        let fields = [SignatureField::new("order.sig", rect(), 0)];
        let plan = SignaturePlan::with_defaults(&fields).unwrap();
        plan.check_contracts().unwrap();
        let entry = &plan.entries()[0];
        let v = entry.v_placeholder().as_dict().unwrap();
        assert_eq!(v.get("Type").unwrap().as_name(), Some("Sig"));
        assert!(v.get("Contents").is_none());
        assert!(v.get("ByteRange").is_none());
        assert_eq!(
            entry.reserved_bytes(),
            SignatureSlot::default_size().unwrap().reserved_bytes()
        );
    }

    #[test]
    fn placeholder_contract_rejects_forged_signature_material() {
        let contract = PlaceholderContract;
        let mut v = SignatureDictionary::default()
            .to_pdf()
            .as_dict()
            .unwrap()
            .clone();
        contract.check_v(&PdfValue::Dictionary(v.clone())).unwrap();

        v.insert("Contents".to_owned(), PdfValue::Text("00".repeat(4)));
        assert_eq!(
            contract.check_v(&PdfValue::Dictionary(v.clone())),
            Err(ContractError::ForbiddenKey("Contents"))
        );
        v.remove("Contents");
        v.insert("ByteRange".to_owned(), PdfValue::Array(Vec::new()));
        assert_eq!(
            contract.check_v(&PdfValue::Dictionary(v.clone())),
            Err(ContractError::ForbiddenKey("ByteRange"))
        );
        v.remove("ByteRange");
        v.remove("SubFilter");
        assert_eq!(
            contract.check_v(&PdfValue::Dictionary(v)),
            Err(ContractError::MissingKey("SubFilter"))
        );
    }

    #[test]
    fn lock_scope_fields_blocks_only_listed_field() {
        let lock = SignatureLock::new(LockAction::Include, ["notes"]);
        let guard = SignatureGuard::from_lock(&lock, None);
        assert_eq!(guard.scope(), LockScope::Fields);
        let mut workflow = fill_workflow();
        workflow.install_signature_guard(guard);

        assert!(matches!(
            workflow.set("notes", crate::field::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(SignatureGuardError::FieldLocked(name))) if name == "notes"
        ));
        workflow
            .set("other", crate::field::FieldValue::Text("y".into()))
            .unwrap();
        assert_eq!(
            workflow.value("other").unwrap(),
            &crate::field::FieldValue::Text("y".into())
        );
    }

    #[test]
    fn lock_scope_all_and_document_block_every_field() {
        for scope in [LockScope::All, LockScope::Document] {
            let mut workflow = fill_workflow();
            workflow.install_signature_guard(SignatureGuard::new(scope, None));
            assert!(matches!(
                workflow.set("notes", crate::field::FieldValue::Text("x".into())),
                Err(FillWorkflowError::Guard(_))
            ));
            assert!(matches!(
                workflow.set("other", crate::field::FieldValue::Text("y".into())),
                Err(FillWorkflowError::Guard(_))
            ));
        }
    }

    #[test]
    fn exclude_lock_leaves_named_fields_fillable() {
        let lock = SignatureLock::new(LockAction::Exclude, ["other"]);
        let guard = SignatureGuard::from_lock(&lock, None);
        assert_eq!(guard.scope(), LockScope::All);
        let mut workflow = fill_workflow();
        workflow.install_signature_guard(guard);
        workflow
            .set("other", crate::field::FieldValue::Text("ok".into()))
            .unwrap();
        assert!(matches!(
            workflow.set("notes", crate::field::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(_))
        ));
    }

    #[test]
    fn certification_no_changes_blocks_all_edits() {
        let guard = SignatureGuard::new(LockScope::Fields, Some(CertPermission::NoChanges));
        let mut workflow = fill_workflow();
        workflow.install_signature_guard(guard);
        assert!(matches!(
            workflow.set("notes", crate::field::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(
                SignatureGuardError::CertificationLocked
            ))
        ));
        assert!(matches!(
            workflow.reset_field("notes"),
            Err(FillWorkflowError::Guard(
                SignatureGuardError::CertificationLocked
            ))
        ));
    }

    #[test]
    fn certification_form_fill_allows_unlocked_fields() {
        let guard = SignatureGuard::new(LockScope::Fields, Some(CertPermission::FormAndSigning))
            .with_locked_fields(["notes"]);
        let mut workflow = fill_workflow();
        workflow.install_signature_guard(guard);
        assert!(matches!(
            workflow.set("notes", crate::field::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(SignatureGuardError::FieldLocked(
                _
            )))
        ));
        workflow
            .set("other", crate::field::FieldValue::Text("y".into()))
            .unwrap();
    }

    #[test]
    fn signature_plan_guard_is_enforced_by_fill_workflow() {
        let field = SignatureField::new("sig", rect(), 0)
            .with_lock(SignatureLock::new(LockAction::Include, ["notes"]))
            .with_certification(Certification::new(CertPermission::FormAndSigning));
        let plan = SignaturePlan::with_defaults(&[field]).unwrap();
        let mut workflow = fill_workflow();
        workflow.install_signature_guard(plan.guard().clone());
        assert!(matches!(
            workflow.set("notes", crate::field::FieldValue::Text("x".into())),
            Err(FillWorkflowError::Guard(_))
        ));
        workflow
            .set("other", crate::field::FieldValue::Text("y".into()))
            .unwrap();
    }

    #[test]
    fn signature_plan_entries_are_sorted_and_unique() {
        let first = SignatureField::new("zeta.sig", rect(), 0);
        let second = SignatureField::new("alpha.sig", rect(), 0);
        let plan = SignaturePlan::with_defaults(&[first.clone(), second.clone()]).unwrap();
        assert_eq!(plan.len(), 2);
        assert_eq!(plan.entries()[0].field(), "alpha.sig");
        assert_eq!(plan.entries()[1].field(), "zeta.sig");
        assert!(matches!(
            SignaturePlan::with_defaults(&[first.clone(), first]),
            Err(SignaturePlanError::DuplicateField(_))
        ));
    }

    #[test]
    fn signature_plan_report_is_deterministic() {
        let a = SignatureField::new("zeta.sig", rect(), 0)
            .with_lock(SignatureLock::new(LockAction::Include, ["notes"]));
        let b = SignatureField::new("alpha.sig", rect(), 0)
            .with_certification(Certification::new(CertPermission::NoChanges));
        let plan_ab = SignaturePlan::with_defaults(&[a.clone(), b.clone()]).unwrap();
        let plan_ba = SignaturePlan::with_defaults(&[b, a]).unwrap();
        assert_eq!(plan_ab.report(), plan_ba.report());
        assert_eq!(plan_ab.report().to_lines(), plan_ba.report().to_lines());
        assert_eq!(plan_ab.report(), plan_ab.report());
        assert!(
            plan_ab
                .report()
                .summary_line()
                .contains("lock scope fields")
        );
        assert!(plan_ab.report().summary_line().contains("certification 1"));
    }

    #[test]
    fn workflow_report_lines_are_deterministic() {
        let first = signed_workflow("zeta.sig").report();
        let second = signed_workflow("alpha.sig").report();
        let ordered = workflow_report_lines(&[first.clone(), second.clone()]);
        let reversed = workflow_report_lines(&[second, first.clone()]);
        assert_eq!(ordered, reversed);
        assert_eq!(ordered[0], "workflow.count=2");
        assert!(ordered[1].starts_with("workflow.alpha.sig.status="));
        assert!(ordered.iter().any(|line| line.ends_with("status=signed")));
        assert_eq!(first.status, SignatureStatus::Signed);
    }
}
