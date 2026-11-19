//! Typed representation of PDF objects.
//!
//! The parser turns the token stream of an indirect object into values of
//! [`Object`]. This is intentionally a small, allocation-friendly model: it
//! covers the objects needed to navigate a document (names, numbers, arrays,
//! dictionaries, indirect references and streams) without pulling in a full
//! content-stream interpreter.

use std::collections::BTreeMap;

/// Identifier of an indirect PDF object.
///
/// Object numbers are positive integers and generation numbers are normally
/// zero. Together they uniquely address one object in a document's
/// cross-reference table.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct ObjectId {
    /// The object number.
    pub number: u32,
    /// The generation number.
    pub generation: u16,
}

impl ObjectId {
    /// Construct an indirect object identifier.
    pub fn new(number: u32, generation: u16) -> Self {
        Self { number, generation }
    }
}

impl std::fmt::Display for ObjectId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{} {} R", self.number, self.generation)
    }
}

/// An ordered mapping from PDF name keys to objects.
pub type Dictionary = BTreeMap<String, Object>;

/// A single PDF object value.
#[derive(Debug, Clone, PartialEq)]
pub enum Object {
    /// The `null` object.
    Null,
    /// A Boolean (`true` or `false`) object.
    Boolean(bool),
    /// An integer number.
    Integer(i64),
    /// A real number.
    Real(f64),
    /// A name object, stored without its leading slash.
    Name(String),
    /// A literal or hexadecimal string, stored as its raw bytes.
    String(Vec<u8>),
    /// An ordered sequence of objects.
    Array(Vec<Object>),
    /// A dictionary mapping names to objects.
    Dictionary(Dictionary),
    /// An indirect reference to another object.
    Reference(ObjectId),
    /// A stream: a dictionary followed by a byte payload.
    Stream {
        /// The stream dictionary describing the payload.
        dictionary: Dictionary,
        /// The raw stream bytes, without a filter applied.
        data: Vec<u8>,
    },
}

impl Object {
    /// Return the name when this object is a name.
    pub fn as_name(&self) -> Option<&str> {
        match self {
            Self::Name(name) => Some(name),
            _ => None,
        }
    }

    /// Return the dictionary when this object is a dictionary.
    ///
    /// Stream dictionaries are also returned.
    pub fn as_dictionary(&self) -> Option<&Dictionary> {
        match self {
            Self::Dictionary(dict) => Some(dict),
            Self::Stream { dictionary, .. } => Some(dictionary),
            _ => None,
        }
    }

    /// Return the array when this object is an array.
    pub fn as_array(&self) -> Option<&[Object]> {
        match self {
            Self::Array(items) => Some(items),
            _ => None,
        }
    }

    /// Return the identifier when this object is an indirect reference.
    pub fn as_reference(&self) -> Option<ObjectId> {
        match self {
            Self::Reference(id) => Some(*id),
            _ => None,
        }
    }

    /// Return the value as an `i64` when it is an integer.
    pub fn as_i64(&self) -> Option<i64> {
        match self {
            Self::Integer(value) => Some(*value),
            _ => None,
        }
    }

    /// Return the value as an `f64` when it is numeric.
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Integer(value) => Some(*value as f64),
            Self::Real(value) => Some(*value),
            _ => None,
        }
    }
}
