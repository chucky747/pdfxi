//! Structured errors for the navigation (outline, link, and attachment) models.
//!
//! Every validation or conversion failure while mapping a bookmark tree, a
//! link annotation, or an embedded-file name tree to and from a [`Dict`] is
//! reported as a [`NavigationError`]. The variants are deliberately fine
//! grained so that callers can distinguish, for example, a reference cycle in
//! an outline chain from an orphaned child, without parsing human-readable
//! strings.

use core::fmt;

use crate::geometry::GeometryError;
use crate::pdf::PdfError;

impl From<GeometryError> for NavigationError {
    fn from(error: GeometryError) -> Self {
        Self::InvalidRect(error)
    }
}

impl From<PdfError> for NavigationError {
    fn from(error: PdfError) -> Self {
        match error {
            PdfError::MissingKey(key) => Self::MissingKey(key),
            PdfError::WrongType { key, expected } => Self::WrongType { key, expected },
            PdfError::NonFinite { key } => Self::NonFinite { key },
            PdfError::InvalidRect(err) => Self::InvalidRect(err),
            PdfError::InvalidColorComponent { index, value } => {
                Self::InvalidColorComponent { index, value }
            }
            other => Self::InvalidName {
                key: "value",
                value: other.to_string(),
            },
        }
    }
}

/// A failure while converting a navigation structure to or from a PDF dictionary.
#[derive(Debug, Clone, PartialEq)]
pub enum NavigationError {
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
    /// A name value did not match an expected token.
    InvalidName {
        /// Key of the offending name.
        key: &'static str,
        /// The rejected name.
        value: String,
    },
    /// A bookmark title or name-tree key was empty.
    EmptyName {
        /// Key of the offending field.
        key: &'static str,
    },
    /// A bookmark title or name-tree key exceeded the allowed byte length.
    NameTooLong {
        /// Key of the offending field.
        key: &'static str,
        /// Maximum permitted length in bytes.
        max: usize,
        /// Length of the supplied value in bytes.
        got: usize,
    },
    /// An outline item id appeared more than once in the `/Items` table.
    DuplicateId(u64),
    /// A reference id could not be converted to a non-negative integer.
    NegativeId(i64),
    /// A page index was too large to store in a dictionary integer.
    PageIndexTooLarge(usize),
    /// A referenced item id was not present in the `/Items` table.
    MissingItem {
        /// The dangling id.
        id: u64,
    },
    /// A `/Parent` reference pointed at a non-existent item.
    UnknownParent {
        /// Id of the child item.
        id: u64,
        /// Id of the missing parent.
        parent: u64,
    },
    /// A reference chain looped back on itself instead of terminating.
    Cycle {
        /// Id of the item where the cycle was detected.
        id: u64,
    },
    /// An item was present but not reachable from any parent's sibling chain.
    Orphan {
        /// Id of the orphaned item.
        id: u64,
    },
    /// The `Next`/`Prev`/`First`/`Last` links around a sibling chain disagreed.
    BrokenSiblingLink {
        /// Id of the item where the inconsistency was detected.
        id: u64,
    },
    /// A destination fit name was not recognised.
    InvalidFit(String),
    /// A destination had the wrong number of parameters for its fit type.
    InvalidFitArguments {
        /// The fit name.
        fit: String,
        /// Expected number of parameters.
        expected: usize,
        /// Number of parameters supplied.
        got: usize,
    },
    /// A destination array was too short to contain a page and a fit name.
    InvalidDestination {
        /// Dictionary key holding the destination.
        key: &'static str,
        /// Number of array elements supplied.
        got: usize,
    },
    /// A link highlight name was not recognised.
    InvalidHighlight(String),
    /// A link action subtype was unsupported.
    InvalidAction(String),
    /// A name-tree `/Names` array did not contain key/value pairs.
    OddNameTreeEntries,
    /// A name-tree key appeared more than once.
    DuplicateName(String),
    /// A rectangle could not be normalized from its four coordinates.
    InvalidRect(GeometryError),
    /// A colour component was non-finite or outside `0.0..=1.0`.
    InvalidColorComponent {
        /// Zero-based index into the colour array.
        index: usize,
        /// The rejected component.
        value: f64,
    },
}

impl fmt::Display for NavigationError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingKey(key) => write!(f, "missing required PDF key `/{key}`"),
            Self::WrongType { key, expected } => {
                write!(f, "PDF key `/{key}` must be {expected}")
            }
            Self::NonFinite { key } => {
                write!(f, "PDF key `/{key}` must contain only finite numbers")
            }
            Self::InvalidName { key, value } => {
                write!(f, "PDF key `/{key}` has invalid name `{value}`")
            }
            Self::EmptyName { key } => write!(f, "PDF key `/{key}` must not be empty"),
            Self::NameTooLong { key, max, got } => {
                write!(
                    f,
                    "PDF key `/{key}` is {got} bytes, exceeding the {max} byte limit"
                )
            }
            Self::DuplicateId(id) => write!(f, "duplicate outline item id {id}"),
            Self::NegativeId(value) => {
                write!(f, "outline reference id {value} must not be negative")
            }
            Self::PageIndexTooLarge(index) => {
                write!(f, "page index {index} is too large to serialize")
            }
            Self::MissingItem { id } => write!(f, "outline item {id} is referenced but missing"),
            Self::UnknownParent { id, parent } => {
                write!(f, "outline item {id} has unknown parent {parent}")
            }
            Self::Cycle { id } => write!(f, "outline reference cycle detected at item {id}"),
            Self::Orphan { id } => write!(f, "outline item {id} is orphaned"),
            Self::BrokenSiblingLink { id } => {
                write!(f, "outline item {id} has inconsistent sibling links")
            }
            Self::InvalidFit(fit) => write!(f, "unknown destination fit type `/{fit}`"),
            Self::InvalidFitArguments { fit, expected, got } => write!(
                f,
                "destination fit `/{fit}` expects {expected} parameters, got {got}"
            ),
            Self::InvalidDestination { key, got } => write!(
                f,
                "PDF key `/{key}` needs at least a page and a fit name, got {got} elements"
            ),
            Self::InvalidHighlight(highlight) => {
                write!(f, "unknown link highlight mode `/{highlight}`")
            }
            Self::InvalidAction(action) => write!(f, "unsupported link action `/{action}`"),
            Self::OddNameTreeEntries => {
                write!(f, "name-tree `/Names` array must contain key/value pairs")
            }
            Self::DuplicateName(name) => {
                write!(f, "name-tree key `{name}` appears more than once")
            }
            Self::InvalidRect(err) => write!(f, "invalid link rectangle: {err}"),
            Self::InvalidColorComponent { index, value } => write!(
                f,
                "colour component {index} must be finite and in 0.0..=1.0, got {value}"
            ),
        }
    }
}

impl std::error::Error for NavigationError {
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
    fn error_messages_are_descriptive() {
        let cases = [
            NavigationError::MissingKey("Items"),
            NavigationError::WrongType {
                key: "C".into(),
                expected: "an array",
            },
            NavigationError::NonFinite { key: "Dest".into() },
            NavigationError::InvalidName {
                key: "S",
                value: String::new(),
            },
            NavigationError::EmptyName { key: "Title" },
            NavigationError::NameTooLong {
                key: "F",
                max: 4,
                got: 9,
            },
            NavigationError::DuplicateId(7),
            NavigationError::NegativeId(-1),
            NavigationError::PageIndexTooLarge(usize::MAX),
            NavigationError::MissingItem { id: 3 },
            NavigationError::UnknownParent { id: 2, parent: 9 },
            NavigationError::Cycle { id: 1 },
            NavigationError::Orphan { id: 5 },
            NavigationError::BrokenSiblingLink { id: 4 },
            NavigationError::InvalidFit("Bogus".into()),
            NavigationError::InvalidFitArguments {
                fit: "FitH".into(),
                expected: 1,
                got: 2,
            },
            NavigationError::InvalidDestination {
                key: "Dest",
                got: 1,
            },
            NavigationError::InvalidHighlight("Q".into()),
            NavigationError::InvalidAction("Launch".into()),
            NavigationError::OddNameTreeEntries,
            NavigationError::DuplicateName("dup".into()),
            NavigationError::InvalidRect(GeometryError::InvalidRotation(45)),
            NavigationError::InvalidColorComponent {
                index: 0,
                value: 2.0,
            },
        ];
        for case in cases {
            assert!(!case.to_string().is_empty());
        }
    }
}
