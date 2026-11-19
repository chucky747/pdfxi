//! Validation results and errors produced by [`crate::Form::validate`].
//!
//! Validation is a pure, side-effect-free pass over a form. It reports three
//! classes of problem, each with a dedicated error variant:
//!
//! - [`ValidationError::MissingRequired`] — a required field has no value.
//! - [`ValidationError::InvalidChoice`] — a choice or radio field selects an
//!   option that is not in its export-value list.
//! - [`ValidationError::InvalidText`] — a text field violates its configured
//!   [`TextConstraint`](crate::TextConstraint).
//! - [`ValidationError::InvalidValueType`] — a value's representation does not
//!   match its field type (for example text in a checkbox).
//!
//! No I/O, JavaScript, or external dependency is involved.

use std::fmt;

/// A single validation problem found while checking a form.
///
/// Every variant carries the fully qualified field name so callers can present
/// an actionable, field-scoped message without guessing which widget failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ValidationError {
    /// A field marked required has an empty value.
    MissingRequired {
        /// Fully qualified name of the offending field.
        field: String,
    },
    /// A choice or radio field selects a value outside its allowed options.
    InvalidChoice {
        /// Fully qualified name of the offending field.
        field: String,
        /// The value that is not part of the field's option list.
        value: String,
    },
    /// A text field's value violates a configured [`crate::TextConstraint`].
    InvalidText {
        /// Fully qualified name of the offending field.
        field: String,
        /// Human-readable description of the violated rule.
        reason: String,
    },
    /// The value's representation does not match the field's type.
    InvalidValueType {
        /// Fully qualified name of the offending field.
        field: String,
        /// Slash-separated list of accepted value categories.
        expected: String,
        /// The category actually found.
        found: String,
    },
}

impl ValidationError {
    /// Returns the fully qualified name of the field that failed validation.
    pub fn field(&self) -> &str {
        match self {
            Self::MissingRequired { field }
            | Self::InvalidChoice { field, .. }
            | Self::InvalidText { field, .. }
            | Self::InvalidValueType { field, .. } => field,
        }
    }
}

impl fmt::Display for ValidationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingRequired { field } => {
                write!(f, "required field '{field}' has no value")
            }
            Self::InvalidChoice { field, value } => write!(
                f,
                "field '{field}' selects '{value}', which is not an allowed choice"
            ),
            Self::InvalidText { field, reason } => {
                write!(f, "field '{field}' has invalid text: {reason}")
            }
            Self::InvalidValueType {
                field,
                expected,
                found,
            } => write!(
                f,
                "field '{field}' holds a {found} value; expected one of {expected}"
            ),
        }
    }
}

impl std::error::Error for ValidationError {}

/// The outcome of validating a form.
///
/// A `Validation` is valid when it contains no errors. Errors are kept in the
/// stable field order in which they were discovered so a UI can highlight the
/// first failing widget deterministically.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Validation {
    errors: Vec<ValidationError>,
}

impl Validation {
    /// Builds a validation result from an ordered list of errors.
    pub fn new(errors: Vec<ValidationError>) -> Self {
        Self { errors }
    }

    /// Returns `true` when no validation errors were recorded.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the validation errors in stable field order.
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    /// Consumes the result and returns the owned error vector.
    pub fn into_errors(self) -> Vec<ValidationError> {
        self.errors
    }

    /// Returns the number of validation errors.
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }
}

impl fmt::Display for Validation {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "validation with {} error(s)", self.errors.len())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn empty_validation_is_valid() {
        let validation = Validation::default();
        assert!(validation.is_valid());
        assert_eq!(validation.error_count(), 0);
        assert!(validation.errors().is_empty());
    }

    #[test]
    fn validation_error_reports_its_field() {
        let error = ValidationError::InvalidChoice {
            field: "form.color".to_owned(),
            value: "purple".to_owned(),
        };
        assert_eq!(error.field(), "form.color");
        assert!(error.to_string().contains("form.color"));
        assert!(error.to_string().contains("purple"));
    }
}
