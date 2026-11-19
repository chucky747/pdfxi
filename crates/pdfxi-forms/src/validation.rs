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
    /// A text value exceeded its configured maximum length.
    MaxLengthExceeded {
        /// Fully qualified name of the offending field.
        field: String,
        /// The maximum number of characters allowed.
        max: usize,
        /// The number of characters found.
        actual: usize,
    },
    /// A value did not satisfy a configured [`crate::rules::Format`].
    InvalidFormat {
        /// Fully qualified name of the offending field.
        field: String,
        /// Human-readable name of the expected format.
        format: String,
        /// Human-readable description of the failure.
        detail: String,
    },
    /// A [`crate::rules::ValidationHook`] rejected the field's value.
    CustomRejected {
        /// Fully qualified name of the offending field.
        field: String,
        /// The reason supplied by the hook.
        reason: String,
    },
    /// A rule was registered for a field that does not exist in the form.
    UnknownField {
        /// Fully qualified name that was not found.
        field: String,
    },
}

impl ValidationError {
    /// Returns the fully qualified name of the field that failed validation.
    pub fn field(&self) -> &str {
        match self {
            Self::MissingRequired { field }
            | Self::InvalidChoice { field, .. }
            | Self::InvalidText { field, .. }
            | Self::InvalidValueType { field, .. }
            | Self::MaxLengthExceeded { field, .. }
            | Self::InvalidFormat { field, .. }
            | Self::CustomRejected { field, .. }
            | Self::UnknownField { field } => field,
        }
    }

    /// Returns a stable rank for this variant, used to order errors that share
    /// a field name. The ordering is independent of platform collation.
    pub(crate) fn sort_rank(&self) -> u8 {
        match self {
            Self::UnknownField { .. } => 0,
            Self::MissingRequired { .. } => 1,
            Self::MaxLengthExceeded { .. } => 2,
            Self::InvalidValueType { .. } => 3,
            Self::InvalidChoice { .. } => 4,
            Self::InvalidFormat { .. } => 5,
            Self::InvalidText { .. } => 6,
            Self::CustomRejected { .. } => 7,
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
            Self::MaxLengthExceeded { field, max, actual } => write!(
                f,
                "field '{field}' has {actual} characters, exceeding the maximum {max}"
            ),
            Self::InvalidFormat {
                field,
                format,
                detail,
            } => write!(f, "field '{field}' failed {format} validation: {detail}"),
            Self::CustomRejected { field, reason } => {
                write!(f, "field '{field}' was rejected: {reason}")
            }
            Self::UnknownField { field } => {
                write!(f, "rule targets unknown field '{field}'")
            }
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

/// A deterministic summary of a rule-based validation pass.
///
/// Errors are sorted by fully qualified field name and then by a stable
/// variant rank, so two runs over equivalent forms produce byte-identical
/// reports regardless of iteration or registration order.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ValidationSummary {
    total_fields: usize,
    errors: Vec<ValidationError>,
}

impl ValidationSummary {
    /// Builds a summary, sorting the errors into deterministic order.
    pub fn new(total_fields: usize, mut errors: Vec<ValidationError>) -> Self {
        errors.sort_by(|a, b| {
            a.field()
                .cmp(b.field())
                .then_with(|| a.sort_rank().cmp(&b.sort_rank()))
        });
        Self {
            total_fields,
            errors,
        }
    }

    /// Returns `true` when the form had no validation errors.
    pub fn is_valid(&self) -> bool {
        self.errors.is_empty()
    }

    /// Returns the number of validation errors.
    pub fn error_count(&self) -> usize {
        self.errors.len()
    }

    /// Returns the number of fields that were checked.
    pub fn total_fields(&self) -> usize {
        self.total_fields
    }

    /// Returns the errors in deterministic order.
    pub fn errors(&self) -> &[ValidationError] {
        &self.errors
    }

    /// Returns the distinct names of the invalid fields, in ascending order.
    pub fn invalid_field_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.errors.iter().map(ValidationError::field).collect();
        names.sort_unstable();
        names.dedup();
        names
    }

    /// Returns a stable one-line description of the result.
    pub fn summary_line(&self) -> String {
        format!(
            "{} error(s) across {} of {} field(s)",
            self.errors.len(),
            self.invalid_field_names().len(),
            self.total_fields
        )
    }

    /// Converts this summary into the simpler [`Validation`] representation.
    pub fn into_validation(self) -> Validation {
        Validation::new(self.errors)
    }
}

impl fmt::Display for ValidationSummary {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.summary_line())
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
