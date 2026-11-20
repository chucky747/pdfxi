//! Deterministic field property model and headless validation.
//!
//! [`FieldProperties`] is the editable, typed description of one form field:
//! its name, tooltip, required/read-only flags, maximum text length, optional
//! value format and choice options. Unlike [`crate::FieldSpec`], which models a
//! field for *emission*, this type models the properties a property editor
//! would show and validates both the definition and a supplied value.
//!
//! [`PropertySet`] stores properties in a name-keyed `BTreeMap`, so
//! [`PropertySet::list`] is deterministic regardless of insertion order.
//! Validation never reads the filesystem, runs scripts or touches a UI: it
//! produces typed [`ValidationError`](crate::validation::ValidationError)s
//! that are folded into the existing deterministic
//! [`ValidationSummary`](crate::validation::ValidationSummary). Checks cover
//! required values, maximum length, numeric/currency/date formats, choice
//! membership, the incompatible required+read-only flag pair, and name and
//! tooltip constraints.

use crate::build::FieldSpec;
use crate::field::{FieldType, FieldValue};
use crate::form::Form;
use crate::rules::Format;
use crate::validation::{ValidationError, ValidationSummary};
use std::collections::BTreeMap;
use std::fmt;

/// Maximum number of characters accepted in a property name.
pub const MAX_NAME_LEN: usize = 256;
/// Maximum number of characters accepted in a tooltip.
pub const MAX_TOOLTIP_LEN: usize = 1024;

/// A structural error in a [`FieldProperties`] definition.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PropertyError {
    /// A property with the same name is already present in a [`PropertySet`].
    #[error("duplicate property name '{0}'")]
    DuplicateName(String),
    /// The name was empty.
    #[error("property name is empty")]
    EmptyName,
    /// The name violated a length or character constraint.
    #[error("property name constraint: {0}")]
    InvalidName(String),
    /// The tooltip violated a length or character constraint.
    #[error("property tooltip constraint: {0}")]
    InvalidTooltip(String),
    /// The required and read-only flags were both set.
    #[error("a field cannot be both required and read-only")]
    ConflictingFlags,
}

/// The editable properties of a single form field.
///
/// Build one with [`FieldProperties::new`] or [`FieldProperties::from_spec`]
/// and the `with_*` / boolean builder methods. Getters expose the final values
/// without allowing mutation after insertion into a [`PropertySet`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FieldProperties {
    name: String,
    field_type: FieldType,
    tooltip: Option<String>,
    required: bool,
    read_only: bool,
    max_len: Option<usize>,
    format: Option<Format>,
    choices: Vec<String>,
}

impl FieldProperties {
    /// Creates a property set for `name` with no optional constraints.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            tooltip: None,
            required: false,
            read_only: false,
            max_len: None,
            format: None,
            choices: Vec::new(),
        }
    }

    /// Derives properties from an emission [`FieldSpec`].
    ///
    /// Name, type, tooltip, required/read-only flags, maximum length and choice
    /// options are copied. A `FieldSpec` carries no value format, so
    /// [`FieldProperties::format`] is `None`; set one with
    /// [`FieldProperties::with_format`] when numeric/currency/date validation
    /// is needed.
    pub fn from_spec(spec: &FieldSpec) -> Self {
        Self::new(spec.name(), spec.field_type())
            .with_optional_tooltip(spec.tooltip())
            .with_required(spec.flags().is_required())
            .with_read_only(spec.flags().is_read_only())
            .with_optional_max_len(spec.max_len())
            .with_choices(spec.options().iter().cloned())
    }

    /// Sets the `/TU` tooltip.
    #[must_use]
    pub fn with_tooltip(mut self, tooltip: impl Into<String>) -> Self {
        self.tooltip = Some(tooltip.into());
        self
    }

    fn with_optional_tooltip(mut self, tooltip: Option<&str>) -> Self {
        self.tooltip = tooltip.map(ToOwned::to_owned);
        self
    }

    /// Sets whether the field is required.
    #[must_use]
    pub fn with_required(mut self, required: bool) -> Self {
        self.required = required;
        self
    }

    /// Sets whether the field is read-only.
    #[must_use]
    pub fn with_read_only(mut self, read_only: bool) -> Self {
        self.read_only = read_only;
        self
    }

    /// Sets the maximum text length.
    #[must_use]
    pub fn with_max_len(mut self, max_len: usize) -> Self {
        self.max_len = Some(max_len);
        self
    }

    fn with_optional_max_len(mut self, max_len: Option<usize>) -> Self {
        self.max_len = max_len;
        self
    }

    /// Sets the expected value [`Format`].
    #[must_use]
    pub fn with_format(mut self, format: Format) -> Self {
        self.format = Some(format);
        self
    }

    /// Sets the allowed choice/radio options.
    #[must_use]
    pub fn with_choices<I, S>(mut self, choices: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }

    /// Returns the fully qualified property name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field type.
    pub const fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// Returns the tooltip, if set.
    pub fn tooltip(&self) -> Option<&str> {
        self.tooltip.as_deref()
    }

    /// Returns whether the field is required.
    pub const fn is_required(&self) -> bool {
        self.required
    }

    /// Returns whether the field is read-only.
    pub const fn is_read_only(&self) -> bool {
        self.read_only
    }

    /// Returns the maximum text length, if set.
    pub const fn max_len(&self) -> Option<usize> {
        self.max_len
    }

    /// Returns the expected value format, if set.
    pub fn format(&self) -> Option<&Format> {
        self.format.as_ref()
    }

    /// Returns the allowed choice/radio options.
    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    /// Validates the definition and returns the first [`PropertyError`].
    ///
    /// # Errors
    ///
    /// Returns [`PropertyError`] for an empty, over-long or control-character
    /// name, an empty, over-long or control-character tooltip, or the
    /// incompatible required+read-only flag pair.
    pub fn validate_definition(&self) -> Result<(), PropertyError> {
        match self.definition_problems().into_iter().next() {
            Some(problem) => Err(problem),
            None => Ok(()),
        }
    }

    /// Returns every typed definition error for these properties.
    pub fn definition_errors(&self) -> Vec<ValidationError> {
        self.definition_problems()
            .iter()
            .map(|problem| self.to_validation_error(problem))
            .collect()
    }

    /// Returns every typed error produced by checking `value`.
    pub fn value_errors(&self, value: &FieldValue) -> Vec<ValidationError> {
        let mut errors = Vec::new();
        let empty = value.is_empty()
            || matches!(value, FieldValue::Text(text) if text.is_empty())
            || matches!(value, FieldValue::Choice(items) if items.is_empty());
        if self.required && empty {
            errors.push(ValidationError::MissingRequired {
                field: self.name.clone(),
            });
        }
        if empty {
            return errors;
        }

        if let Some(max) = self.max_len
            && let Some(text) = value.as_text()
        {
            let actual = text.chars().count();
            if actual > max {
                errors.push(ValidationError::MaxLengthExceeded {
                    field: self.name.clone(),
                    max,
                    actual,
                });
            }
        }

        if let Some(format) = &self.format {
            let text = match value {
                FieldValue::Text(text) => Some(text.clone()),
                FieldValue::Number(number) if number.is_finite() => {
                    if let Format::Currency(currency) = format {
                        let precision = usize::from(currency.decimals());
                        Some(format!("{number:.precision$}"))
                    } else {
                        Some(format!("{number}"))
                    }
                }
                _ => None,
            };
            match text {
                Some(text) => {
                    if let Err(detail) = format.check(&text) {
                        errors.push(ValidationError::InvalidFormat {
                            field: self.name.clone(),
                            format: format.label(),
                            detail,
                        });
                    }
                }
                None => errors.push(ValidationError::InvalidFormat {
                    field: self.name.clone(),
                    format: format.label(),
                    detail: "value is not text or a finite number".to_owned(),
                }),
            }
        }

        if matches!(self.field_type, FieldType::Choice | FieldType::Radio)
            && !self.choices.is_empty()
            && let Some(selected) = value.as_choices()
        {
            for item in selected {
                if !self.choices.iter().any(|choice| choice == item) {
                    errors.push(ValidationError::InvalidChoice {
                        field: self.name.clone(),
                        value: item.clone(),
                    });
                }
            }
        }
        errors
    }

    /// Validates these properties and one value into a deterministic summary.
    pub fn validate_value(&self, value: &FieldValue) -> ValidationSummary {
        let mut errors = self.definition_errors();
        errors.extend(self.value_errors(value));
        ValidationSummary::new(1, errors)
    }

    fn definition_problems(&self) -> Vec<PropertyError> {
        let mut problems = Vec::new();
        if self.name.is_empty() {
            problems.push(PropertyError::EmptyName);
        } else {
            let length = self.name.chars().count();
            if length > MAX_NAME_LEN {
                problems.push(PropertyError::InvalidName(format!(
                    "{length} characters exceed the maximum {MAX_NAME_LEN}"
                )));
            }
            if self.name.chars().any(char::is_control) {
                problems.push(PropertyError::InvalidName(
                    "contains a control character".to_owned(),
                ));
            }
        }
        if let Some(tooltip) = &self.tooltip {
            if tooltip.is_empty() {
                problems.push(PropertyError::InvalidTooltip("tooltip is empty".to_owned()));
            } else {
                let length = tooltip.chars().count();
                if length > MAX_TOOLTIP_LEN {
                    problems.push(PropertyError::InvalidTooltip(format!(
                        "{length} characters exceed the maximum {MAX_TOOLTIP_LEN}"
                    )));
                }
                if tooltip.chars().any(char::is_control) {
                    problems.push(PropertyError::InvalidTooltip(
                        "contains a control character".to_owned(),
                    ));
                }
            }
        }
        if self.required && self.read_only {
            problems.push(PropertyError::ConflictingFlags);
        }
        problems
    }

    fn to_validation_error(&self, problem: &PropertyError) -> ValidationError {
        let field = self.name.clone();
        match problem {
            PropertyError::EmptyName => ValidationError::InvalidName {
                field,
                reason: "name is empty".to_owned(),
            },
            PropertyError::InvalidName(reason) | PropertyError::DuplicateName(reason) => {
                ValidationError::InvalidName {
                    field,
                    reason: reason.clone(),
                }
            }
            PropertyError::InvalidTooltip(reason) => ValidationError::InvalidTooltip {
                field,
                reason: reason.clone(),
            },
            PropertyError::ConflictingFlags => ValidationError::ConflictingFlags {
                field,
                reason: "a field cannot be both required and read-only".to_owned(),
            },
        }
    }
}

impl fmt::Display for FieldProperties {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} ({})", self.name, self.field_type)
    }
}

/// A deterministic, name-sorted collection of field properties.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct PropertySet {
    properties: BTreeMap<String, FieldProperties>,
}

impl PropertySet {
    /// Creates an empty property set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts properties, rejecting a duplicate fully qualified name.
    ///
    /// Structural constraints are checked by [`PropertySet::validate`] rather
    /// than here so a caller can inspect the typed errors for an invalid
    /// definition instead of having insertion fail early.
    ///
    /// # Errors
    ///
    /// Returns [`PropertyError::DuplicateName`] when the name is already used.
    pub fn add(&mut self, properties: FieldProperties) -> Result<(), PropertyError> {
        if self.properties.contains_key(properties.name()) {
            return Err(PropertyError::DuplicateName(properties.name().to_owned()));
        }
        self.properties
            .insert(properties.name().to_owned(), properties);
        Ok(())
    }

    /// Returns the properties in ascending name order.
    pub fn list(&self) -> Vec<&FieldProperties> {
        self.properties.values().collect()
    }

    /// Returns the properties with the given fully qualified name.
    pub fn get(&self, name: &str) -> Option<&FieldProperties> {
        self.properties.get(name)
    }

    /// Returns the number of properties.
    pub fn len(&self) -> usize {
        self.properties.len()
    }

    /// Returns `true` when the set has no properties.
    pub fn is_empty(&self) -> bool {
        self.properties.is_empty()
    }

    /// Validates every property definition and every matching form value.
    ///
    /// Properties with no matching field produce
    /// [`ValidationError::UnknownField`]. The returned summary sorts all errors
    /// by field name and rank, so the report is fully deterministic and needs
    /// no UI to interpret.
    pub fn validate(&self, form: &Form) -> ValidationSummary {
        let mut errors = Vec::new();
        for properties in self.list() {
            errors.extend(properties.definition_errors());
            match form.field(properties.name()) {
                Some(field) => errors.extend(properties.value_errors(field.value())),
                None => errors.push(ValidationError::UnknownField {
                    field: properties.name().to_owned(),
                }),
            }
        }
        ValidationSummary::new(form.len(), errors)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::appearance::WidgetRect;
    use crate::field::{Field, FieldFlags};
    use crate::rules::{CurrencyFormat, DateLayout, Format as ValueFormat};

    fn form() -> Form {
        Form::from_fields([
            Field::new("zeta", FieldType::Text).with_value(FieldValue::Text("Ada".into())),
            Field::new("alpha", FieldType::Text).with_value(FieldValue::Text("42".into())),
            Field::new("color", FieldType::Choice)
                .with_choices(["red", "green"])
                .with_value(FieldValue::Choice(vec!["red".into()])),
        ])
        .unwrap()
    }

    #[test]
    fn property_set_lists_by_name_deterministically() {
        let mut set = PropertySet::new();
        for name in ["zeta", "alpha", "mid"] {
            set.add(FieldProperties::new(name, FieldType::Text))
                .unwrap();
        }
        let first: Vec<&str> = set.list().iter().map(|p| p.name()).collect();
        assert_eq!(first, ["alpha", "mid", "zeta"]);

        let mut reversed = PropertySet::new();
        for name in ["mid", "zeta", "alpha"] {
            reversed
                .add(FieldProperties::new(name, FieldType::Text))
                .unwrap();
        }
        assert_eq!(set, reversed);
        assert_eq!(set.get("alpha").map(FieldProperties::name), Some("alpha"));
        assert_eq!(set.len(), 3);
        assert!(!set.is_empty());
    }

    #[test]
    fn property_validation_success_matrix() {
        let mut set = PropertySet::new();
        set.add(
            FieldProperties::new("zeta", FieldType::Text)
                .with_required(true)
                .with_max_len(5)
                .with_tooltip("User name"),
        )
        .unwrap();
        set.add(FieldProperties::new("alpha", FieldType::Text).with_format(ValueFormat::Numeric))
            .unwrap();
        set.add(FieldProperties::new("color", FieldType::Choice).with_choices(["red", "green"]))
            .unwrap();

        let summary = set.validate(&form());
        assert!(summary.is_valid(), "{:?}", summary.errors());
        assert_eq!(summary.error_count(), 0);

        let currency = FieldProperties::new("amount", FieldType::Text)
            .with_format(ValueFormat::Currency(CurrencyFormat::usd()));
        assert!(
            currency
                .validate_value(&FieldValue::Text("$1,234.56".into()))
                .is_valid()
        );
        let date = FieldProperties::new("when", FieldType::Text)
            .with_format(ValueFormat::Date(DateLayout::Iso));
        assert!(
            date.validate_value(&FieldValue::Text("2024-02-29".into()))
                .is_valid()
        );
    }

    #[test]
    fn property_validation_failure_matrix() {
        let mut form = form();
        form.set_value("zeta", FieldValue::Text(String::new()))
            .unwrap_or(());
        let mut set = PropertySet::new();
        set.add(FieldProperties::new("zeta", FieldType::Text).with_required(true))
            .unwrap();
        set.add(
            FieldProperties::new("alpha", FieldType::Text)
                .with_max_len(1)
                .with_format(ValueFormat::Numeric),
        )
        .unwrap();
        set.add(FieldProperties::new("color", FieldType::Choice).with_choices(["blue"]))
            .unwrap();
        set.add(FieldProperties::new("ghost", FieldType::Text))
            .unwrap();
        let summary = set.validate(&form);
        let names = summary.invalid_field_names();
        assert_eq!(names, ["alpha", "color", "ghost", "zeta"]);
        assert!(matches!(
            &summary.errors()[0],
            ValidationError::MaxLengthExceeded { field, .. } if field == "alpha"
        ));
        assert!(summary.errors().iter().any(
            |e| matches!(e, ValidationError::InvalidChoice { field, .. } if field == "color")
        ));
        assert!(
            summary.errors().iter().any(
                |e| matches!(e, ValidationError::MissingRequired { field } if field == "zeta")
            )
        );
        assert!(
            summary
                .errors()
                .iter()
                .any(|e| matches!(e, ValidationError::UnknownField { field } if field == "ghost"))
        );
    }

    #[test]
    fn format_and_length_failures_are_typed() {
        let case = |format: ValueFormat, value: FieldValue| {
            FieldProperties::new("f", FieldType::Text)
                .with_format(format)
                .validate_value(&value)
        };
        assert!(matches!(
            &case(ValueFormat::Numeric, FieldValue::Text("abc".into())).errors()[0],
            ValidationError::InvalidFormat { format, .. } if format == "numeric"
        ));
        assert!(
            case(
                ValueFormat::Currency(CurrencyFormat::usd()),
                FieldValue::Text("12.5".into())
            )
            .error_count()
                == 1
        );
        assert!(
            case(
                ValueFormat::Date(DateLayout::Iso),
                FieldValue::Text("2023-02-29".into())
            )
            .error_count()
                == 1
        );
        let too_long = FieldProperties::new("f", FieldType::Text)
            .with_max_len(2)
            .validate_value(&FieldValue::Text("abcd".into()));
        assert!(matches!(
            &too_long.errors()[0],
            ValidationError::MaxLengthExceeded { max, actual, .. } if *max == 2 && *actual == 4
        ));
    }

    #[test]
    fn duplicate_property_name_is_rejected() {
        let mut set = PropertySet::new();
        set.add(FieldProperties::new("dup", FieldType::Text))
            .unwrap();
        assert_eq!(
            set.add(FieldProperties::new("dup", FieldType::Text)),
            Err(PropertyError::DuplicateName("dup".to_owned()))
        );
        assert_eq!(set.len(), 1);
    }

    #[test]
    fn readonly_required_conflict_is_typed_error() {
        let properties = FieldProperties::new("locked", FieldType::Text)
            .with_required(true)
            .with_read_only(true);
        assert_eq!(
            properties.validate_definition(),
            Err(PropertyError::ConflictingFlags)
        );
        let summary = properties.validate_value(&FieldValue::Empty);
        assert!(summary.errors().iter().any(|error| matches!(
            error,
            ValidationError::ConflictingFlags { field, .. } if field == "locked"
        )));
        assert_eq!(summary.error_count(), 2);
    }

    #[test]
    fn name_and_tooltip_constraints_are_enforced() {
        let empty = FieldProperties::new("", FieldType::Text);
        assert_eq!(empty.validate_definition(), Err(PropertyError::EmptyName));
        assert!(matches!(
            &empty.definition_errors()[0],
            ValidationError::InvalidName { .. }
        ));

        let long_name = FieldProperties::new("x".repeat(MAX_NAME_LEN + 1), FieldType::Text);
        assert!(matches!(
            long_name.validate_definition(),
            Err(PropertyError::InvalidName(_))
        ));
        let control = FieldProperties::new("bad\u{7}name", FieldType::Text);
        assert!(matches!(
            control.validate_definition(),
            Err(PropertyError::InvalidName(_))
        ));

        let empty_tip = FieldProperties::new("f", FieldType::Text).with_tooltip("");
        assert!(matches!(
            empty_tip.validate_definition(),
            Err(PropertyError::InvalidTooltip(_))
        ));
        let long_tip = FieldProperties::new("f", FieldType::Text)
            .with_tooltip("y".repeat(MAX_TOOLTIP_LEN + 1));
        assert!(matches!(
            long_tip.validate_definition(),
            Err(PropertyError::InvalidTooltip(_))
        ));
        let control_tip = FieldProperties::new("f", FieldType::Text).with_tooltip("a\n\u{1}");
        assert!(matches!(
            control_tip.validate_definition(),
            Err(PropertyError::InvalidTooltip(_))
        ));
    }

    #[test]
    fn properties_from_spec_round_trip_flags() {
        let spec = FieldSpec::text("order.name", WidgetRect::new(1.0, 2.0, 30.0, 12.0), 0)
            .with_tooltip("Name")
            .with_max_len(40)
            .with_flags(FieldFlags::REQUIRED);
        let properties = FieldProperties::from_spec(&spec);
        assert_eq!(properties.name(), "order.name");
        assert_eq!(properties.field_type(), FieldType::Text);
        assert_eq!(properties.tooltip(), Some("Name"));
        assert_eq!(properties.max_len(), Some(40));
        assert!(properties.is_required());
        assert!(!properties.is_read_only());
        assert_eq!(properties.to_string(), "order.name (text)");
    }

    #[test]
    fn choice_and_radio_membership_is_checked() {
        let properties = FieldProperties::new("size", FieldType::Radio)
            .with_choices(["S", "M"])
            .with_required(true);
        let valid = properties.validate_value(&FieldValue::Choice(vec!["M".into()]));
        assert!(valid.is_valid(), "{:?}", valid.errors());
        let invalid = properties.validate_value(&FieldValue::Choice(vec!["XL".into()]));
        assert!(matches!(
            &invalid.errors()[0],
            ValidationError::InvalidChoice { value, .. } if value == "XL"
        ));
        let empty_choice = properties.validate_value(&FieldValue::Choice(Vec::new()));
        assert_eq!(empty_choice.error_count(), 1);
    }
}
