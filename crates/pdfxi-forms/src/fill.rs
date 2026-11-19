//! Form-filling sessions and deterministic change reports.
//!
//! [`FormFill`] wraps a [`Form`] and records every successful write as a *net*
//! change keyed by fully qualified field name. Because the change log is a
//! [`BTreeMap`], reports are always ordered by field name regardless of the
//! order in which fields were filled. Setting a field back to its original
//! value removes it from the report, so a report describes the difference
//! between the session's start and end states.
//!
//! Filling goes through [`Form::set_value`], so read-only rejection, value type
//! checking, and required-field protection are inherited rather than
//! reimplemented here.

use crate::fdf::FdfDocument;
use crate::field::FieldValue;
use crate::form::{Form, FormError};
use crate::validation::Validation;
use std::collections::BTreeMap;
use std::collections::btree_map::Entry;
use std::fmt;

/// The net change applied to one field during a [`FormFill`] session.
///
/// `old` is the value the field held before the first successful write in the
/// session and `new` is its value at the end of the session.
#[derive(Debug, Clone, PartialEq)]
pub struct ChangedField {
    /// Fully qualified name of the changed field.
    pub name: String,
    /// Value before the session's first write to this field.
    pub old: FieldValue,
    /// Value after the session's last write to this field.
    pub new: FieldValue,
}

/// An ordered report of the fields changed by a [`FormFill`] session.
///
/// Entries are sorted by fully qualified field name. The ordering is
/// independent of insertion order and of platform string collation because it
/// uses Rust's byte-wise [`String`] ordering.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct FillReport {
    changes: Vec<ChangedField>,
}

impl FillReport {
    /// Builds a report from changes, sorting them by field name.
    pub fn new(mut changes: Vec<ChangedField>) -> Self {
        changes.sort_by(|a, b| a.name.cmp(&b.name));
        Self { changes }
    }

    /// Returns the changed fields in ascending name order.
    pub fn entries(&self) -> &[ChangedField] {
        &self.changes
    }

    /// Returns the fully qualified names of the changed fields in order.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.changes.iter().map(|change| change.name.as_str())
    }

    /// Returns the number of changed fields.
    pub fn len(&self) -> usize {
        self.changes.len()
    }

    /// Returns `true` when no fields changed.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Consumes the report and returns the owned change vector.
    pub fn into_entries(self) -> Vec<ChangedField> {
        self.changes
    }
}

impl fmt::Display for FillReport {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "fill report with {} changed field(s)",
            self.changes.len()
        )
    }
}

/// A filling session over a form.
///
/// Create one with [`FormFill::new`], write values with [`FormFill::set`] (or a
/// batch with [`FormFill::fill_many`] / [`FormFill::fill_from_fdf`]), and read
/// the resulting [`FillReport`] with [`FormFill::changes`]. Use
/// [`FormFill::into_form`] or [`FormFill::finish`] to retrieve the mutated form.
#[derive(Debug, Clone)]
pub struct FormFill {
    form: Form,
    changes: BTreeMap<String, (FieldValue, FieldValue)>,
}

impl FormFill {
    /// Starts a filling session over `form` with an empty change log.
    pub fn new(form: Form) -> Self {
        Self {
            form,
            changes: BTreeMap::new(),
        }
    }

    /// Returns a reference to the form being filled.
    pub fn form(&self) -> &Form {
        &self.form
    }

    /// Returns the current value of a named field without recording a change.
    pub fn value(&self, name: &str) -> Result<&FieldValue, FormError> {
        self.form.value(name)
    }

    /// Returns `true` when the session has not changed any field.
    pub fn is_empty(&self) -> bool {
        self.changes.is_empty()
    }

    /// Writes one field and records the net change.
    ///
    /// All validation is delegated to [`Form::set_value`], so an error leaves
    /// both the form and the change log untouched. Writing the same field more
    /// than once keeps the *original* old value; if the final value equals the
    /// original, the field is removed from the report.
    pub fn set(&mut self, name: &str, value: FieldValue) -> Result<(), FormError> {
        let old = self.form.value(name)?.clone();
        self.form.set_value(name, value.clone())?;

        match self.changes.entry(name.to_owned()) {
            Entry::Occupied(mut entry) => {
                let (original, current) = entry.get_mut();
                *current = value;
                if *original == *current {
                    entry.remove();
                }
            }
            Entry::Vacant(entry) => {
                if old != value {
                    entry.insert((old, value));
                }
            }
        }
        Ok(())
    }

    /// Writes a batch of fields in the given order.
    ///
    /// The first rejected write is returned as an error. Writes that succeeded
    /// before the failure are kept in both the form and the change log; callers
    /// that need all-or-nothing behaviour should pre-validate their inputs.
    pub fn fill_many<I>(&mut self, values: I) -> Result<(), FormError>
    where
        I: IntoIterator<Item = (String, FieldValue)>,
    {
        for (name, value) in values {
            self.set(&name, value)?;
        }
        Ok(())
    }

    /// Writes every field carried by an imported FDF document.
    ///
    /// Fields are applied in the document's order; the resulting report is
    /// still ordered by name. The first rejected write is returned as an error.
    pub fn fill_from_fdf(&mut self, document: &FdfDocument) -> Result<(), FormError> {
        for field in document.fields() {
            self.set(field.name(), field.value().clone())?;
        }
        Ok(())
    }

    /// Returns the net changes made so far, ordered by field name.
    pub fn changes(&self) -> FillReport {
        FillReport::new(
            self.changes
                .iter()
                .map(|(name, (old, new))| ChangedField {
                    name: name.clone(),
                    old: old.clone(),
                    new: new.clone(),
                })
                .collect(),
        )
    }

    /// Validates the form's current state.
    pub fn validate(&self) -> Validation {
        self.form.validate()
    }

    /// Consumes the session and returns the form together with its change report.
    pub fn finish(self) -> (Form, FillReport) {
        let report = self.changes();
        (self.form, report)
    }

    /// Consumes the session only if the resulting form validates.
    ///
    /// On failure the session is returned unchanged alongside the validation
    /// errors, so the caller can correct the values and retry.
    pub fn finish_validated(self) -> Result<(Form, FillReport), (Self, Validation)> {
        let validation = self.form.validate();
        if !validation.is_valid() {
            return Err((self, validation));
        }
        Ok(self.finish())
    }

    /// Consumes the session and returns the mutated form, discarding the report.
    pub fn into_form(self) -> Form {
        self.form
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{Field, FieldFlags, FieldType};

    fn sample_form() -> Form {
        Form::from_fields([
            Field::new("zeta", FieldType::Text),
            Field::new("alpha", FieldType::Text),
            Field::new("middle", FieldType::Text),
            Field::new("locked", FieldType::Text).with_flags(FieldFlags::READ_ONLY),
        ])
        .unwrap()
    }

    fn text(value: &str) -> FieldValue {
        FieldValue::Text(value.to_owned())
    }

    #[test]
    fn fills_each_supported_field_type() {
        let form = Form::from_fields([
            Field::new("name", FieldType::Text),
            Field::new("agree", FieldType::CheckBox),
            Field::new("color", FieldType::Choice).with_choices(["red", "green"]),
            Field::new("size", FieldType::Radio).with_choices(["S", "M"]),
            Field::new("total", FieldType::Text),
        ])
        .unwrap();
        let mut fill = FormFill::new(form);

        fill.set("name", text("Ada")).unwrap();
        fill.set("agree", FieldValue::Bool(true)).unwrap();
        fill.set("color", FieldValue::Choice(vec!["red".into()]))
            .unwrap();
        fill.set("size", FieldValue::Choice(vec!["M".into()]))
            .unwrap();
        fill.set("total", FieldValue::number(42.5)).unwrap();

        assert_eq!(fill.value("name"), Ok(&text("Ada")));
        assert_eq!(fill.value("agree"), Ok(&FieldValue::Bool(true)));
        assert_eq!(
            fill.value("color"),
            Ok(&FieldValue::Choice(vec!["red".into()]))
        );
        assert_eq!(fill.changes().len(), 5);
        assert!(fill.validate().is_valid(), "{:?}", fill.validate().errors());
    }

    #[test]
    fn type_mismatch_is_rejected_without_recording_a_change() {
        let mut fill = FormFill::new(sample_form());
        let error = fill.set("alpha", FieldValue::Bool(true)).unwrap_err();

        assert!(matches!(
            error,
            FormError::TypeMismatch { ref field, found, .. }
                if field == "alpha" && found == crate::field::ValueKind::Bool
        ));
        assert!(fill.is_empty());
        assert_eq!(fill.value("alpha"), Ok(&FieldValue::Empty));
    }

    #[test]
    fn read_only_and_required_fields_are_rejected() {
        let form = Form::from_fields([
            Field::new("locked", FieldType::Text).with_flags(FieldFlags::READ_ONLY),
            Field::new("must", FieldType::Text).with_flags(FieldFlags::REQUIRED),
        ])
        .unwrap();
        let mut fill = FormFill::new(form);

        assert_eq!(
            fill.set("locked", text("x")),
            Err(FormError::ReadOnlyField("locked".to_owned()))
        );
        assert_eq!(
            fill.set("must", FieldValue::Empty),
            Err(FormError::RequiredField("must".to_owned()))
        );
        assert!(fill.is_empty());
    }

    #[test]
    fn changed_fields_are_ordered_by_name_and_net_changes_collapse() {
        let mut fill = FormFill::new(sample_form());
        fill.set("zeta", text("1")).unwrap();
        fill.set("alpha", text("2")).unwrap();
        fill.set("middle", text("3")).unwrap();
        fill.set("alpha", text("4")).unwrap();
        fill.set("middle", FieldValue::Empty).unwrap();

        let report = fill.changes();
        assert_eq!(report.names().collect::<Vec<_>>(), ["alpha", "zeta"]);
        assert_eq!(
            report.entries()[0],
            ChangedField {
                name: "alpha".to_owned(),
                old: FieldValue::Empty,
                new: text("4"),
            }
        );
        assert_eq!(report.len(), 2);
    }

    #[test]
    fn fill_many_stops_at_first_error_but_keeps_prior_writes() {
        let mut fill = FormFill::new(sample_form());
        let result = fill.fill_many([
            ("alpha".to_owned(), text("ok")),
            ("locked".to_owned(), text("nope")),
            ("zeta".to_owned(), text("never")),
        ]);

        assert_eq!(result, Err(FormError::ReadOnlyField("locked".to_owned())));
        assert_eq!(fill.value("alpha"), Ok(&text("ok")));
        assert_eq!(fill.value("zeta"), Ok(&FieldValue::Empty));
        assert_eq!(fill.changes().names().collect::<Vec<_>>(), ["alpha"]);
    }

    #[test]
    fn finish_and_finish_validated_report_the_same_changes() {
        let form = Form::from_fields([
            Field::new("name", FieldType::Text).with_flags(FieldFlags::REQUIRED)
        ])
        .unwrap();

        let incomplete = FormFill::new(form.clone());
        assert!(incomplete.finish_validated().is_err());

        let mut complete = FormFill::new(form);
        complete.set("name", text("Ada")).unwrap();
        let (filled, report) = complete.finish_validated().unwrap();
        assert_eq!(filled.value("name"), Ok(&text("Ada")));
        assert_eq!(report.names().collect::<Vec<_>>(), ["name"]);
    }
}
