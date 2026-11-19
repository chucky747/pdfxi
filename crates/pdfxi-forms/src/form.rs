//! The [`Form`] container: field lookup, mutation, reset, and validation.
//!
//! A form keeps its fields in a [`Vec`], so enumeration order is the stable
//! insertion order. Lookup by fully qualified name is exact, and duplicate
//! names are rejected when fields are added. Because the field vector is the
//! single source of truth, there is no secondary index that could drift out of
//! sync with the fields.

use crate::field::{Field, FieldValue};
use crate::validation::Validation;
use std::fmt;

/// An error produced by a mutating [`Form`] operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormError {
    /// A field with the same fully qualified name already exists in the form.
    DuplicateFieldName(String),
    /// No field with the given fully qualified name exists.
    FieldNotFound(String),
    /// The target field is read-only and cannot be changed.
    ReadOnlyField(String),
}

impl fmt::Display for FormError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::DuplicateFieldName(name) => {
                write!(f, "duplicate field name '{name}'")
            }
            Self::FieldNotFound(name) => write!(f, "no field named '{name}'"),
            Self::ReadOnlyField(name) => write!(f, "field '{name}' is read-only"),
        }
    }
}

impl std::error::Error for FormError {}

/// An AcroForm: an ordered collection of uniquely named fields.
///
/// Fields are stored in insertion order, which is the order returned by
/// [`Form::fields`] and used when collecting [`Form::validate`] errors. Values
/// may be changed with [`Form::set_value`] (which refuses read-only fields)
/// and restored with [`Form::reset`] / [`Form::reset_field`].
#[derive(Debug, Clone, Default)]
pub struct Form {
    fields: Vec<Field>,
}

impl Form {
    /// Creates an empty form.
    pub fn new() -> Self {
        Self { fields: Vec::new() }
    }

    /// Builds a form from an iterator of fields, rejecting duplicate names.
    ///
    /// Returns the first duplicate encountered as [`FormError::DuplicateFieldName`].
    pub fn from_fields<I>(fields: I) -> Result<Self, FormError>
    where
        I: IntoIterator<Item = Field>,
    {
        let mut form = Self::new();
        for field in fields {
            form.add_field(field)?;
        }
        Ok(form)
    }

    /// Adds a field to the end of the form.
    ///
    /// Returns [`FormError::DuplicateFieldName`] if a field with the same fully
    /// qualified name is already present; in that case the form is unchanged.
    pub fn add_field(&mut self, field: Field) -> Result<(), FormError> {
        if self
            .fields
            .iter()
            .any(|existing| existing.name() == field.name())
        {
            return Err(FormError::DuplicateFieldName(field.name().to_owned()));
        }
        self.fields.push(field);
        Ok(())
    }

    /// Returns the field with the given fully qualified name, if present.
    pub fn field(&self, name: &str) -> Option<&Field> {
        self.fields.iter().find(|field| field.name() == name)
    }

    /// Returns a mutable reference to the named field, if present.
    pub fn field_mut(&mut self, name: &str) -> Option<&mut Field> {
        self.fields.iter_mut().find(|field| field.name() == name)
    }

    /// Returns `true` if a field with the given name exists.
    pub fn contains(&self, name: &str) -> bool {
        self.field(name).is_some()
    }

    /// Sets the current value of a named field.
    ///
    /// Returns [`FormError::FieldNotFound`] if the name is unknown, or
    /// [`FormError::ReadOnlyField`] if the field is read-only. A successful
    /// call never changes the default value, so [`Form::reset`] can undo it.
    pub fn set_value(&mut self, name: &str, value: FieldValue) -> Result<(), FormError> {
        let field = self
            .field_mut(name)
            .ok_or_else(|| FormError::FieldNotFound(name.to_owned()))?;
        if field.is_read_only() {
            return Err(FormError::ReadOnlyField(name.to_owned()));
        }
        field.set_value(value);
        Ok(())
    }

    /// Restores a single named field to its default value.
    ///
    /// Read-only fields are left unchanged and reported as
    /// [`FormError::ReadOnlyField`].
    pub fn reset_field(&mut self, name: &str) -> Result<(), FormError> {
        let field = self
            .field_mut(name)
            .ok_or_else(|| FormError::FieldNotFound(name.to_owned()))?;
        if field.is_read_only() {
            return Err(FormError::ReadOnlyField(name.to_owned()));
        }
        field.reset();
        Ok(())
    }

    /// Restores every writable field to its default value.
    ///
    /// Read-only fields are skipped so their displayed value is not changed by
    /// a bulk reset.
    pub fn reset(&mut self) {
        for field in &mut self.fields {
            if !field.is_read_only() {
                field.reset();
            }
        }
    }

    /// Returns all fields in stable insertion order.
    pub fn fields(&self) -> impl ExactSizeIterator<Item = &Field> + '_ {
        self.fields.iter()
    }

    /// Returns all fully qualified field names in stable insertion order.
    pub fn field_names(&self) -> impl ExactSizeIterator<Item = &str> + '_ {
        self.fields.iter().map(|field| field.name())
    }

    /// Returns the number of fields in the form.
    pub fn len(&self) -> usize {
        self.fields.len()
    }

    /// Returns `true` when the form has no fields.
    pub fn is_empty(&self) -> bool {
        self.fields.is_empty()
    }

    /// Validates every field and collects the errors in stable field order.
    pub fn validate(&self) -> Validation {
        let errors = self
            .fields
            .iter()
            .filter_map(|field| field.validate().err())
            .collect();
        Validation::new(errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::field::{FieldFlags, FieldType, TextConstraint};
    use crate::validation::ValidationError;

    fn text_field(name: &str) -> Field {
        Field::new(name, FieldType::Text)
    }

    #[test]
    fn field_lookup_by_fully_qualified_name() {
        let mut form = Form::new();
        form.add_field(text_field("order.address.city")).unwrap();
        form.add_field(text_field("order.address.zip")).unwrap();

        assert!(form.contains("order.address.city"));
        assert_eq!(
            form.field("order.address.zip").map(|f| f.name()),
            Some("order.address.zip")
        );
        assert!(form.field("order.city").is_none());
        assert!(form.field("missing").is_none());
    }

    #[test]
    fn duplicate_field_names_are_rejected() {
        let mut form = Form::new();
        form.add_field(text_field("dup")).unwrap();
        let result = form.add_field(text_field("dup"));

        assert_eq!(result, Err(FormError::DuplicateFieldName("dup".to_owned())));
        assert_eq!(form.len(), 1, "a rejected field must not be inserted");
    }

    #[test]
    fn from_fields_detects_duplicates() {
        let result = Form::from_fields([text_field("x"), text_field("x")]);
        assert_eq!(
            result.err(),
            Some(FormError::DuplicateFieldName("x".to_owned()))
        );
    }

    #[test]
    fn required_field_missing_fails_validation() {
        let form = Form::from_fields([
            text_field("name").with_flags(FieldFlags::REQUIRED),
            text_field("nickname"),
        ])
        .unwrap();

        let validation = form.validate();
        assert!(!validation.is_valid());
        assert_eq!(
            validation.errors(),
            &[ValidationError::MissingRequired {
                field: "name".to_owned()
            }]
        );
    }

    #[test]
    fn fully_valid_form_passes_validation() {
        let form = Form::from_fields([
            text_field("name")
                .with_flags(FieldFlags::REQUIRED)
                .with_value(FieldValue::Text("Ada".into())),
            Field::new("subscribe", FieldType::CheckBox).with_default(FieldValue::Bool(false)),
        ])
        .unwrap();

        assert!(form.validate().is_valid(), "{:?}", form.validate().errors());
    }

    #[test]
    fn choice_field_rejects_value_outside_choices() {
        let form = Form::from_fields([
            Field::new("color", FieldType::Choice)
                .with_choices(["red", "green"])
                .with_value(FieldValue::Choice(vec!["red".into()])),
            Field::new("size", FieldType::Radio)
                .with_choices(["S", "M"])
                .with_value(FieldValue::Choice(vec!["XL".into()])),
        ])
        .unwrap();

        let validation = form.validate();
        assert_eq!(
            validation.errors(),
            &[ValidationError::InvalidChoice {
                field: "size".to_owned(),
                value: "XL".to_owned(),
            }]
        );
    }

    #[test]
    fn text_field_enforces_constraints() {
        let form = Form::from_fields([text_field("code")
            .with_constraint(TextConstraint::new().with_max_len(3))
            .with_value(FieldValue::Text("too-long".into()))])
        .unwrap();

        let validation = form.validate();
        assert_eq!(validation.error_count(), 1);
        assert!(matches!(
            &validation.errors()[0],
            ValidationError::InvalidText { field, .. } if field == "code"
        ));
    }

    #[test]
    fn reset_restores_default_values() {
        let mut form = Form::from_fields([
            text_field("city").with_default(FieldValue::Text("Paris".into())),
            text_field("note"),
        ])
        .unwrap();

        form.set_value("city", FieldValue::Text("Rome".into()))
            .unwrap();
        form.set_value("note", FieldValue::Text("temp".into()))
            .unwrap();
        assert_eq!(
            form.field("city").unwrap().value(),
            &FieldValue::Text("Rome".into())
        );

        form.reset();
        assert_eq!(
            form.field("city").unwrap().value(),
            &FieldValue::Text("Paris".into())
        );
        assert_eq!(form.field("note").unwrap().value(), &FieldValue::Empty);

        form.set_value("city", FieldValue::Text("Rome".into()))
            .unwrap();
        form.reset_field("city").unwrap();
        assert_eq!(
            form.field("city").unwrap().value(),
            &FieldValue::Text("Paris".into())
        );
    }

    #[test]
    fn fields_are_enumerated_in_stable_insertion_order() {
        let form = Form::from_fields([
            text_field("zebra"),
            text_field("alpha"),
            text_field("middle"),
        ])
        .unwrap();

        let names: Vec<&str> = form.fields().map(|f| f.name()).collect();
        assert_eq!(names, ["zebra", "alpha", "middle"]);

        let field_names: Vec<&str> = form.field_names().collect();
        assert_eq!(field_names, names);
        assert_eq!(form.len(), 3);
        assert!(!form.is_empty());
    }

    #[test]
    fn set_value_refuses_read_only_and_unknown_fields() {
        let mut form = Form::new();
        form.add_field(text_field("locked").with_flags(FieldFlags::READ_ONLY))
            .unwrap();

        assert_eq!(
            form.set_value("locked", FieldValue::Text("x".into())),
            Err(FormError::ReadOnlyField("locked".to_owned()))
        );
        assert_eq!(
            form.set_value("absent", FieldValue::Text("x".into())),
            Err(FormError::FieldNotFound("absent".to_owned()))
        );
        assert_eq!(form.field("locked").unwrap().value(), &FieldValue::Empty);
    }

    #[test]
    fn read_only_fields_are_skipped_by_bulk_reset() {
        let mut form = Form::new();
        form.add_field(
            text_field("frozen")
                .with_flags(FieldFlags::READ_ONLY)
                .with_default(FieldValue::Text("default".into()))
                .with_value(FieldValue::Text("override".into())),
        )
        .unwrap();

        form.reset();
        assert_eq!(
            form.field("frozen").unwrap().value(),
            &FieldValue::Text("override".into())
        );
    }
}
