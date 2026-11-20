//! Deterministic, headless editing of field properties.
//!
//! Where [`crate::properties`] describes a field's properties and validates
//! them, this module models what a property editor *does*: it publishes the
//! properties that apply to each [`FieldType`] ([`applicable_properties`]),
//! collects typed [`PropertyEdit`]s into a deterministic [`EditPlan`], validates
//! the plan atomically, and applies it to three views of the same field — the
//! [`PropertySet`], the emitted field dictionaries ([`FieldDictionarySet`]) and
//! the in-memory [`Form`] — then re-runs validation.
//!
//! An [`EditSnapshot`] captures all three views cheaply so a caller can undo an
//! apply. Nothing here reads a file, opens a UI, runs a script or uses the
//! clock. Every error is a typed [`EditError`], and all iteration is over
//! `BTreeMap`/`BTreeSet` so reports are byte-for-byte deterministic.

use crate::acroform::PdfValue;
use crate::build::{CreationError, FieldSpec};
use crate::field::{Field, FieldFlags, FieldType, TextConstraint};
use crate::form::Form;
use crate::properties::{FieldProperties, PropertyError, PropertySet};
use crate::rules::Format;
use crate::validation::ValidationSummary;
use std::collections::{BTreeMap, BTreeSet};
use std::fmt;

/// A single editable property of a form field.
///
/// The variants map onto the keys a property editor would show and, where a PDF
/// equivalent exists, onto a field-dictionary entry: `Tooltip` → `/TU`,
/// `Required`/`ReadOnly` → `/Ff`, `MaxLength` → `/MaxLen`, `Choices` → `/Opt`.
/// `Format` is a validation-only property with no direct PDF key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PropertyKey {
    /// The `/TU` tooltip.
    Tooltip,
    /// The required flag (`/Ff` bit 1).
    Required,
    /// The read-only flag (`/Ff` bit 2).
    ReadOnly,
    /// The maximum text length (`/MaxLen`).
    MaxLength,
    /// The expected value format (validation only).
    Format,
    /// The allowed choice/radio options (`/Opt`).
    Choices,
}

impl PropertyKey {
    /// Returns a stable lowercase identifier for this key.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tooltip => "tooltip",
            Self::Required => "required",
            Self::ReadOnly => "readonly",
            Self::MaxLength => "maxlength",
            Self::Format => "format",
            Self::Choices => "choices",
        }
    }

    /// Returns a stable human-readable label for this key.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Tooltip => "Tooltip",
            Self::Required => "Required",
            Self::ReadOnly => "Read only",
            Self::MaxLength => "Maximum length",
            Self::Format => "Format",
            Self::Choices => "Choices",
        }
    }

    /// Returns `true` when this property applies to `field_type`.
    ///
    /// - `Tooltip` applies to every field type.
    /// - `Required` applies to text, checkbox, radio and choice fields.
    /// - `ReadOnly` also applies to signature fields.
    /// - `MaxLength` and `Format` apply only to text fields.
    /// - `Choices` applies only to choice and radio fields.
    pub const fn applies_to(self, field_type: FieldType) -> bool {
        match self {
            Self::Tooltip => true,
            Self::Required => matches!(
                field_type,
                FieldType::Text | FieldType::CheckBox | FieldType::Radio | FieldType::Choice
            ),
            Self::ReadOnly => matches!(
                field_type,
                FieldType::Text
                    | FieldType::CheckBox
                    | FieldType::Radio
                    | FieldType::Choice
                    | FieldType::Signature
            ),
            Self::MaxLength | Self::Format => matches!(field_type, FieldType::Text),
            Self::Choices => matches!(field_type, FieldType::Choice | FieldType::Radio),
        }
    }
}

impl fmt::Display for PropertyKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// The typed value carried by a [`PropertyEdit`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PropertyValue {
    /// A `/TU` tooltip string.
    Tooltip(String),
    /// A Boolean flag value, used by `Required` and `ReadOnly`.
    Flag(bool),
    /// A maximum text length.
    MaxLength(usize),
    /// A value format.
    Format(Format),
    /// An ordered list of choice/radio options.
    Choices(Vec<String>),
}

impl PropertyValue {
    /// Returns the property key this value belongs to.
    pub const fn key(&self) -> PropertyKey {
        match self {
            Self::Tooltip(_) => PropertyKey::Tooltip,
            Self::Flag(_) => PropertyKey::Required,
            Self::MaxLength(_) => PropertyKey::MaxLength,
            Self::Format(_) => PropertyKey::Format,
            Self::Choices(_) => PropertyKey::Choices,
        }
    }
}

/// A structural description of one editable property.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PropertyDescriptor {
    key: PropertyKey,
    label: &'static str,
}

impl PropertyDescriptor {
    /// Returns the property key.
    pub const fn key(self) -> PropertyKey {
        self.key
    }

    /// Returns the human-readable label.
    pub const fn label(self) -> &'static str {
        self.label
    }

    /// Returns `true` when this property applies to `field_type`.
    pub const fn applies_to(self, field_type: FieldType) -> bool {
        self.key.applies_to(field_type)
    }
}

const ALL_KEYS: [PropertyKey; 6] = [
    PropertyKey::Tooltip,
    PropertyKey::Required,
    PropertyKey::ReadOnly,
    PropertyKey::MaxLength,
    PropertyKey::Format,
    PropertyKey::Choices,
];

/// Returns the editable property descriptors that apply to `field_type`.
///
/// The result is in stable key order, so a UI can render it directly without
/// sorting. This is the authoritative "which properties apply" table used by
/// [`EditPlan`] validation.
pub fn applicable_properties(field_type: FieldType) -> Vec<PropertyDescriptor> {
    ALL_KEYS
        .into_iter()
        .filter(|key| key.applies_to(field_type))
        .map(|key| PropertyDescriptor {
            key,
            label: key.label(),
        })
        .collect()
}

/// Whether a property is being set to a value or cleared.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EditOperation {
    /// Assign the supplied typed value.
    Set(PropertyValue),
    /// Remove the property, restoring the field's default.
    Clear,
}

/// One property edit targeting a fully qualified field name.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PropertyEdit {
    field: String,
    key: PropertyKey,
    operation: EditOperation,
}

impl PropertyEdit {
    /// Creates a `set` edit, rejecting a value whose kind does not match `key`.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::ValueKeyMismatch`] when `value.key() != key`.
    pub fn set(
        field: impl Into<String>,
        key: PropertyKey,
        value: PropertyValue,
    ) -> Result<Self, EditError> {
        if !value_matches_key(key, &value) {
            return Err(EditError::ValueKeyMismatch {
                key,
                expected: value.key(),
            });
        }
        Ok(Self {
            field: field.into(),
            key,
            operation: EditOperation::Set(value),
        })
    }

    /// Creates a `clear` edit for `key` on `field`.
    pub fn clear(field: impl Into<String>, key: PropertyKey) -> Self {
        Self {
            field: field.into(),
            key,
            operation: EditOperation::Clear,
        }
    }

    /// Returns the fully qualified target field name.
    pub fn field(&self) -> &str {
        &self.field
    }

    /// Returns the property key being edited.
    pub const fn key(&self) -> PropertyKey {
        self.key
    }

    /// Returns the requested operation.
    pub fn operation(&self) -> &EditOperation {
        &self.operation
    }
}

/// A typed error produced while planning or applying property edits.
#[derive(Debug, Clone, PartialEq, thiserror::Error)]
pub enum EditError {
    /// A value was supplied for the wrong property key.
    #[error("property '{key}' cannot hold a value for '{expected}'")]
    ValueKeyMismatch {
        /// The key the edit targeted.
        key: PropertyKey,
        /// The key the value actually belongs to.
        expected: PropertyKey,
    },
    /// The same field/property pair was edited more than once in a plan.
    #[error("duplicate edit for field '{field}' property '{key}'")]
    DuplicateEdit {
        /// Fully qualified field name.
        field: String,
        /// The duplicated property key.
        key: PropertyKey,
    },
    /// The target field does not exist.
    #[error("no field named '{0}'")]
    UnknownField(String),
    /// The property does not apply to the target field's type.
    #[error("property '{key}' does not apply to a {field_type} field '{field}'")]
    NotApplicable {
        /// Fully qualified field name.
        field: String,
        /// The inapplicable property key.
        key: PropertyKey,
        /// The field's type.
        field_type: FieldType,
    },
    /// The supplied value was structurally invalid.
    #[error("invalid value for field '{field}' property '{key}': {reason}")]
    InvalidValue {
        /// Fully qualified field name.
        field: String,
        /// The property key.
        key: PropertyKey,
        /// Why the value was rejected.
        reason: String,
    },
    /// Applying the edits would leave an invalid property definition.
    #[error("invalid property definition for field '{field}': {reason}")]
    InvalidDefinition {
        /// Fully qualified field name.
        field: String,
        /// Why the resulting definition was rejected.
        reason: String,
    },
    /// A field specification was malformed.
    #[error("field creation error: {0}")]
    Creation(#[from] CreationError),
    /// The underlying property model rejected a definition.
    #[error("property model error: {0}")]
    Property(#[from] PropertyError),
}

/// A `Flag` value is valid for both Boolean flag keys; all other values must
/// match their single key exactly.
fn value_matches_key(key: PropertyKey, value: &PropertyValue) -> bool {
    match (key, value) {
        (PropertyKey::Required | PropertyKey::ReadOnly, PropertyValue::Flag(_)) => true,
        _ => value.key() == key,
    }
}

fn operation_rank(operation: &EditOperation) -> u8 {
    match operation {
        EditOperation::Set(_) => 0,
        EditOperation::Clear => 1,
    }
}

/// A deterministic, validated sequence of property edits.
///
/// `EditPlan::new` rejects duplicate field/property pairs and orders the edits
/// by field name, then property key, then operation, so two callers supplying
/// the same edits in different orders derive the identical plan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPlan {
    edits: Vec<PropertyEdit>,
}

impl EditPlan {
    /// Builds a plan from `edits`.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::DuplicateEdit`] when a field/property pair appears
    /// more than once. Value/key mismatches are rejected earlier by
    /// [`PropertyEdit::set`], but are rechecked here for defensive safety.
    pub fn new(edits: Vec<PropertyEdit>) -> Result<Self, EditError> {
        let mut seen = BTreeSet::new();
        for edit in &edits {
            if let EditOperation::Set(value) = &edit.operation
                && !value_matches_key(edit.key, value)
            {
                return Err(EditError::ValueKeyMismatch {
                    key: edit.key,
                    expected: value.key(),
                });
            }
            if !seen.insert((edit.field.clone(), edit.key)) {
                return Err(EditError::DuplicateEdit {
                    field: edit.field.clone(),
                    key: edit.key,
                });
            }
        }
        let mut edits = edits;
        edits.sort_by(|a, b| {
            a.field
                .cmp(&b.field)
                .then_with(|| a.key.cmp(&b.key))
                .then_with(|| operation_rank(&a.operation).cmp(&operation_rank(&b.operation)))
        });
        Ok(Self { edits })
    }

    /// Returns the ordered edits.
    pub fn edits(&self) -> &[PropertyEdit] {
        &self.edits
    }

    /// Returns the number of edits.
    pub fn len(&self) -> usize {
        self.edits.len()
    }

    /// Returns `true` when the plan has no edits.
    pub fn is_empty(&self) -> bool {
        self.edits.is_empty()
    }
}

/// The result of applying an [`EditPlan`].
#[derive(Debug, Clone, PartialEq)]
pub struct EditReport {
    /// Number of `set` operations applied.
    pub applied: usize,
    /// Number of `clear` operations applied.
    pub cleared: usize,
    /// Validation snapshot recomputed after the edits.
    pub validation: ValidationSummary,
}

impl EditReport {
    /// Returns `true` when the post-edit validation had no errors.
    pub fn is_valid(&self) -> bool {
        self.validation.is_valid()
    }

    /// Returns stable report lines in a fixed order.
    pub fn to_lines(&self) -> Vec<String> {
        vec![
            format!("applied={}", self.applied),
            format!("cleared={}", self.cleared),
            format!("validation={}", self.validation.summary_line()),
        ]
    }
}

/// A cheap, cloneable snapshot of every view mutated by an [`EditPlan`].
#[derive(Debug, Clone)]
pub struct EditSnapshot {
    form: Form,
    properties: PropertySet,
    dictionaries: FieldDictionarySet,
}

impl EditSnapshot {
    /// Returns the captured form model.
    pub const fn form(&self) -> &Form {
        &self.form
    }

    /// Returns the captured property set.
    pub const fn properties(&self) -> &PropertySet {
        &self.properties
    }

    /// Returns the captured field dictionaries.
    pub const fn dictionaries(&self) -> &FieldDictionarySet {
        &self.dictionaries
    }
}

/// Emitted field dictionaries keyed by fully qualified field name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FieldDictionarySet {
    dicts: BTreeMap<String, PdfValue>,
}

impl FieldDictionarySet {
    /// Emits one field dictionary per spec, keyed by the spec's name.
    ///
    /// # Errors
    ///
    /// Returns [`CreationError`] when any spec is malformed.
    pub fn from_specs(specs: &[FieldSpec]) -> Result<Self, CreationError> {
        let mut dicts = BTreeMap::new();
        for spec in specs {
            dicts.insert(spec.name().to_owned(), spec.field_dictionary()?);
        }
        Ok(Self { dicts })
    }

    /// Returns the dictionary for `name`, if present.
    pub fn get(&self, name: &str) -> Option<&PdfValue> {
        self.dicts.get(name)
    }

    /// Returns all dictionaries in ascending field-name order.
    pub fn dicts(&self) -> impl ExactSizeIterator<Item = (&str, &PdfValue)> {
        self.dicts
            .iter()
            .map(|(name, value)| (name.as_str(), value))
    }

    /// Returns the number of dictionaries.
    pub fn len(&self) -> usize {
        self.dicts.len()
    }

    /// Returns `true` when the set is empty.
    pub fn is_empty(&self) -> bool {
        self.dicts.is_empty()
    }
}

/// The headless property editor coupling properties, dictionaries and form.
#[derive(Debug, Clone, PartialEq)]
pub struct PropertyEditor {
    properties: PropertySet,
    dictionaries: FieldDictionarySet,
}

impl PropertyEditor {
    /// Builds an editor from typed field specifications.
    ///
    /// # Errors
    ///
    /// Returns [`EditError::Creation`] for a malformed spec or
    /// [`EditError::Property`] for duplicate field names.
    pub fn from_specs(specs: &[FieldSpec]) -> Result<Self, EditError> {
        let dictionaries = FieldDictionarySet::from_specs(specs)?;
        let mut properties = PropertySet::new();
        for spec in specs {
            properties.add(FieldProperties::from_spec(spec))?;
        }
        Ok(Self {
            properties,
            dictionaries,
        })
    }

    /// Builds an editor directly from its two model views.
    pub fn from_parts(properties: PropertySet, dictionaries: FieldDictionarySet) -> Self {
        Self {
            properties,
            dictionaries,
        }
    }

    /// Returns the editable property set.
    pub const fn properties(&self) -> &PropertySet {
        &self.properties
    }

    /// Returns the emitted field dictionaries.
    pub const fn dictionaries(&self) -> &FieldDictionarySet {
        &self.dictionaries
    }

    /// Captures a snapshot of all three views so an apply can be undone.
    pub fn snapshot(&self, form: &Form) -> EditSnapshot {
        EditSnapshot {
            form: form.clone(),
            properties: self.properties.clone(),
            dictionaries: self.dictionaries.clone(),
        }
    }

    /// Validates `plan` against the current form without mutating anything.
    ///
    /// # Errors
    ///
    /// Returns a typed [`EditError`] for an unknown field, an inapplicable
    /// property, an invalid value, or a definition that would become invalid.
    pub fn validate(&self, plan: &EditPlan, form: &Form) -> Result<(), EditError> {
        let mut simulated: BTreeMap<String, FieldProperties> = BTreeMap::new();
        for edit in plan.edits() {
            let field = form
                .field(edit.field())
                .ok_or_else(|| EditError::UnknownField(edit.field().to_owned()))?;
            if !edit.key().applies_to(field.field_type()) {
                return Err(EditError::NotApplicable {
                    field: edit.field().to_owned(),
                    key: edit.key(),
                    field_type: field.field_type(),
                });
            }
            if let EditOperation::Set(value) = edit.operation() {
                validate_value(edit.field(), edit.key(), value)?;
            }
            let mut candidate = match simulated.get(edit.field()) {
                Some(existing) => existing.clone(),
                None => self
                    .properties
                    .get(edit.field())
                    .ok_or_else(|| EditError::UnknownField(edit.field().to_owned()))?
                    .clone(),
            };
            apply_to_properties(&mut candidate, edit);
            if let Err(problem) = candidate.validate_definition() {
                return Err(EditError::InvalidDefinition {
                    field: edit.field().to_owned(),
                    reason: problem.to_string(),
                });
            }
            simulated.insert(edit.field().to_owned(), candidate);
        }
        Ok(())
    }

    /// Applies `plan` atomically and re-runs validation.
    ///
    /// All edits are validated before any view is mutated, so an error leaves
    /// the editor and form unchanged. On success the property set, field
    /// dictionaries and form fields are updated consistently and a fresh
    /// [`ValidationSummary`] is returned in [`EditReport::validation`].
    ///
    /// # Errors
    ///
    /// Returns the same typed errors as [`Self::validate`].
    pub fn apply(&mut self, plan: &EditPlan, form: &mut Form) -> Result<EditReport, EditError> {
        self.validate(plan, form)?;
        let mut applied = 0usize;
        let mut cleared = 0usize;
        for edit in plan.edits() {
            if matches!(edit.operation(), EditOperation::Clear) {
                cleared += 1;
            } else {
                applied += 1;
            }
            if let Some(properties) = self.properties.get_mut(edit.field()) {
                apply_to_properties(properties, edit);
            }
            if let Some(dictionary) = self.dictionaries.dicts.get_mut(edit.field()) {
                apply_to_dictionary(dictionary, edit);
            }
            if let Some(field) = form.field_mut(edit.field()) {
                apply_to_field(field, edit);
            }
        }
        Ok(EditReport {
            applied,
            cleared,
            validation: self.properties.validate(form),
        })
    }

    /// Re-runs property validation against `form` without mutating anything.
    pub fn validate_form(&self, form: &Form) -> ValidationSummary {
        self.properties.validate(form)
    }

    /// Restores the editor and form from a previously captured snapshot.
    pub fn restore(&mut self, snapshot: &EditSnapshot, form: &mut Form) {
        *form = snapshot.form.clone();
        self.properties = snapshot.properties.clone();
        self.dictionaries = snapshot.dictionaries.clone();
    }
}

fn validate_value(field: &str, key: PropertyKey, value: &PropertyValue) -> Result<(), EditError> {
    let reason = match (key, value) {
        (PropertyKey::Tooltip, PropertyValue::Tooltip(text)) if text.is_empty() => {
            Some("tooltip must not be empty")
        }
        (PropertyKey::MaxLength, PropertyValue::MaxLength(length)) if *length == 0 => {
            Some("maximum length must be greater than zero")
        }
        (PropertyKey::Choices, PropertyValue::Choices(choices)) if choices.is_empty() => {
            Some("choices must not be empty")
        }
        (PropertyKey::Choices, PropertyValue::Choices(choices)) => {
            let mut seen = BTreeSet::new();
            if choices.iter().any(|choice| !seen.insert(choice.as_str())) {
                Some("choices must be unique")
            } else {
                None
            }
        }
        _ => None,
    };
    match reason {
        Some(reason) => Err(EditError::InvalidValue {
            field: field.to_owned(),
            key,
            reason: reason.to_owned(),
        }),
        None => Ok(()),
    }
}

fn apply_to_properties(properties: &mut FieldProperties, edit: &PropertyEdit) {
    match (edit.key(), edit.operation()) {
        (PropertyKey::Tooltip, EditOperation::Set(PropertyValue::Tooltip(value))) => {
            properties.set_tooltip(Some(value.clone()));
        }
        (PropertyKey::Tooltip, EditOperation::Clear) => properties.set_tooltip(None),
        (PropertyKey::Required, EditOperation::Set(PropertyValue::Flag(value))) => {
            properties.set_required(*value);
        }
        (PropertyKey::Required, EditOperation::Clear) => properties.set_required(false),
        (PropertyKey::ReadOnly, EditOperation::Set(PropertyValue::Flag(value))) => {
            properties.set_read_only(*value);
        }
        (PropertyKey::ReadOnly, EditOperation::Clear) => properties.set_read_only(false),
        (PropertyKey::MaxLength, EditOperation::Set(PropertyValue::MaxLength(value))) => {
            properties.set_max_len(Some(*value));
        }
        (PropertyKey::MaxLength, EditOperation::Clear) => properties.set_max_len(None),
        (PropertyKey::Format, EditOperation::Set(PropertyValue::Format(value))) => {
            properties.set_format(Some(value.clone()));
        }
        (PropertyKey::Format, EditOperation::Clear) => properties.set_format(None),
        (PropertyKey::Choices, EditOperation::Set(PropertyValue::Choices(value))) => {
            properties.set_choices(value.clone());
        }
        (PropertyKey::Choices, EditOperation::Clear) => properties.set_choices(Vec::new()),
        // A validated plan guarantees key/value agreement; unreachable arms are
        // intentionally no-ops rather than panics on hostile input.
        _ => {}
    }
}

fn set_flag(entries: &mut BTreeMap<String, PdfValue>, flag: FieldFlags, enabled: bool) {
    let current = entries.get("Ff").and_then(PdfValue::as_i64).unwrap_or(0);
    let bits = flag.bits() as i64;
    let updated = if enabled {
        current | bits
    } else {
        current & !bits
    };
    entries.insert("Ff".to_owned(), PdfValue::Number(updated as f64));
}

fn apply_to_dictionary(dictionary: &mut PdfValue, edit: &PropertyEdit) {
    let PdfValue::Dictionary(entries) = dictionary else {
        return;
    };
    match (edit.key(), edit.operation()) {
        (PropertyKey::Tooltip, EditOperation::Set(PropertyValue::Tooltip(value))) => {
            entries.insert("TU".to_owned(), PdfValue::Text(value.clone()));
        }
        (PropertyKey::Tooltip, EditOperation::Clear) => {
            entries.remove("TU");
        }
        (PropertyKey::Required, EditOperation::Set(PropertyValue::Flag(value))) => {
            set_flag(entries, FieldFlags::REQUIRED, *value);
        }
        (PropertyKey::Required, EditOperation::Clear) => {
            set_flag(entries, FieldFlags::REQUIRED, false);
        }
        (PropertyKey::ReadOnly, EditOperation::Set(PropertyValue::Flag(value))) => {
            set_flag(entries, FieldFlags::READ_ONLY, *value);
        }
        (PropertyKey::ReadOnly, EditOperation::Clear) => {
            set_flag(entries, FieldFlags::READ_ONLY, false);
        }
        (PropertyKey::MaxLength, EditOperation::Set(PropertyValue::MaxLength(value))) => {
            entries.insert("MaxLen".to_owned(), PdfValue::Number(*value as f64));
        }
        (PropertyKey::MaxLength, EditOperation::Clear) => {
            entries.remove("MaxLen");
        }
        (PropertyKey::Choices, EditOperation::Set(PropertyValue::Choices(value))) => {
            entries.insert(
                "Opt".to_owned(),
                PdfValue::Array(value.iter().cloned().map(PdfValue::Text).collect()),
            );
        }
        (PropertyKey::Choices, EditOperation::Clear) => {
            entries.remove("Opt");
        }
        // `Format` has no dedicated PDF field-dictionary key.
        _ => {}
    }
}

fn apply_to_field(field: &mut Field, edit: &PropertyEdit) {
    match (edit.key(), edit.operation()) {
        (PropertyKey::Required, EditOperation::Set(PropertyValue::Flag(value))) => {
            let bits = field.flags().bits();
            let updated = if *value {
                bits | FieldFlags::REQUIRED.bits()
            } else {
                bits & !FieldFlags::REQUIRED.bits()
            };
            field.set_flags(FieldFlags::from_bits(updated));
        }
        (PropertyKey::Required, EditOperation::Clear) => {
            field.set_flags(FieldFlags::from_bits(
                field.flags().bits() & !FieldFlags::REQUIRED.bits(),
            ));
        }
        (PropertyKey::ReadOnly, EditOperation::Set(PropertyValue::Flag(value))) => {
            let bits = field.flags().bits();
            let updated = if *value {
                bits | FieldFlags::READ_ONLY.bits()
            } else {
                bits & !FieldFlags::READ_ONLY.bits()
            };
            field.set_flags(FieldFlags::from_bits(updated));
        }
        (PropertyKey::ReadOnly, EditOperation::Clear) => {
            field.set_flags(FieldFlags::from_bits(
                field.flags().bits() & !FieldFlags::READ_ONLY.bits(),
            ));
        }
        (PropertyKey::MaxLength, EditOperation::Set(PropertyValue::MaxLength(value))) => {
            field.set_constraint(TextConstraint::new().with_max_len(*value));
        }
        (PropertyKey::MaxLength, EditOperation::Clear) => {
            field.set_constraint(TextConstraint::new());
        }
        (PropertyKey::Choices, EditOperation::Set(PropertyValue::Choices(value))) => {
            field.set_choices(value.iter().cloned());
        }
        (PropertyKey::Choices, EditOperation::Clear) => {
            field.set_choices(std::iter::empty::<String>());
        }
        // Tooltip and format are not stored on `Field`.
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::WidgetRect;
    use crate::field::FieldValue;
    use crate::validation::ValidationError;

    fn specs() -> Vec<FieldSpec> {
        let rect = WidgetRect::new(0.0, 0.0, 60.0, 20.0);
        vec![
            FieldSpec::text("name", rect, 0).with_tooltip("Name"),
            FieldSpec::choice("size", rect, 0, ["S", "M"], false),
            FieldSpec::checkbox("agree", rect, 0),
        ]
    }

    fn editor_and_form() -> (PropertyEditor, Form) {
        let specs = specs();
        let editor = PropertyEditor::from_specs(&specs).unwrap();
        let form = Form::from_fields(specs.iter().map(|spec| spec.to_field().unwrap())).unwrap();
        (editor, form)
    }

    #[test]
    fn descriptors_vary_by_field_type() {
        let text = applicable_properties(FieldType::Text);
        let text_keys: Vec<PropertyKey> = text.iter().map(|descriptor| descriptor.key()).collect();
        assert!(text_keys.contains(&PropertyKey::MaxLength));
        assert!(text_keys.contains(&PropertyKey::Format));
        assert!(!text_keys.contains(&PropertyKey::Choices));

        for choice_type in [FieldType::Choice, FieldType::Radio] {
            let descriptors = applicable_properties(choice_type);
            assert!(descriptors.iter().any(|d| d.key() == PropertyKey::Choices));
            assert!(
                !descriptors
                    .iter()
                    .any(|d| d.key() == PropertyKey::MaxLength)
            );
        }

        let checkbox = applicable_properties(FieldType::CheckBox);
        assert!(!checkbox.iter().any(|d| d.key() == PropertyKey::Choices));
        assert!(!checkbox.iter().any(|d| d.key() == PropertyKey::MaxLength));
        assert_eq!(text, applicable_properties(FieldType::Text));
        assert_eq!(text[0].key(), PropertyKey::Tooltip);
        assert_eq!(text[0].label(), "Tooltip");
    }

    #[test]
    fn edit_plan_is_deterministic_regardless_of_input_order() {
        let first = PropertyEdit::set("name", PropertyKey::MaxLength, PropertyValue::MaxLength(10))
            .unwrap();
        let second = PropertyEdit::clear("size", PropertyKey::Choices);
        let third = PropertyEdit::set(
            "name",
            PropertyKey::Tooltip,
            PropertyValue::Tooltip("Label".to_owned()),
        )
        .unwrap();
        let plan_a = EditPlan::new(vec![first.clone(), second.clone(), third.clone()]).unwrap();
        let plan_b = EditPlan::new(vec![third, second, first]).unwrap();
        assert_eq!(plan_a, plan_b);
        assert_eq!(plan_a.len(), 3);
        assert!(!plan_a.is_empty());
        assert_eq!(plan_a.edits()[0].field(), "name");
        assert_eq!(plan_a.edits()[0].key(), PropertyKey::Tooltip);
        assert_eq!(plan_a.edits()[1].key(), PropertyKey::MaxLength);
    }

    #[test]
    fn duplicate_edit_is_rejected() {
        let first = PropertyEdit::set("name", PropertyKey::MaxLength, PropertyValue::MaxLength(10))
            .unwrap();
        let second = PropertyEdit::clear("name", PropertyKey::MaxLength);
        assert!(matches!(
            EditPlan::new(vec![first, second]),
            Err(EditError::DuplicateEdit { .. })
        ));
    }

    #[test]
    fn applicable_property_and_value_kind_are_enforced() {
        assert!(matches!(
            PropertyEdit::set(
                "name",
                PropertyKey::MaxLength,
                PropertyValue::Tooltip("wrong".to_owned())
            ),
            Err(EditError::ValueKeyMismatch { .. })
        ));

        let (mut editor, mut form) = editor_and_form();
        let plan = EditPlan::new(vec![
            PropertyEdit::set(
                "name",
                PropertyKey::Choices,
                PropertyValue::Choices(vec!["a".to_owned()]),
            )
            .unwrap(),
        ])
        .unwrap();
        assert!(matches!(
            editor.apply(&plan, &mut form),
            Err(EditError::NotApplicable { .. })
        ));
    }

    #[test]
    fn invalid_values_are_rejected_before_any_mutation() {
        let (mut editor, mut form) = editor_and_form();
        for (field, key, value) in [
            (
                "name",
                PropertyKey::Tooltip,
                PropertyValue::Tooltip(String::new()),
            ),
            ("name", PropertyKey::MaxLength, PropertyValue::MaxLength(0)),
            (
                "size",
                PropertyKey::Choices,
                PropertyValue::Choices(Vec::new()),
            ),
            (
                "size",
                PropertyKey::Choices,
                PropertyValue::Choices(vec!["x".to_owned(), "x".to_owned()]),
            ),
        ] {
            let plan = EditPlan::new(vec![PropertyEdit::set(field, key, value).unwrap()]).unwrap();
            assert!(matches!(
                editor.apply(&plan, &mut form),
                Err(EditError::InvalidValue { .. })
            ));
        }
    }

    #[test]
    fn apply_updates_field_dictionary_and_properties() {
        let (mut editor, mut form) = editor_and_form();
        form.set_value("name", FieldValue::Text("Ada".to_owned()))
            .unwrap();
        form.set_value("agree", FieldValue::Bool(true)).unwrap();
        let plan = EditPlan::new(vec![
            PropertyEdit::set(
                "name",
                PropertyKey::Tooltip,
                PropertyValue::Tooltip("Full name".to_owned()),
            )
            .unwrap(),
            PropertyEdit::set("name", PropertyKey::MaxLength, PropertyValue::MaxLength(12))
                .unwrap(),
            PropertyEdit::set(
                "size",
                PropertyKey::Choices,
                PropertyValue::Choices(vec!["S".to_owned(), "M".to_owned(), "L".to_owned()]),
            )
            .unwrap(),
            PropertyEdit::set("agree", PropertyKey::Required, PropertyValue::Flag(true)).unwrap(),
        ])
        .unwrap();
        let report = editor.apply(&plan, &mut form).unwrap();
        assert_eq!(report.applied, 4);
        assert_eq!(report.cleared, 0);
        assert!(report.is_valid(), "{:?}", report.validation.errors());

        let name_dict = editor
            .dictionaries()
            .get("name")
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(name_dict.get("TU").unwrap().as_text(), Some("Full name"));
        assert_eq!(name_dict.get("MaxLen").unwrap().as_i64(), Some(12));
        let size_dict = editor
            .dictionaries()
            .get("size")
            .unwrap()
            .as_dict()
            .unwrap();
        assert_eq!(size_dict.get("Opt").unwrap().as_array().unwrap().len(), 3);
        let agree_dict = editor
            .dictionaries()
            .get("agree")
            .unwrap()
            .as_dict()
            .unwrap();
        let flags = FieldFlags::from_bits(agree_dict.get("Ff").unwrap().as_i64().unwrap() as u32);
        assert!(flags.is_required());

        assert_eq!(editor.properties().get("name").unwrap().max_len(), Some(12));
        assert_eq!(
            editor.properties().get("name").unwrap().tooltip(),
            Some("Full name")
        );
        assert!(form.field("agree").unwrap().is_required());
        assert_eq!(report.to_lines(), report.to_lines());
    }

    #[test]
    fn invalid_definition_is_rejected_atomically() {
        let (mut editor, mut form) = editor_and_form();
        let read_only = EditPlan::new(vec![
            PropertyEdit::set("name", PropertyKey::ReadOnly, PropertyValue::Flag(true)).unwrap(),
        ])
        .unwrap();
        editor.apply(&read_only, &mut form).unwrap();
        let before = editor.dictionaries().clone();

        let conflicting = EditPlan::new(vec![
            PropertyEdit::set("name", PropertyKey::Required, PropertyValue::Flag(true)).unwrap(),
        ])
        .unwrap();
        assert!(matches!(
            editor.apply(&conflicting, &mut form),
            Err(EditError::InvalidDefinition { .. })
        ));
        assert_eq!(*editor.dictionaries(), before);
        assert!(!form.field("name").unwrap().is_required());
    }

    #[test]
    fn undo_snapshot_restores_form_properties_and_dictionaries() {
        let (mut editor, mut form) = editor_and_form();
        form.set_value("name", FieldValue::Text("Ada".to_owned()))
            .unwrap();
        let snapshot = editor.snapshot(&form);
        let plan = EditPlan::new(vec![
            PropertyEdit::set(
                "name",
                PropertyKey::Tooltip,
                PropertyValue::Tooltip("Changed".to_owned()),
            )
            .unwrap(),
            PropertyEdit::set("name", PropertyKey::MaxLength, PropertyValue::MaxLength(5)).unwrap(),
        ])
        .unwrap();
        editor.apply(&plan, &mut form).unwrap();
        assert_eq!(editor.properties().get("name").unwrap().max_len(), Some(5));

        editor.restore(&snapshot, &mut form);
        assert_eq!(editor.properties().get("name").unwrap().max_len(), None);
        assert_eq!(
            editor.properties().get("name").unwrap().tooltip(),
            Some("Name")
        );
        assert_eq!(
            form.value("name").unwrap(),
            &FieldValue::Text("Ada".to_owned())
        );
        assert_eq!(*editor.dictionaries(), snapshot.dictionaries().clone());
    }

    #[test]
    fn apply_reruns_validation_after_property_change() {
        let (mut editor, mut form) = editor_and_form();
        let plan = EditPlan::new(vec![
            PropertyEdit::set("name", PropertyKey::Required, PropertyValue::Flag(true)).unwrap(),
        ])
        .unwrap();
        let report = editor.apply(&plan, &mut form).unwrap();
        assert!(!report.is_valid());
        assert!(report.validation.errors().iter().any(|error| matches!(
            error,
            ValidationError::MissingRequired { field } if field == "name"
        )));

        form.set_value("name", FieldValue::Text("Ada".to_owned()))
            .unwrap();
        assert!(editor.validate_form(&form).is_valid());
    }
}
