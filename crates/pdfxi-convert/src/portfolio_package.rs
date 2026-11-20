//! Real, byte-stable PDF package assembly for CREATE-010 (ADR 0008).
//!
//! `crate::portfolio` validates a manifest and serializes an XML descriptor,
//! and `crate::portfolio_builder` customizes that manifest. Neither produces a
//! PDF package. This module closes that library-level gap: it turns a validated
//! [`PortfolioManifest`] plus caller-supplied file bytes into a typed
//! [`PortfolioPackage`] object graph and a deterministic classic-xref PDF
//! serializer.
//!
//! # What is assembled
//!
//! - one embedded-file stream and one `/Filespec` per manifest entry, built
//!   from bytes the caller supplies (never read from the file system);
//! - a `/Names /EmbeddedFiles` name tree whose keys are sorted by UTF-8 byte
//!   order, independent of the caller's insertion or `order` values;
//! - a `/Collection` dictionary carrying the manifest title and a declared
//!   schema for `Name`, `Description`, and `Order`;
//! - a catalog that wires `/Pages`, `/Names`, and `/Collection` deterministically,
//!   plus a single blank page so the result is a normal one-page PDF.
//!
//! # Determinism and safety
//!
//! Object numbers, dictionary key order, the xref table, and the trailer are
//! fixed by construction. No timestamps, random IDs, environment values, or
//! file-system access enter the output, so equivalent inputs serialize to
//! byte-identical PDFs. Sizes are bounded and every failure is a typed
//! [`PortfolioPackageError`]. The writer is a small local classic-xref writer
//! (the same shape as `crate::create`'s); `pdfxi-core` is used only by tests to
//! parse the result. No validator is invoked and no conformance is claimed.

use crate::portfolio::{PortfolioEntry, PortfolioError, PortfolioManifest};
use std::collections::{BTreeMap, HashSet};

/// PDF version written by this assembler.
pub const PACKAGE_PDF_VERSION: &str = "1.7";
/// Maximum bytes accepted for one embedded file (64 MiB).
pub const PACKAGE_MAX_FILE_BYTES: usize = 64 * 1024 * 1024;
/// Maximum total embedded bytes across one package (256 MiB).
pub const PACKAGE_MAX_TOTAL_BYTES: usize = 256 * 1024 * 1024;

/// A typed failure while assembling a PDF portfolio package.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum PortfolioPackageError {
    /// The supplied manifest failed its own validation.
    #[error(transparent)]
    Manifest(#[from] PortfolioError),
    /// A name was supplied twice, ignoring ASCII case.
    #[error("duplicate embedded file name '{name}'")]
    DuplicateName {
        /// The duplicated name as first supplied.
        name: String,
    },
    /// A manifest entry has no caller-supplied bytes.
    #[error("embedded file '{name}' is missing caller-supplied bytes")]
    MissingFile {
        /// The manifest entry without bytes.
        name: String,
    },
    /// Bytes were supplied for a name absent from the manifest.
    #[error("embedded file '{name}' is not declared by the manifest")]
    UnknownFile {
        /// The undeclared name.
        name: String,
    },
    /// One embedded file exceeds the documented byte budget.
    #[error("embedded file '{name}' is {bytes} bytes (limit {limit})")]
    FileTooLarge {
        /// The offending file name.
        name: String,
        /// Its supplied size.
        bytes: usize,
        /// The enforced per-file limit.
        limit: usize,
    },
    /// The combined embedded bytes exceed the documented budget.
    #[error("embedded files total {bytes} bytes (limit {limit})")]
    TotalTooLarge {
        /// The combined supplied size.
        bytes: usize,
        /// The enforced total limit.
        limit: usize,
    },
    /// Object numbering overflowed the PDF `u32` space.
    #[error("portfolio object number space is exhausted")]
    ObjectNumberOverflow,
}

/// A PDF object value used by the package object graph.
///
/// Dictionaries keep caller-chosen key order so serialization is deterministic;
/// streams carry their exact length and raw bytes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PdfValue {
    /// The PDF `null` object.
    Null,
    /// A boolean object.
    Boolean(bool),
    /// A signed integer object.
    Integer(i64),
    /// A PDF name (serialized with `#XX` escapes where required).
    Name(String),
    /// A PDF literal string (serialized with balanced escapes).
    Literal(String),
    /// An indirect reference to an object number.
    Reference(u32),
    /// An ordered array of values.
    Array(Vec<PdfValue>),
    /// An ordered dictionary of name/value pairs.
    Dictionary(Vec<(String, PdfValue)>),
    /// A stream with a dictionary and exact raw data.
    Stream {
        /// Dictionary entries written before the stream keyword.
        entries: Vec<(String, PdfValue)>,
        /// Raw stream bytes (written verbatim between `stream` and `endstream`).
        data: Vec<u8>,
    },
}

/// One numbered indirect object in a [`PortfolioPackage`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PdfObject {
    /// One-based PDF object number.
    pub number: u32,
    /// Value stored in this object.
    pub value: PdfValue,
}

/// A validated, caller-visible description of one embedded file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PackageFile {
    /// Relative name used as the name-tree key.
    pub name: String,
    /// Description copied from the manifest entry.
    pub description: String,
    /// Sort key copied from the manifest entry.
    pub order: usize,
    /// Number of embedded bytes.
    pub size: usize,
    /// Object number of this file's `/Filespec`.
    pub filespec_number: u32,
    /// Object number of this file's embedded-file stream.
    pub stream_number: u32,
}

/// A serializable PDF portfolio package object graph.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PortfolioPackage {
    title: String,
    catalog_number: u32,
    objects: Vec<PdfObject>,
    files: Vec<PackageFile>,
}

impl PortfolioPackage {
    /// Return the manifest title carried in the `/Collection /D` entry.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.title
    }

    /// Return the catalog object number used by the trailer `/Root`.
    #[must_use]
    pub const fn catalog_number(&self) -> u32 {
        self.catalog_number
    }

    /// Return every object in ascending number order.
    #[must_use]
    pub fn objects(&self) -> &[PdfObject] {
        &self.objects
    }

    /// Return the embedded files in ascending name-tree order.
    #[must_use]
    pub fn files(&self) -> &[PackageFile] {
        &self.files
    }

    /// Find an object by number, or `None` when it is not part of the graph.
    #[must_use]
    pub fn object(&self, number: u32) -> Option<&PdfObject> {
        self.objects.iter().find(|object| object.number == number)
    }

    /// Return the name-tree keys in serialization order.
    #[must_use]
    pub fn name_tree_names(&self) -> Vec<&str> {
        self.files.iter().map(|file| file.name.as_str()).collect()
    }

    /// Return the number of embedded files.
    #[must_use]
    pub fn file_count(&self) -> usize {
        self.files.len()
    }

    /// Serialize this package to a deterministic classic-xref PDF.
    #[must_use]
    pub fn to_bytes(&self) -> Vec<u8> {
        serialize_portfolio_package(self)
    }
}

/// Mutable assembler that pairs a manifest with caller-supplied file bytes.
#[derive(Debug, Clone)]
pub struct PortfolioPackageBuilder {
    manifest: PortfolioManifest,
    files: BTreeMap<String, Vec<u8>>,
}

impl PortfolioPackageBuilder {
    /// Start assembling a package from a manifest.
    #[must_use]
    pub fn new(manifest: PortfolioManifest) -> Self {
        Self {
            manifest,
            files: BTreeMap::new(),
        }
    }

    /// Supply the bytes for one named entry.
    ///
    /// The name must exactly match a manifest entry. Names are ordered by their
    /// UTF-8 bytes when the package is serialized, regardless of insertion order.
    pub fn add_file(&mut self, name: impl Into<String>, bytes: Vec<u8>) -> &mut Self {
        let _ = self.files.insert(name.into(), bytes);
        self
    }

    /// Validate and assemble the typed object graph.
    ///
    /// # Errors
    ///
    /// Returns a typed [`PortfolioPackageError`] for an invalid manifest, a
    /// missing or undeclared file, a duplicate name, an oversized file, or an
    /// exhausted object number space.
    pub fn build(self) -> Result<PortfolioPackage, PortfolioPackageError> {
        self.manifest.validate()?;

        let mut seen: HashSet<String> = HashSet::with_capacity(self.manifest.entries().len());
        for entry in self.manifest.entries() {
            let key = entry.name().to_ascii_lowercase();
            if !seen.insert(key) {
                return Err(PortfolioPackageError::DuplicateName {
                    name: entry.name().to_owned(),
                });
            }
        }

        let mut declared: HashSet<&str> = HashSet::with_capacity(self.files.len());
        for name in self.files.keys() {
            if !self
                .manifest
                .entries()
                .iter()
                .any(|entry| entry.name() == name)
            {
                return Err(PortfolioPackageError::UnknownFile { name: name.clone() });
            }
            let _ = declared.insert(name.as_str());
        }

        let mut entries: Vec<&PortfolioEntry> = self.manifest.entries().iter().collect();
        entries.sort_by(|left, right| left.name().as_bytes().cmp(right.name().as_bytes()));
        let mut total = 0usize;
        for entry in &entries {
            let bytes =
                self.files
                    .get(entry.name())
                    .ok_or_else(|| PortfolioPackageError::MissingFile {
                        name: entry.name().to_owned(),
                    })?;
            if bytes.len() > PACKAGE_MAX_FILE_BYTES {
                return Err(PortfolioPackageError::FileTooLarge {
                    name: entry.name().to_owned(),
                    bytes: bytes.len(),
                    limit: PACKAGE_MAX_FILE_BYTES,
                });
            }
            total = total
                .checked_add(bytes.len())
                .ok_or(PortfolioPackageError::ObjectNumberOverflow)?;
            if total > PACKAGE_MAX_TOTAL_BYTES {
                return Err(PortfolioPackageError::TotalTooLarge {
                    bytes: total,
                    limit: PACKAGE_MAX_TOTAL_BYTES,
                });
            }
        }
        debug_assert_eq!(declared.len(), self.files.len());

        let file_count = entries.len();
        let object_count = 5usize
            .checked_add(
                file_count
                    .checked_mul(2)
                    .ok_or(PortfolioPackageError::ObjectNumberOverflow)?,
            )
            .ok_or(PortfolioPackageError::ObjectNumberOverflow)?;
        if object_count > u32::MAX as usize {
            return Err(PortfolioPackageError::ObjectNumberOverflow);
        }

        let mut objects = Vec::with_capacity(object_count);
        objects.push(PdfObject {
            number: 1,
            value: PdfValue::Dictionary(vec![
                ("Type".to_owned(), PdfValue::Name("Catalog".to_owned())),
                ("Pages".to_owned(), PdfValue::Reference(2)),
                (
                    "Names".to_owned(),
                    PdfValue::Dictionary(vec![(
                        "EmbeddedFiles".to_owned(),
                        PdfValue::Reference(4),
                    )]),
                ),
                ("Collection".to_owned(), PdfValue::Reference(5)),
            ]),
        });
        objects.push(PdfObject {
            number: 2,
            value: PdfValue::Dictionary(vec![
                ("Type".to_owned(), PdfValue::Name("Pages".to_owned())),
                (
                    "Kids".to_owned(),
                    PdfValue::Array(vec![PdfValue::Reference(3)]),
                ),
                ("Count".to_owned(), PdfValue::Integer(1)),
            ]),
        });
        objects.push(PdfObject {
            number: 3,
            value: PdfValue::Dictionary(vec![
                ("Type".to_owned(), PdfValue::Name("Page".to_owned())),
                ("Parent".to_owned(), PdfValue::Reference(2)),
                (
                    "MediaBox".to_owned(),
                    PdfValue::Array(vec![
                        PdfValue::Integer(0),
                        PdfValue::Integer(0),
                        PdfValue::Integer(612),
                        PdfValue::Integer(792),
                    ]),
                ),
                ("Resources".to_owned(), PdfValue::Dictionary(Vec::new())),
            ]),
        });

        let mut names: Vec<PdfValue> = Vec::with_capacity(file_count * 2);
        let mut files: Vec<PackageFile> = Vec::with_capacity(file_count);
        for (index, entry) in entries.iter().enumerate() {
            let filespec_number = 6u32
                .checked_add(
                    (index as u32)
                        .checked_mul(2)
                        .ok_or(PortfolioPackageError::ObjectNumberOverflow)?,
                )
                .ok_or(PortfolioPackageError::ObjectNumberOverflow)?;
            let stream_number = filespec_number
                .checked_add(1)
                .ok_or(PortfolioPackageError::ObjectNumberOverflow)?;
            let bytes = match self.files.get(entry.name()) {
                Some(bytes) => bytes,
                None => {
                    return Err(PortfolioPackageError::MissingFile {
                        name: entry.name().to_owned(),
                    });
                }
            };
            names.push(PdfValue::Literal(entry.name().to_owned()));
            names.push(PdfValue::Reference(filespec_number));
            files.push(PackageFile {
                name: entry.name().to_owned(),
                description: entry.description().to_owned(),
                order: entry.order(),
                size: bytes.len(),
                filespec_number,
                stream_number,
            });
        }

        objects.push(PdfObject {
            number: 4,
            value: PdfValue::Dictionary(vec![("Names".to_owned(), PdfValue::Array(names))]),
        });
        objects.push(PdfObject {
            number: 5,
            value: PdfValue::Dictionary(vec![
                ("Type".to_owned(), PdfValue::Name("Collection".to_owned())),
                (
                    "D".to_owned(),
                    PdfValue::Literal(self.manifest.title().to_owned()),
                ),
                ("View".to_owned(), PdfValue::Name("D".to_owned())),
                (
                    "Schema".to_owned(),
                    PdfValue::Dictionary(vec![
                        (
                            "Type".to_owned(),
                            PdfValue::Name("CollectionSchema".to_owned()),
                        ),
                        (
                            "Fields".to_owned(),
                            PdfValue::Array(vec![
                                PdfValue::Literal("Name".to_owned()),
                                PdfValue::Literal("Description".to_owned()),
                                PdfValue::Literal("Order".to_owned()),
                            ]),
                        ),
                    ]),
                ),
            ]),
        });

        for file in &files {
            let bytes = match self.files.get(&file.name) {
                Some(bytes) => bytes,
                None => {
                    return Err(PortfolioPackageError::MissingFile {
                        name: file.name.clone(),
                    });
                }
            };
            objects.push(PdfObject {
                number: file.filespec_number,
                value: PdfValue::Dictionary(vec![
                    ("Type".to_owned(), PdfValue::Name("Filespec".to_owned())),
                    ("F".to_owned(), PdfValue::Literal(file.name.clone())),
                    ("UF".to_owned(), PdfValue::Literal(file.name.clone())),
                    (
                        "Desc".to_owned(),
                        PdfValue::Literal(file.description.clone()),
                    ),
                    (
                        "EF".to_owned(),
                        PdfValue::Dictionary(vec![(
                            "F".to_owned(),
                            PdfValue::Reference(file.stream_number),
                        )]),
                    ),
                ]),
            });
            let size = file.size as i64;
            objects.push(PdfObject {
                number: file.stream_number,
                value: PdfValue::Stream {
                    entries: vec![
                        ("Type".to_owned(), PdfValue::Name("EmbeddedFile".to_owned())),
                        ("Length".to_owned(), PdfValue::Integer(size)),
                        (
                            "Params".to_owned(),
                            PdfValue::Dictionary(vec![(
                                "Size".to_owned(),
                                PdfValue::Integer(size),
                            )]),
                        ),
                    ],
                    data: bytes.clone(),
                },
            });
        }

        objects.sort_by_key(|object| object.number);
        Ok(PortfolioPackage {
            title: self.manifest.title().to_owned(),
            catalog_number: 1,
            objects,
            files,
        })
    }
}

/// Assemble a package from a validated manifest and caller-supplied bytes.
///
/// # Errors
///
/// Propagates every [`PortfolioPackageError`] from
/// [`PortfolioPackageBuilder::build`].
pub fn build_portfolio_package(
    manifest: &PortfolioManifest,
    files: &[(&str, Vec<u8>)],
) -> Result<PortfolioPackage, PortfolioPackageError> {
    let mut builder = PortfolioPackageBuilder::new(manifest.clone());
    for (name, bytes) in files {
        let _ = builder.add_file((*name).to_owned(), bytes.clone());
    }
    builder.build()
}

/// Escape a PDF name so the serialized token is well formed.
fn escape_name(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    for byte in name.as_bytes() {
        let delim = matches!(
            *byte,
            b'(' | b')' | b'<' | b'>' | b'[' | b']' | b'{' | b'}' | b'/' | b'%' | b'#' | b'\\'
        );
        if !(*byte >= 0x21 && *byte <= 0x7e) || delim {
            out.push_str(&format!("#{byte:02X}"));
        } else {
            out.push(*byte as char);
        }
    }
    out
}

/// Escape a string as a PDF literal string.
fn escape_literal(text: &str) -> Vec<u8> {
    let mut out = Vec::with_capacity(text.len());
    out.push(b'(');
    for byte in text.as_bytes() {
        match *byte {
            b'\\' | b'(' | b')' => {
                out.push(b'\\');
                out.push(*byte);
            }
            0x00..=0x1f | 0x7f => out.extend_from_slice(format!("\\{byte:03o}").as_bytes()),
            _ => out.push(*byte),
        }
    }
    out.push(b')');
    out
}

/// Serialize one value into `out` with fixed, deterministic key ordering.
fn write_value(value: &PdfValue, out: &mut Vec<u8>) {
    match value {
        PdfValue::Null => out.extend_from_slice(b"null"),
        PdfValue::Boolean(true) => out.extend_from_slice(b"true"),
        PdfValue::Boolean(false) => out.extend_from_slice(b"false"),
        PdfValue::Integer(number) => out.extend_from_slice(number.to_string().as_bytes()),
        PdfValue::Name(name) => {
            out.push(b'/');
            out.extend_from_slice(escape_name(name).as_bytes());
        }
        PdfValue::Literal(text) => out.extend_from_slice(&escape_literal(text)),
        PdfValue::Reference(number) => {
            out.extend_from_slice(format!("{number} 0 R").as_bytes());
        }
        PdfValue::Array(values) => {
            out.push(b'[');
            for value in values {
                out.push(b' ');
                write_value(value, out);
            }
            out.extend_from_slice(b" ]");
        }
        PdfValue::Dictionary(entries) => {
            out.extend_from_slice(b"<<");
            for (key, value) in entries {
                out.push(b' ');
                out.push(b'/');
                out.extend_from_slice(escape_name(key).as_bytes());
                out.push(b' ');
                write_value(value, out);
            }
            out.extend_from_slice(b" >>");
        }
        PdfValue::Stream { entries, data } => {
            write_value(&PdfValue::Dictionary(entries.clone()), out);
            out.extend_from_slice(b"\nstream\n");
            out.extend_from_slice(data);
            out.extend_from_slice(b"\nendstream");
        }
    }
}

/// Serialize a package to a byte-identical classic-xref PDF 1.7 document.
///
/// The header, binary comment, object order, xref widths, and trailer are fixed.
/// No timestamps, IDs, environment values, or file reads influence the result.
#[must_use]
pub fn serialize_portfolio_package(package: &PortfolioPackage) -> Vec<u8> {
    let mut out = format!("%PDF-{PACKAGE_PDF_VERSION}\n%").into_bytes();
    out.extend_from_slice(&[0xE2, 0xE3, 0xCF, 0xD3, b'\n']);
    let mut offsets = Vec::with_capacity(package.objects.len());
    for object in &package.objects {
        offsets.push(out.len());
        out.extend_from_slice(format!("{} 0 obj\n", object.number).as_bytes());
        write_value(&object.value, &mut out);
        out.extend_from_slice(b"\nendobj\n");
    }
    let xref_offset = out.len();
    let size = package
        .objects
        .last()
        .map_or(1, |object| object.number.saturating_add(1));
    out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
    out.extend_from_slice(b"0000000000 65535 f \n");
    for offset in &offsets {
        out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
    }
    out.extend_from_slice(
        format!(
            "trailer\n<< /Size {size} /Root {} 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            package.catalog_number
        )
        .as_bytes(),
    );
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::portfolio::PortfolioEntry;

    fn manifest(entries: Vec<PortfolioEntry>) -> PortfolioManifest {
        PortfolioManifest::new("Evidence", entries)
    }

    fn package(entries: Vec<(&str, &[u8])>) -> PortfolioPackage {
        let mut builder = PortfolioPackageBuilder::new(manifest(
            entries
                .iter()
                .enumerate()
                .map(|(index, (name, _))| PortfolioEntry::new(*name, "desc", index))
                .collect(),
        ));
        for (name, bytes) in entries {
            let _ = builder.add_file(name, bytes.to_vec());
        }
        builder
            .build()
            .unwrap_or_else(|error| panic!("build: {error}"))
    }

    #[test]
    fn package_emits_catalog_names_tree_collection_and_empty_page() {
        let built = package(vec![("b.txt", b"BB"), ("a.txt", b"A")]);
        let text = String::from_utf8_lossy(&built.to_bytes()).into_owned();
        assert!(text.starts_with("%PDF-1.7\n"));
        assert!(text.contains("/Type /Catalog"));
        assert!(text.contains("/Names << /EmbeddedFiles 4 0 R >>"));
        assert!(text.contains("/Collection 5 0 R"));
        assert!(text.contains("/Type /Collection /D (Evidence)"));
        assert!(text.contains("/Type /EmbeddedFile /Length 2"));
        assert!(text.contains("stream\nBB\nendstream"));
        assert!(text.contains("/Type /Page /Parent 2 0 R"));
        assert_eq!(built.name_tree_names(), vec!["a.txt", "b.txt"]);
        assert_eq!(built.catalog_number(), 1);
        assert_eq!(built.file_count(), 2);
    }

    #[test]
    fn name_tree_order_is_byte_stable_regardless_of_insertion_order() {
        let first = package(vec![("z.pdf", b"z"), ("a.pdf", b"a"), ("m.pdf", b"m")]);
        let second = package(vec![("m.pdf", b"m"), ("z.pdf", b"z"), ("a.pdf", b"a")]);
        assert_eq!(first.name_tree_names(), vec!["a.pdf", "m.pdf", "z.pdf"]);
        assert_eq!(first.to_bytes(), second.to_bytes());
        let text = String::from_utf8_lossy(&first.to_bytes()).into_owned();
        let a = text.find("(a.pdf) 6 0 R").expect("first key");
        let m = text.find("(m.pdf) 8 0 R").expect("second key");
        let z = text.find("(z.pdf) 10 0 R").expect("third key");
        assert!(a < m && m < z);
    }

    #[test]
    fn missing_declared_file_is_a_typed_error() {
        let mut builder =
            PortfolioPackageBuilder::new(manifest(vec![PortfolioEntry::new("needed.txt", "", 0)]));
        let _ = builder.add_file("other.txt", b"x".to_vec());
        let error = builder.build().expect_err("missing file must fail");
        assert!(matches!(
            error,
            PortfolioPackageError::UnknownFile { .. } | PortfolioPackageError::MissingFile { .. }
        ));
        let empty =
            PortfolioPackageBuilder::new(manifest(vec![PortfolioEntry::new("needed.txt", "", 0)]))
                .build();
        assert_eq!(
            empty.err(),
            Some(PortfolioPackageError::MissingFile {
                name: "needed.txt".to_owned()
            })
        );
    }

    #[test]
    fn duplicate_names_are_rejected_before_any_serialization() {
        let mut builder = PortfolioPackageBuilder::new(manifest(vec![
            PortfolioEntry::new("A.txt", "", 0),
            PortfolioEntry::new("a.TXT", "", 1),
        ]));
        let _ = builder.add_file("A.txt", b"a".to_vec());
        let _ = builder.add_file("a.TXT", b"b".to_vec());
        assert!(matches!(
            builder.build(),
            Err(PortfolioPackageError::Manifest(_))
                | Err(PortfolioPackageError::DuplicateName { .. })
        ));
    }

    #[test]
    fn empty_portfolio_is_a_valid_package_with_an_empty_name_tree() {
        let built = PortfolioPackageBuilder::new(manifest(Vec::new()))
            .build()
            .expect("empty package");
        assert_eq!(built.file_count(), 0);
        assert!(built.name_tree_names().is_empty());
        let text = String::from_utf8_lossy(&built.to_bytes()).into_owned();
        assert!(text.contains("/Names [ ] >>"));
        assert!(text.contains("/Size 6"));
        assert!(!text.contains("/Type /Filespec"));
    }

    #[test]
    fn oversized_files_and_totals_are_typed_errors() {
        let mut too_large =
            PortfolioPackageBuilder::new(manifest(vec![PortfolioEntry::new("big.bin", "", 0)]));
        let _ = too_large.add_file("big.bin", vec![0u8; PACKAGE_MAX_FILE_BYTES + 1]);
        assert!(matches!(
            too_large.build(),
            Err(PortfolioPackageError::FileTooLarge { .. })
        ));

        let entries: Vec<PortfolioEntry> = (0..5)
            .map(|index| PortfolioEntry::new(format!("f{index}.bin"), "", index))
            .collect();
        let mut too_much = PortfolioPackageBuilder::new(manifest(entries));
        for index in 0..5 {
            let _ = too_much.add_file(
                format!("f{index}.bin"),
                vec![0u8; PACKAGE_MAX_TOTAL_BYTES / 4],
            );
        }
        assert!(matches!(
            too_much.build(),
            Err(PortfolioPackageError::TotalTooLarge { .. })
        ));
    }

    #[test]
    fn package_bytes_are_deterministic_and_free_of_timestamps() {
        let build = || {
            package(vec![
                ("report.pdf", b"%PDF-1.4\n%%EOF\n"),
                ("assets/data.bin", &[0u8, 1, 2, 255]),
            ])
            .to_bytes()
        };
        let first = build();
        assert_eq!(first, build());
        let text = String::from_utf8_lossy(&first);
        assert!(!text.contains("/CreationDate"));
        assert!(!text.contains("/ModDate"));
        assert!(!text.contains("/ID"));
        assert!(text.ends_with("%%EOF\n"));
    }

    #[test]
    fn literal_and_name_escaping_preserves_hostile_text() {
        let mut out = Vec::new();
        write_value(
            &PdfValue::Literal("a\\b (c) d\u{0001}".to_owned()),
            &mut out,
        );
        let escaped = String::from_utf8(out).expect("ASCII escapes");
        assert_eq!(escaped, "(a\\\\b \\(c\\) d\\001)");
        assert_eq!(escape_name("a/b#c"), "a#2Fb#23c");
    }

    #[test]
    fn object_numbers_are_contiguous_and_streams_are_exact() {
        let built = package(vec![("one", b"1"), ("two", b"22"), ("three", b"333")]);
        let numbers: Vec<u32> = built.objects().iter().map(|object| object.number).collect();
        assert_eq!(numbers, vec![1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]);
        for file in built.files() {
            let stream = built.object(file.stream_number).expect("stream object");
            match &stream.value {
                PdfValue::Stream { data, .. } => assert_eq!(data.len(), file.size),
                other => panic!("expected a stream, got {other:?}"),
            }
        }
    }

    #[test]
    fn convenience_builder_matches_the_step_builder() {
        let manifest = manifest(vec![PortfolioEntry::new("x.txt", "X", 0)]);
        let mut step_builder = PortfolioPackageBuilder::new(manifest.clone());
        let _ = step_builder.add_file("x.txt", b"payload".to_vec());
        let expected = step_builder.build().expect("step build");
        let actual = build_portfolio_package(&manifest, &[("x.txt", b"payload".to_vec())])
            .expect("convenience build");
        assert_eq!(actual, expected);
        assert_eq!(actual.to_bytes(), expected.to_bytes());
    }
}
