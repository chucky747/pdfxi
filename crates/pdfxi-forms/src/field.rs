//! AcroForm field primitives: [`FieldType`], [`FieldFlags`], [`FieldValue`],
//! [`TextConstraint`], and the [`Field`] model.
//!
//! A [`Field`] is the in-memory representation of a single AcroForm widget. Its
//! `name` is the *fully qualified* name (for example `order.address.city`), so
//! no hierarchical name resolution is performed by this crate; callers supply
//! the final dotted path. This mirrors how FDF/XFDF and field APIs address
//! fields by their complete name.

use crate::validation::ValidationError;
use std::fmt;

/// The kind of an AcroForm field.
///
/// This is an open-ended classification: fields whose subtype is not modelled
/// yet are represented as [`FieldType::Unknown`] rather than being rejected, so
/// a form can be inspected without loss.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FieldType {
    /// A text field (single-line or multiline, password, file-select).
    Text,
    /// A checkbox field with an on/off state.
    CheckBox,
    /// A radio button group; one option is selected.
    Radio,
    /// A choice field (list box or combo box), possibly multi-select.
    Choice,
    /// A pushbutton or other button-like field that carries no value.
    Button,
    /// A digital signature field.
    Signature,
    /// A field type that is not recognized or is defined by a future spec.
    Unknown,
}

impl fmt::Display for FieldType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let label = match self {
            Self::Text => "text",
            Self::CheckBox => "checkbox",
            Self::Radio => "radio",
            Self::Choice => "choice",
            Self::Button => "button",
            Self::Signature => "signature",
            Self::Unknown => "unknown",
        };
        f.write_str(label)
    }
}

/// A set of AcroForm field flags.
///
/// This is a thin, dependency-free bit set over the `u32` field-flag value
/// stored in a PDF's `/Ff` entry. Bit positions 1-3 are common to all field types
/// ([`FieldFlags::REQUIRED`], [`FieldFlags::READ_ONLY`],
/// [`FieldFlags::NO_EXPORT`]); the remaining named bits are the commonly used
/// text- and choice-specific flags. Unknown bits are preserved so a flag word
/// can round-trip without losing information.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct FieldFlags(u32);

impl FieldFlags {
    /// No flags set.
    pub const NONE: Self = Self(0);
    /// The field is required (PDF `/Ff` bit position 2, mask `1 << 1`).
    pub const REQUIRED: Self = Self(1 << 1);
    /// The field is read-only (PDF `/Ff` bit position 1, mask `1 << 0`).
    pub const READ_ONLY: Self = Self(1 << 0);
    /// The field must not be exported (PDF `/Ff` bit 3).
    pub const NO_EXPORT: Self = Self(1 << 2);
    /// A text field accepts multiple lines (PDF `/Ff` bit 13).
    pub const MULTILINE: Self = Self(1 << 12);
    /// A text field is a password field (PDF `/Ff` bit 14).
    pub const PASSWORD: Self = Self(1 << 13);
    /// A choice field is a combo box (PDF `/Ff` bit 18).
    pub const COMBO: Self = Self(1 << 17);
    /// A combo box allows editing its text (PDF `/Ff` bit 19).
    pub const CHOICE_EDIT: Self = Self(1 << 18);
    /// A choice field sorts its options (PDF `/Ff` bit 20).
    pub const SORT: Self = Self(1 << 19);

    /// Builds a flag set from a raw `u32` bit pattern, preserving unknown bits.
    pub const fn from_bits(bits: u32) -> Self {
        Self(bits)
    }

    /// Returns the raw `u32` bit pattern.
    pub const fn bits(self) -> u32 {
        self.0
    }

    /// Returns `true` if every bit in `other` is set in `self`.
    pub const fn contains(self, other: Self) -> bool {
        (self.0 & other.0) == other.0
    }

    /// Returns the union of two flag sets.
    #[must_use]
    pub const fn union(self, other: Self) -> Self {
        Self(self.0 | other.0)
    }

    /// Returns `true` if the required bit is set.
    pub const fn is_required(self) -> bool {
        (self.0 & Self::REQUIRED.0) != 0
    }

    /// Returns `true` if the read-only bit is set.
    pub const fn is_read_only(self) -> bool {
        (self.0 & Self::READ_ONLY.0) != 0
    }

    /// Returns `true` if the no-export bit is set.
    pub const fn is_no_export(self) -> bool {
        (self.0 & Self::NO_EXPORT.0) != 0
    }
}

/// The broad category of a [`FieldValue`].
///
/// [`Field::accepts`] uses this coarse classification to reject values whose
/// representation does not match the widget type before they enter a form.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ValueKind {
    /// [`FieldValue::Empty`].
    Empty,
    /// [`FieldValue::Text`].
    Text,
    /// [`FieldValue::Bool`].
    Bool,
    /// [`FieldValue::Choice`].
    Choice,
    /// [`FieldValue::Number`].
    Number,
}

impl ValueKind {
    /// Returns a stable, human-readable label for this kind.
    pub const fn label(self) -> &'static str {
        match self {
            Self::Empty => "empty",
            Self::Text => "text",
            Self::Bool => "boolean",
            Self::Choice => "choice",
            Self::Number => "number",
        }
    }
}

/// The current or default value of a field.
///
/// The representation is deliberately small and total: every widget state maps
/// to one of these variants, and an untouched field uses [`FieldValue::Empty`].
///
/// Numeric values are represented by [`FieldValue::Number`]. PDF has no separate
/// "number" widget type; a numeric field is a text field with a number format,
/// so [`FieldType::Text`] accepts both [`FieldValue::Text`] and
/// [`FieldValue::Number`]. Calculations operate on [`FieldValue::Number`]
/// values only.
#[derive(Debug, Clone, Default)]
pub enum FieldValue {
    /// The field carries no value.
    #[default]
    Empty,
    /// A textual value (text fields, password fields, signature names).
    Text(String),
    /// A boolean state, used by checkboxes (`true` is the "on" state).
    Bool(bool),
    /// Zero or more selected option names (choice and radio fields).
    ///
    /// A single radio selection is represented as a one-element vector; a
    /// multi-select list box holds several entries.
    Choice(Vec<String>),
    /// A numeric value, used by formatted number fields and calculations.
    ///
    /// Non-finite values are rejected by FDF export and calculation evaluation.
    Number(f64),
}

/// Canonicalises the bit pattern of an `f64` so equality is a total relation.
///
/// All NaNs compare equal and the two zeros compare equal, which lets
/// [`FieldValue`] implement [`Eq`] despite holding a floating-point number.
fn canonical_f64_bits(value: f64) -> u64 {
    if value.is_nan() {
        f64::NAN.to_bits()
    } else if value == 0.0 {
        0.0_f64.to_bits()
    } else {
        value.to_bits()
    }
}

impl PartialEq for FieldValue {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Empty, Self::Empty) => true,
            (Self::Text(a), Self::Text(b)) => a == b,
            (Self::Bool(a), Self::Bool(b)) => a == b,
            (Self::Choice(a), Self::Choice(b)) => a == b,
            (Self::Number(a), Self::Number(b)) => canonical_f64_bits(*a) == canonical_f64_bits(*b),
            _ => false,
        }
    }
}

impl Eq for FieldValue {}

impl FieldValue {
    /// Creates a numeric value, rejecting nothing (see [`FieldValue::Number`]).
    pub const fn number(value: f64) -> Self {
        Self::Number(value)
    }

    /// Returns the category of this value.
    pub const fn kind(&self) -> ValueKind {
        match self {
            Self::Empty => ValueKind::Empty,
            Self::Text(_) => ValueKind::Text,
            Self::Bool(_) => ValueKind::Bool,
            Self::Choice(_) => ValueKind::Choice,
            Self::Number(_) => ValueKind::Number,
        }
    }

    /// Returns `true` when this value is [`FieldValue::Empty`].
    pub fn is_empty(&self) -> bool {
        matches!(self, Self::Empty)
    }

    /// Returns the contained text, if this is a [`FieldValue::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Returns the contained boolean, if this is a [`FieldValue::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the contained option names, if this is a [`FieldValue::Choice`].
    pub fn as_choices(&self) -> Option<&[String]> {
        match self {
            Self::Choice(values) => Some(values),
            _ => None,
        }
    }

    /// Returns the contained number, if this is a [`FieldValue::Number`].
    pub const fn as_number(&self) -> Option<f64> {
        match self {
            Self::Number(value) => Some(*value),
            _ => None,
        }
    }
}

/// Optional constraints applied to a text field's value during validation.
///
/// All configured constraints must hold. Lengths are measured in Unicode
/// scalar values (characters), not bytes, so non-ASCII text is handled fairly.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TextConstraint {
    min_len: Option<usize>,
    max_len: Option<usize>,
    allowed: Option<Vec<String>>,
}

impl TextConstraint {
    /// Creates an unconstrained rule set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Requires at least `len` characters.
    #[must_use]
    pub fn with_min_len(mut self, len: usize) -> Self {
        self.min_len = Some(len);
        self
    }

    /// Requires at most `len` characters.
    #[must_use]
    pub fn with_max_len(mut self, len: usize) -> Self {
        self.max_len = Some(len);
        self
    }

    /// Requires the text to exactly equal one of the given strings.
    #[must_use]
    pub fn with_allowed<I, S>(mut self, allowed: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.allowed = Some(allowed.into_iter().map(Into::into).collect());
        self
    }

    /// Returns the configured minimum length, if any.
    pub fn min_len(&self) -> Option<usize> {
        self.min_len
    }

    /// Returns the configured maximum length, if any.
    pub fn max_len(&self) -> Option<usize> {
        self.max_len
    }

    /// Returns the configured allow-list, if any.
    pub fn allowed(&self) -> Option<&[String]> {
        self.allowed.as_deref()
    }

    /// Checks `text` against every configured constraint.
    ///
    /// Returns `Ok(())` when the text satisfies all rules, otherwise a
    /// human-readable reason suitable for [`ValidationError::InvalidText`].
    pub fn validate(&self, text: &str) -> Result<(), String> {
        let len = text.chars().count();
        if let Some(min) = self.min_len
            && len < min
        {
            return Err(format!(
                "text has {len} characters, fewer than the minimum {min}"
            ));
        }
        if let Some(max) = self.max_len
            && len > max
        {
            return Err(format!(
                "text has {len} characters, more than the maximum {max}"
            ));
        }
        if let Some(allowed) = &self.allowed
            && !allowed.iter().any(|value| value == text)
        {
            return Err("text is not one of the allowed values".to_owned());
        }
        Ok(())
    }
}

/// A single AcroForm field.
///
/// A field owns its fully qualified `name`, [`FieldType`], [`FieldFlags`],
/// current [`FieldValue`], default value, choice options, and optional text
/// [`TextConstraint`]. Build one with [`Field::new`] and the `with_*` builder
/// methods, then add it to a [`crate::Form`].
#[derive(Debug, Clone, PartialEq)]
pub struct Field {
    name: String,
    field_type: FieldType,
    flags: FieldFlags,
    value: FieldValue,
    default_value: FieldValue,
    choices: Vec<String>,
    constraint: TextConstraint,
}

impl Field {
    /// Creates a field with the given fully qualified name and type.
    ///
    /// The new field has no flags, an empty current and default value, no
    /// choice options, and no text constraints.
    pub fn new(name: impl Into<String>, field_type: FieldType) -> Self {
        Self {
            name: name.into(),
            field_type,
            flags: FieldFlags::NONE,
            value: FieldValue::Empty,
            default_value: FieldValue::Empty,
            choices: Vec::new(),
            constraint: TextConstraint::new(),
        }
    }

    /// Sets the field flags.
    #[must_use]
    pub fn with_flags(mut self, flags: FieldFlags) -> Self {
        self.flags = flags;
        self
    }

    /// Sets the allowed export values for choice and radio fields.
    #[must_use]
    pub fn with_choices<I, S>(mut self, choices: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = choices.into_iter().map(Into::into).collect();
        self
    }

    /// Sets the text validation constraints.
    #[must_use]
    pub fn with_constraint(mut self, constraint: TextConstraint) -> Self {
        self.constraint = constraint;
        self
    }

    /// Sets the field's default value.
    ///
    /// The current value is initialised to the same value, matching PDF
    /// behaviour where a field with no explicit `/V` uses its `/DV`.
    #[must_use]
    pub fn with_default(mut self, value: FieldValue) -> Self {
        self.default_value = value.clone();
        self.value = value;
        self
    }

    /// Sets the current value without changing the default.
    #[must_use]
    pub fn with_value(mut self, value: FieldValue) -> Self {
        self.value = value;
        self
    }

    /// Returns the fully qualified field name.
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Returns the field type.
    pub fn field_type(&self) -> FieldType {
        self.field_type
    }

    /// Returns the field flags.
    pub fn flags(&self) -> FieldFlags {
        self.flags
    }

    /// Returns the current value.
    pub fn value(&self) -> &FieldValue {
        &self.value
    }

    /// Returns the default value used by [`Field::reset`].
    pub fn default_value(&self) -> &FieldValue {
        &self.default_value
    }

    /// Returns the allowed export values for choice and radio fields.
    pub fn choices(&self) -> &[String] {
        &self.choices
    }

    /// Returns the text validation constraints.
    pub fn constraint(&self) -> &TextConstraint {
        &self.constraint
    }

    /// Returns `true` if the required flag is set.
    pub fn is_required(&self) -> bool {
        self.flags.is_required()
    }

    /// Returns `true` if the read-only flag is set.
    pub fn is_read_only(&self) -> bool {
        self.flags.is_read_only()
    }

    /// Returns the value categories this field's type can hold.
    ///
    /// The classification is intentionally coarse: it checks the *shape* of a
    /// value, not whether a choice is in the option list or a text satisfies its
    /// constraints (those are checked by [`Field::validate`]).
    ///
    /// - [`FieldType::Text`] accepts [`ValueKind::Text`] and
    ///   [`ValueKind::Number`] (a PDF number field is a text field with a
    ///   number format).
    /// - [`FieldType::CheckBox`] accepts [`ValueKind::Bool`].
    /// - [`FieldType::Radio`] accepts [`ValueKind::Choice`] with at most one
    ///   selection.
    /// - [`FieldType::Choice`] accepts [`ValueKind::Choice`] (one or many
    ///   selections).
    /// - [`FieldType::Button`] accepts only [`ValueKind::Empty`] because a
    ///   pushbutton carries no value.
    /// - [`FieldType::Signature`] accepts [`ValueKind::Text`] (the signing
    ///   name).
    /// - [`FieldType::Unknown`] accepts every kind, preserving forward
    ///   compatibility.
    ///
    /// Every type also accepts [`ValueKind::Empty`] except buttons, which are
    /// always empty.
    pub fn accepted_kinds(&self) -> &'static [ValueKind] {
        match self.field_type {
            FieldType::Text => &[ValueKind::Empty, ValueKind::Text, ValueKind::Number],
            FieldType::CheckBox => &[ValueKind::Empty, ValueKind::Bool],
            FieldType::Radio => &[ValueKind::Empty, ValueKind::Choice],
            FieldType::Choice => &[ValueKind::Empty, ValueKind::Choice],
            FieldType::Button => &[ValueKind::Empty],
            FieldType::Signature => &[ValueKind::Empty, ValueKind::Text],
            FieldType::Unknown => &[
                ValueKind::Empty,
                ValueKind::Text,
                ValueKind::Bool,
                ValueKind::Choice,
                ValueKind::Number,
            ],
        }
    }

    /// Returns `true` when `value` has an acceptable shape for this field.
    pub fn accepts(&self, value: &FieldValue) -> bool {
        if !self.accepted_kinds().contains(&value.kind()) {
            return false;
        }
        // A radio group selects exactly one option at a time.
        if self.field_type == FieldType::Radio
            && let Some(selected) = value.as_choices()
        {
            return selected.len() <= 1;
        }
        true
    }

    /// Replaces the current value.
    ///
    /// Read-only enforcement is the responsibility of [`crate::Form`], which
    /// refuses such writes through its public API.
    pub fn set_value(&mut self, value: FieldValue) {
        self.value = value;
    }

    /// Replaces the field flags.
    ///
    /// Used by the property editor when applying an edit plan. Validation of the
    /// resulting flag combination is the caller's responsibility.
    pub fn set_flags(&mut self, flags: FieldFlags) {
        self.flags = flags;
    }

    /// Replaces the allowed export values for choice and radio fields.
    pub fn set_choices<I, S>(&mut self, choices: I)
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.choices = choices.into_iter().map(Into::into).collect();
    }

    /// Replaces the text validation constraints.
    pub fn set_constraint(&mut self, constraint: TextConstraint) {
        self.constraint = constraint;
    }

    /// Restores the current value to the default value.
    pub fn reset(&mut self) {
        self.value = self.default_value.clone();
    }

    /// Validates this field's current value.
    ///
    /// Returns `Ok(())` when the value is acceptable, otherwise a
    /// [`ValidationError`] describing the problem. Required-but-empty fields,
    /// values of the wrong shape, out-of-range choices, and constrained text
    /// that fails its rules are all reported here; button, checkbox, signature,
    /// and unknown fields have no extra constraints beyond "required" and the
    /// coarse value shape.
    pub fn validate(&self) -> Result<(), ValidationError> {
        if self.value.is_empty() {
            return if self.is_required() {
                Err(ValidationError::MissingRequired {
                    field: self.name.clone(),
                })
            } else {
                Ok(())
            };
        }

        if !self.accepts(&self.value) {
            let expected = self
                .accepted_kinds()
                .iter()
                .map(|kind| kind.label())
                .collect::<Vec<_>>()
                .join("/");
            return Err(ValidationError::InvalidValueType {
                field: self.name.clone(),
                expected,
                found: self.value.kind().label().to_owned(),
            });
        }

        match self.field_type {
            FieldType::Choice | FieldType::Radio => {
                if let Some(selected) = self.value.as_choices() {
                    for value in selected {
                        if !self.choices.iter().any(|option| option == value) {
                            return Err(ValidationError::InvalidChoice {
                                field: self.name.clone(),
                                value: value.clone(),
                            });
                        }
                    }
                }
                Ok(())
            }
            FieldType::Text => {
                if let Some(text) = self.value.as_text()
                    && let Err(reason) = self.constraint.validate(text)
                {
                    return Err(ValidationError::InvalidText {
                        field: self.name.clone(),
                        reason,
                    });
                }
                Ok(())
            }
            FieldType::CheckBox | FieldType::Button | FieldType::Signature | FieldType::Unknown => {
                Ok(())
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn flags_preserve_unknown_bits_and_report_common_bits() {
        let flags = FieldFlags::from_bits(FieldFlags::REQUIRED.bits() | 1 << 30);
        assert!(flags.is_required());
        assert!(!flags.is_read_only());
        assert!(flags.contains(FieldFlags::REQUIRED));
        assert_eq!(flags.bits() & (1 << 30), 1 << 30);
    }

    #[test]
    fn with_default_initializes_current_value() {
        let field = Field::new("f", FieldType::Text).with_default(FieldValue::Text("hi".into()));
        assert_eq!(field.value(), &FieldValue::Text("hi".into()));
        assert_eq!(field.default_value(), field.value());
    }

    #[test]
    fn text_constraint_checks_length_and_allow_list() {
        let rule = TextConstraint::new().with_min_len(2).with_max_len(4);
        assert!(rule.validate("abc").is_ok());
        assert!(rule.validate("a").is_err());
        assert!(rule.validate("abcde").is_err());

        let allowed = TextConstraint::new().with_allowed(["red", "green"]);
        assert!(allowed.validate("red").is_ok());
        assert!(allowed.validate("blue").is_err());
    }
}
