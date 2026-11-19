//! Dependency-free value and dictionary types used as the serialization intermediate.
//!
//! [`Dict`] is an ordered mapping from PDF key names (stored **without** the
//! leading slash) to [`Value`]s. It is intentionally smaller than the full
//! `pdfxi-core` object model: `pdfxi-edit` stays dependency-free and only needs
//! scalars, arrays, and nested dictionaries to describe an annotation. The
//! writer in `pdfxi-core` can consume this intermediate type when the document
//! is eventually saved.

use std::collections::BTreeMap;

use super::error::PdfError;

/// A scalar or array value stored in a PDF dictionary.
#[derive(Clone, Debug, PartialEq)]
pub enum Value {
    /// The PDF `null` object.
    Null,
    /// A Boolean (`true` or `false`) object.
    Bool(bool),
    /// An integer number.
    Int(i64),
    /// A real number.
    Real(f64),
    /// A name object, stored without its leading slash.
    Name(String),
    /// A literal string, stored as its Unicode text.
    Text(String),
    /// An ordered sequence of values.
    Array(Vec<Value>),
    /// A nested dictionary.
    Dictionary(Dict),
}

impl Value {
    /// Returns the name when this value is a [`Value::Name`].
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    /// Returns the text when this value is a [`Value::Text`].
    pub fn as_text(&self) -> Option<&str> {
        match self {
            Self::Text(text) => Some(text),
            _ => None,
        }
    }

    /// Returns the integer when this value is a [`Value::Int`].
    pub fn as_int(&self) -> Option<i64> {
        match self {
            Self::Int(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the value as an `f64` when it is numeric.
    pub fn as_number(&self) -> Option<f64> {
        match self {
            Self::Int(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the Boolean when this value is a [`Value::Bool`].
    pub fn as_bool(&self) -> Option<bool> {
        match self {
            Self::Bool(value) => Some(*value),
            _ => None,
        }
    }

    /// Returns the items when this value is a [`Value::Array`].
    pub fn as_array(&self) -> Option<&[Value]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Returns the mutable items when this value is a [`Value::Array`].
    pub fn as_array_mut(&mut self) -> Option<&mut Vec<Value>> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Returns the dictionary when this value is a [`Value::Dictionary`].
    pub fn as_dict(&self) -> Option<&Dict> {
        match self {
            Self::Dictionary(dict) => Some(dict),
            _ => None,
        }
    }
}

/// An ordered mapping from PDF key names to values.
///
/// A `BTreeMap` is used so serialization is deterministic: two annotations
/// with the same logical content always produce byte-identical key ordering.
pub type Dict = BTreeMap<String, Value>;

/// Reads a required name value from `dict`.
pub(crate) fn expect_name<'a>(dict: &'a Dict, key: &'static str) -> Result<&'a str, PdfError> {
    match dict.get(key) {
        None => Err(PdfError::MissingKey(key)),
        Some(value) => value.as_name().ok_or_else(|| PdfError::WrongType {
            key: key.to_string(),
            expected: "a name",
        }),
    }
}

/// Reads an optional name value from `dict`.
pub(crate) fn optional_name<'a>(
    dict: &'a Dict,
    key: &'static str,
) -> Result<Option<&'a str>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_name()
            .map(Some)
            .ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "a name",
            }),
    }
}

/// Reads an optional text value from `dict`.
pub(crate) fn optional_text<'a>(
    dict: &'a Dict,
    key: &'static str,
) -> Result<Option<&'a str>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_text()
            .map(Some)
            .ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "a string",
            }),
    }
}

/// Reads an optional integer value from `dict`.
pub(crate) fn optional_int(dict: &Dict, key: &'static str) -> Result<Option<i64>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value.as_int().map(Some).ok_or_else(|| PdfError::WrongType {
            key: key.to_string(),
            expected: "an integer",
        }),
    }
}

/// Reads an optional numeric value from `dict`.
pub(crate) fn optional_number(dict: &Dict, key: &'static str) -> Result<Option<f64>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => {
            let number = value.as_number().ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "a number",
            })?;
            if number.is_finite() {
                Ok(Some(number))
            } else {
                Err(PdfError::NonFinite {
                    key: key.to_string(),
                })
            }
        }
    }
}

/// Reads a required Boolean value from `dict`.
pub(crate) fn optional_bool(dict: &Dict, key: &'static str) -> Result<Option<bool>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_bool()
            .map(Some)
            .ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "a boolean",
            }),
    }
}

/// Reads a required array value from `dict`.
pub(crate) fn expect_array<'a>(dict: &'a Dict, key: &'static str) -> Result<&'a [Value], PdfError> {
    match dict.get(key) {
        None => Err(PdfError::MissingKey(key)),
        Some(value) => value.as_array().ok_or_else(|| PdfError::WrongType {
            key: key.to_string(),
            expected: "an array",
        }),
    }
}

/// Reads an optional array value from `dict`.
pub(crate) fn optional_array<'a>(
    dict: &'a Dict,
    key: &'static str,
) -> Result<Option<&'a [Value]>, PdfError> {
    match dict.get(key) {
        None => Ok(None),
        Some(value) => value
            .as_array()
            .map(Some)
            .ok_or_else(|| PdfError::WrongType {
                key: key.to_string(),
                expected: "an array",
            }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn value_accessors_return_typed_views() {
        assert_eq!(Value::Name("Annot".into()).as_name(), Some("Annot"));
        assert_eq!(Value::Text("hi".into()).as_text(), Some("hi"));
        assert_eq!(Value::Int(3).as_int(), Some(3));
        assert_eq!(Value::Real(0.5).as_number(), Some(0.5));
        assert_eq!(Value::Int(2).as_number(), Some(2.0));
        assert_eq!(Value::Bool(true).as_bool(), Some(true));
        assert_eq!(Value::Array(vec![]).as_array(), Some(&[][..]));
        assert_eq!(Value::Name("x".into()).as_int(), None);
    }

    #[test]
    fn dictionary_typed_access_reports_structured_errors() {
        let mut dict = Dict::new();
        dict.insert("A".into(), Value::Name("B".into()));
        assert_eq!(expect_name(&dict, "A"), Ok("B"));
        assert_eq!(expect_name(&dict, "Z"), Err(PdfError::MissingKey("Z")));
        assert_eq!(
            optional_int(&dict, "A"),
            Err(PdfError::WrongType {
                key: "A".into(),
                expected: "an integer",
            })
        );
        assert_eq!(optional_text(&dict, "Z"), Ok(None));
    }
}
