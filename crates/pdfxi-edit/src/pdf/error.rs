//! Structured errors for PDF annotation dictionary conversion.
//!
//! Every failure while mapping an [`Annotation`](crate::Annotation) to a
//! [`Dict`](super::value::Dict), or while validating a parsed dictionary, is
//! reported as a [`PdfError`]. The variants are deliberately fine-grained so a
//! caller can tell a missing key from a malformed value without parsing strings.

use core::fmt;

use crate::geometry::GeometryError;

/// A failure while converting an annotation to or from a PDF dictionary.
#[derive(Debug, Clone, PartialEq)]
pub enum PdfError {
    /// A required dictionary key was absent.
    MissingKey(&'static str),
    /// A dictionary value had an unexpected type.
    WrongType {
        /// Key whose value was malformed.
        key: String,
        /// Human-readable name of the expected value type.
        expected: &'static str,
    },
    /// A numeric value was infinite or `NaN`.
    NonFinite {
        /// Key whose value was non-finite.
        key: String,
    },
    /// A rectangle could not be normalized from its four coordinates.
    InvalidRect(GeometryError),
    /// A rectangle had zero width or height after normalization.
    DegenerateRect,
    /// A colour component was non-finite or outside `0.0..=1.0`.
    InvalidColorComponent {
        /// Zero-based index into the colour array.
        index: usize,
        /// The rejected component.
        value: f64,
    },
    /// A required point array was absent.
    MissingPoints(&'static str),
    /// A point array contained fewer points than the subtype requires.
    InsufficientPoints {
        /// Key of the offending array.
        key: &'static str,
        /// Minimum number of points required.
        expected: usize,
        /// Number of points supplied.
        got: usize,
    },
    /// A point array had an odd number of coordinates.
    OddPointCoordinates(&'static str),
    /// A name value was empty or otherwise invalid.
    InvalidName {
        /// Key of the offending name.
        key: &'static str,
        /// The rejected name.
        value: String,
    },
    /// A subtype name could not be mapped back to an [`AnnotationKind`](crate::AnnotationKind).
    ///
    /// Unknown subtypes are preserved as [`PdfSubtype::Opaque`](super::serialize::PdfSubtype::Opaque)
    /// in the dictionary representation, but the older [`Annotation`](crate::Annotation)
    /// model has no corresponding kind, so conversion stops here.
    UnknownSubtype(String),
    /// A border dash pattern contained a non-finite or non-positive length.
    InvalidDashLength {
        /// Zero-based index into the dash array.
        index: usize,
        /// The rejected value.
        value: f64,
    },
}

impl fmt::Display for PdfError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey(key) => write!(f, "missing required PDF key `/{key}`"),
            Self::WrongType { key, expected } => {
                write!(f, "PDF key `/{key}` must be {expected}")
            }
            Self::NonFinite { key } => {
                write!(f, "PDF key `/{key}` must contain only finite numbers")
            }
            Self::InvalidRect(err) => write!(f, "invalid PDF rectangle: {err}"),
            Self::DegenerateRect => {
                write!(f, "PDF rectangle must have positive width and height")
            }
            Self::InvalidColorComponent { index, value } => write!(
                f,
                "PDF colour component {index} must be finite and in 0.0..=1.0, got {value}"
            ),
            Self::MissingPoints(key) => write!(f, "PDF key `/{key}` is required and is missing"),
            Self::InsufficientPoints { key, expected, got } => write!(
                f,
                "PDF key `/{key}` needs at least {expected} points, got {got}"
            ),
            Self::OddPointCoordinates(key) => {
                write!(f, "PDF key `/{key}` must contain x/y coordinate pairs")
            }
            Self::InvalidName { key, value } => {
                write!(f, "PDF key `/{key}` has invalid name `{value}`")
            }
            Self::UnknownSubtype(subtype) => {
                write!(
                    f,
                    "PDF subtype `/{subtype}` has no AnnotationKind equivalent"
                )
            }
            Self::InvalidDashLength { index, value } => write!(
                f,
                "PDF border dash length {index} must be finite and greater than zero, got {value}"
            ),
        }
    }
}

impl std::error::Error for PdfError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::InvalidRect(err) => Some(err),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pdf_error_messages_are_descriptive() {
        let cases = [
            PdfError::MissingKey("Rect"),
            PdfError::WrongType {
                key: "C".into(),
                expected: "an array",
            },
            PdfError::NonFinite { key: "CA".into() },
            PdfError::DegenerateRect,
            PdfError::InvalidColorComponent {
                index: 1,
                value: 2.0,
            },
            PdfError::MissingPoints("L"),
            PdfError::InsufficientPoints {
                key: "L",
                expected: 2,
                got: 1,
            },
            PdfError::OddPointCoordinates("InkList"),
            PdfError::InvalidName {
                key: "S",
                value: String::new(),
            },
            PdfError::UnknownSubtype("Widget".into()),
            PdfError::InvalidDashLength {
                index: 0,
                value: 0.0,
            },
        ];
        for case in cases {
            assert!(!case.to_string().is_empty());
        }
    }
}
