//! Embedded-file attachment model and `/EmbeddedFiles` name-tree round-trip.
//!
//! An [`Attachment`] describes a file carried by the document. The actual file
//! bytes are **not** stored in this intermediate type: `bytes_ref` is an opaque
//! identifier (`XI_StreamRef` in the dictionary) that the caller maps to a real
//! embedded-file stream when the document is written. Only the file
//! specification metadata is round-tripped here.
//!
//! A [`NameTree`] is the flat `/Names` form of a name tree: an ordered list of
//! attachments keyed by their [`Attachment::name`]. Keys are kept sorted so
//! serialization is deterministic and duplicate keys are rejected.

use crate::pdf::{Dict, Value};

use super::{MAX_NAME_BYTES, NavigationError, required_name, validate_name};

/// Default MIME type used when an embedded file does not declare one.
pub const DEFAULT_MIME_TYPE: &str = "application/octet-stream";

/// Metadata for one embedded file attachment.
#[derive(Clone, Debug, PartialEq)]
pub struct Attachment {
    /// Name-tree key under which the attachment is registered.
    pub name: String,
    /// File name shown to the user (`/F` in the file specification).
    pub file_name: String,
    /// Human-readable description (`/Desc`).
    pub description: String,
    /// MIME type of the embedded stream (`/Subtype`).
    pub mime_type: String,
    /// Opaque identifier for the caller-held stream bytes (`XI_StreamRef`).
    pub bytes_ref: String,
}

impl Attachment {
    /// Creates an attachment, validating the name, file name, MIME type, and
    /// stream reference lengths.
    pub fn new(
        name: impl Into<String>,
        file_name: impl Into<String>,
        description: impl Into<String>,
        mime_type: impl Into<String>,
        bytes_ref: impl Into<String>,
    ) -> Result<Self, NavigationError> {
        let name = name.into();
        let file_name = file_name.into();
        let description = description.into();
        let mime_type = mime_type.into();
        let bytes_ref = bytes_ref.into();
        validate_name("Names", &name)?;
        validate_name("F", &file_name)?;
        check_length("Desc", &description)?;
        check_length("Subtype", &mime_type)?;
        check_length("XI_StreamRef", &bytes_ref)?;
        Ok(Self {
            name,
            file_name,
            description,
            mime_type,
            bytes_ref,
        })
    }
}

fn check_length(key: &'static str, text: &str) -> Result<(), NavigationError> {
    let len = text.len();
    if len > MAX_NAME_BYTES {
        return Err(NavigationError::NameTooLong {
            key,
            max: MAX_NAME_BYTES,
            got: len,
        });
    }
    Ok(())
}

/// Serializes an attachment into a PDF file specification dictionary.
///
/// The result is a `/Filespec` dictionary whose `/EF /F` entry is an
/// `/EmbeddedFile` dictionary carrying the MIME type and the opaque
/// `XI_StreamRef`. No file bytes are embedded in the returned value.
pub fn attachment_to_value(attachment: &Attachment) -> Value {
    let mut embedded = Dict::new();
    embedded.insert("Type".to_string(), Value::Name("EmbeddedFile".to_string()));
    embedded.insert(
        "Subtype".to_string(),
        Value::Text(attachment.mime_type.clone()),
    );
    embedded.insert(
        "XI_StreamRef".to_string(),
        Value::Text(attachment.bytes_ref.clone()),
    );

    let mut ef = Dict::new();
    ef.insert("F".to_string(), Value::Dictionary(embedded));

    let mut spec = Dict::new();
    spec.insert("Type".to_string(), Value::Name("Filespec".to_string()));
    spec.insert("F".to_string(), Value::Text(attachment.file_name.clone()));
    spec.insert(
        "Desc".to_string(),
        Value::Text(attachment.description.clone()),
    );
    spec.insert("EF".to_string(), Value::Dictionary(ef));
    Value::Dictionary(spec)
}

/// Parses an attachment from a PDF file specification dictionary.
///
/// The name-tree key is supplied separately by the caller (taken from the
/// alternating `/Names` array) because the file specification itself does not
/// carry it.
pub fn value_to_attachment(value: &Value, name: String) -> Result<Attachment, NavigationError> {
    let spec = value.as_dict().ok_or_else(|| NavigationError::WrongType {
        key: "Names".to_string(),
        expected: "a file specification dictionary",
    })?;
    let file_name = required_name(spec, "F")?;
    let description = match spec.get("Desc") {
        Some(value) => value
            .as_text()
            .ok_or_else(|| NavigationError::WrongType {
                key: "Desc".to_string(),
                expected: "a string",
            })?
            .to_string(),
        None => String::new(),
    };
    check_length("Desc", &description)?;

    let ef = spec
        .get("EF")
        .and_then(Value::as_dict)
        .ok_or(NavigationError::MissingKey("EF"))?;
    let embedded = ef
        .get("F")
        .and_then(Value::as_dict)
        .ok_or(NavigationError::MissingKey("F"))?;

    let mime_type = match embedded.get("Subtype") {
        Some(value) => value
            .as_text()
            .ok_or_else(|| NavigationError::WrongType {
                key: "Subtype".to_string(),
                expected: "a string",
            })?
            .to_string(),
        None => DEFAULT_MIME_TYPE.to_string(),
    };
    let bytes_ref = match embedded.get("XI_StreamRef") {
        Some(value) => value
            .as_text()
            .ok_or_else(|| NavigationError::WrongType {
                key: "XI_StreamRef".to_string(),
                expected: "a string",
            })?
            .to_string(),
        None => String::new(),
    };
    check_length("Subtype", &mime_type)?;
    check_length("XI_StreamRef", &bytes_ref)?;
    validate_name("Names", &name)?;

    Ok(Attachment {
        name,
        file_name,
        description,
        mime_type,
        bytes_ref,
    })
}

/// A flat `/EmbeddedFiles` name tree of attachments, sorted by key.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct NameTree {
    /// Attachments in ascending key order.
    pub entries: Vec<Attachment>,
}

impl NameTree {
    /// Creates an empty name tree.
    pub fn new() -> Self {
        Self::default()
    }

    /// Inserts an attachment, keeping entries sorted by name.
    ///
    /// Returns [`NavigationError::DuplicateName`] when the key already exists.
    pub fn insert(&mut self, attachment: Attachment) -> Result<(), NavigationError> {
        match self
            .entries
            .binary_search_by(|entry| entry.name.as_str().cmp(attachment.name.as_str()))
        {
            Ok(_) => Err(NavigationError::DuplicateName(attachment.name)),
            Err(position) => {
                self.entries.insert(position, attachment);
                Ok(())
            }
        }
    }

    /// Returns the attachment with the given name, if present.
    pub fn get(&self, name: &str) -> Option<&Attachment> {
        self.entries
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .ok()
            .and_then(|position| self.entries.get(position))
    }

    /// Removes and returns the attachment with the given name.
    ///
    /// Returns [`NavigationError::AttachmentNotFound`] when the key is absent.
    /// Entries remain sorted after removal, so serialization stays
    /// deterministic.
    pub fn remove(&mut self, name: &str) -> Result<Attachment, NavigationError> {
        let position = self
            .entries
            .binary_search_by(|entry| entry.name.as_str().cmp(name))
            .map_err(|_| NavigationError::AttachmentNotFound(name.to_string()))?;
        Ok(self.entries.remove(position))
    }

    /// Number of attachments in the tree.
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    /// Returns `true` when the tree has no attachments.
    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}

/// Serializes a name tree into an `/EmbeddedFiles` name-tree dictionary.
///
/// The `/Names` array alternates string keys and file specification values, in
/// ascending key order, so the output is deterministic.
pub fn name_tree_to_dict(tree: &NameTree) -> Dict {
    let mut names = Vec::with_capacity(tree.entries.len() * 2);
    for attachment in &tree.entries {
        names.push(Value::Text(attachment.name.clone()));
        names.push(attachment_to_value(attachment));
    }
    let mut dict = Dict::new();
    dict.insert("Names".to_string(), Value::Array(names));
    dict
}

/// Parses an `/EmbeddedFiles` name-tree dictionary into a [`NameTree`].
///
/// Only the flat `/Names` representation is accepted; a `/Kids` tree must be
/// flattened by the caller first.
pub fn dict_to_name_tree(dict: &Dict) -> Result<NameTree, NavigationError> {
    let values = dict
        .get("Names")
        .and_then(Value::as_array)
        .ok_or(NavigationError::MissingKey("Names"))?;
    if values.len() % 2 != 0 {
        return Err(NavigationError::OddNameTreeEntries);
    }

    let mut tree = NameTree::new();
    for pair in values.as_chunks::<2>().0 {
        let name = pair[0]
            .as_text()
            .ok_or_else(|| NavigationError::WrongType {
                key: "Names".to_string(),
                expected: "a string key",
            })?
            .to_string();
        let attachment = value_to_attachment(&pair[1], name)?;
        tree.insert(attachment)?;
    }
    Ok(tree)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_tree() -> NameTree {
        let mut tree = NameTree::new();
        tree.insert(
            Attachment::new(
                "report.pdf",
                "Q3-report.pdf",
                "Quarterly report",
                "application/pdf",
                "stream-1",
            )
            .expect("attachment"),
        )
        .expect("insert");
        tree.insert(
            Attachment::new(
                "notes.txt",
                "notes.txt",
                String::new(),
                "text/plain",
                "stream-2",
            )
            .expect("attachment"),
        )
        .expect("insert");
        tree
    }

    #[test]
    fn name_tree_round_trip() {
        let tree = sample_tree();
        let dict = name_tree_to_dict(&tree);
        let names = dict.get("Names").and_then(Value::as_array).expect("Names");
        assert_eq!(names.len(), 4);
        assert_eq!(names[0].as_text(), Some("notes.txt"));
        assert_eq!(names[2].as_text(), Some("report.pdf"));
        assert_eq!(dict_to_name_tree(&dict).expect("round-trip"), tree);
    }

    #[test]
    fn names_are_sorted_and_deterministic() {
        let tree = sample_tree();
        let first = name_tree_to_dict(&tree);
        let second = name_tree_to_dict(&tree);
        assert_eq!(first, second);
        let names = first.get("Names").and_then(Value::as_array).expect("Names");
        assert_eq!(names[0].as_text(), Some("notes.txt"));
        assert_eq!(names[2].as_text(), Some("report.pdf"));
    }

    #[test]
    fn attachment_value_round_trip() {
        let attachment = Attachment::new(
            "data",
            "data.bin",
            "Binary data",
            "application/octet-stream",
            "ref-9",
        )
        .expect("attachment");
        let value = attachment_to_value(&attachment);
        assert_eq!(
            value_to_attachment(&value, attachment.name.clone()).expect("round-trip"),
            attachment
        );
    }

    #[test]
    fn name_size_limit_is_enforced() {
        let long = "a".repeat(MAX_NAME_BYTES + 1);
        assert_eq!(
            Attachment::new(long, "f", "", "text/plain", "ref"),
            Err(NavigationError::NameTooLong {
                key: "Names",
                max: MAX_NAME_BYTES,
                got: MAX_NAME_BYTES + 1,
            })
        );
    }

    #[test]
    fn duplicate_name_is_rejected() {
        let mut tree = sample_tree();
        let duplicate = Attachment::new("notes.txt", "other.txt", "", "text/plain", "stream-3")
            .expect("attachment");
        assert!(matches!(
            tree.insert(duplicate),
            Err(NavigationError::DuplicateName(_))
        ));
    }

    #[test]
    fn malformed_names_array_is_rejected() {
        let mut dict = Dict::new();
        dict.insert(
            "Names".to_string(),
            Value::Array(vec![Value::Text("orphan".to_string())]),
        );
        assert!(matches!(
            dict_to_name_tree(&dict),
            Err(NavigationError::OddNameTreeEntries)
        ));
    }

    #[test]
    fn get_returns_registered_attachment() {
        let tree = sample_tree();
        assert_eq!(
            tree.get("report.pdf").map(|a| a.file_name.as_str()),
            Some("Q3-report.pdf")
        );
        assert!(tree.get("missing").is_none());
    }

    #[test]
    fn empty_name_tree_round_trip() {
        let tree = NameTree::new();
        assert!(tree.is_empty());
        let dict = name_tree_to_dict(&tree);
        assert_eq!(
            dict.get("Names")
                .and_then(Value::as_array)
                .map(<[Value]>::len),
            Some(0)
        );
        assert_eq!(dict_to_name_tree(&dict).expect("empty tree"), tree);
    }
}
