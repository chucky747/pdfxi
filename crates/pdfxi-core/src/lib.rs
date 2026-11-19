//! Core PDF object model, parsing, and document lifecycle for PDFXI.
//!
//! This crate is deliberately free of UI and rendering dependencies. It owns the
//! typed representation of a PDF document, the cross-reference table, stream
//! filters, encryption hooks, and incremental save. Rendering lives in
//! `pdfxi-render`; editing semantics live in `pdfxi-edit`.
//!
//! [`Document::from_bytes`] validates the file header immediately and parses
//! the structural layer (classic xref table or PDF 1.5+ xref stream, trailer,
//! catalog and page tree, including objects held in `/ObjStm` streams)
//! eagerly. Structural errors are retained on the handle and surfaced by the
//! page accessors, so callers can still inspect the version of a file whose
//! body is damaged.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod boxes;
pub mod compatibility;
pub mod content;
pub mod filter;
pub mod linearization;
pub mod linearize;
pub mod metadata;
pub mod object;
pub mod objstm;
pub mod optimizer;
pub mod page;
pub mod pages;
pub mod printer_marks;
pub mod repair;

mod lexer;
mod parser;
mod writer;

#[cfg(test)]
mod xref_tests;

pub use boxes::{BoxKind, PageBoxes};
pub use compatibility::{
    CompatibilityReport, CompatibilityWarning, HEADER_SCAN_LIMIT, PdfVersion, VersionSource,
    analyze_compatibility,
};
pub use content::extract_text;
pub use filter::{
    DEFAULT_DECODE_CAP, Filter, FlateParams, Predictor, decode_stream, decode_stream_with_limit,
};
pub use linearization::{HintStreamRange, Linearization, parse_linearization};
pub use linearize::{
    HintStream, LinearizationOptions, LinearizedLayout, PageHint, SharedObjectHint,
    parse_hint_stream, write_linearized_document, write_linearized_document_with_layout,
};
pub use metadata::{
    DocumentInfo, PdfDate, XmpMetadata, decode_text_string, encode_text_string, parse_pdf_date,
    parse_xmp,
};
pub use object::{Dictionary, Object, ObjectId};
pub use objstm::{
    ObjectStreamOptions, ObjectStreamPlan, ObjectStreamReport, PackedObjectRef, PackingExclusion,
    is_packing_eligible, packing_exclusion_reason, write_object_stream_document,
    write_object_stream_increment,
};
pub use optimizer::{CancellationToken, OptimizationReport, OptimizationResult, OptimizeOptions};
pub use page::{Page, Rectangle};
pub use pages::{PageOperation, PagePlan, RotationTarget};
pub use printer_marks::{
    MarkCircle, MarkGeometry, MarkSegment, PrinterMarkOptions, PrinterMarks, generate_printer_marks,
};
pub use repair::{RepairError, RepairReport, RepairWarning, RepairedDocument, repair_document};

use std::collections::BTreeMap;

use parser::Structure;

/// Errors returned by the core crate.
#[derive(Debug, Clone, thiserror::Error)]
pub enum Error {
    /// The input does not start with a supported PDF header.
    #[error("invalid PDF header; expected %PDF-")]
    InvalidHeader,
    /// No `startxref` marker was found in the file.
    #[error("missing startxref marker")]
    MissingStartXref,
    /// The value following `startxref` was not a valid byte offset.
    #[error("invalid startxref value: {0}")]
    InvalidStartXref(String),
    /// The `startxref` value lies outside the file.
    #[error("startxref offset {offset} is outside the file ({len} bytes)")]
    StartXrefOutOfBounds {
        /// The offset read from `startxref`.
        offset: usize,
        /// The total length of the input.
        len: usize,
    },
    /// No classic `xref` table exists at the given offset.
    #[error("no classic xref table at byte offset {0}")]
    MissingXref(usize),
    /// The classic xref table was structurally malformed.
    #[error("malformed xref table: {0}")]
    MalformedXref(String),
    /// A cross-reference stream (`/Type /XRef`) was structurally malformed.
    #[error("malformed xref stream: {0}")]
    MalformedXrefStream(String),
    /// An object stream (`/Type /ObjStm`) was structurally malformed.
    #[error("malformed object stream: {0}")]
    MalformedObjectStream(String),
    /// The linearization hint dictionary was present but invalid.
    #[error("malformed linearization hint: {0}")]
    MalformedLinearization(String),
    /// A linearization hint stream was malformed.
    #[error("malformed linearization hint stream: {0}")]
    MalformedHintStream(String),
    /// A fully linearized document could not be produced.
    #[error("linearization failed: {0}")]
    LinearizationFailed(String),
    /// The chain of `/Prev` xref sections contains a cycle.
    #[error("xref /Prev cycle detected at byte offset {0}")]
    XrefCycle(usize),
    /// The xref table has no trailer dictionary.
    #[error("missing trailer dictionary")]
    MissingTrailer,
    /// The trailer has no valid `/Root` indirect reference.
    #[error("trailer /Root is missing or not an indirect reference")]
    MissingRoot,
    /// The catalog has no valid `/Pages` indirect reference.
    #[error("catalog /Pages is missing or not an indirect reference")]
    MissingPages,
    /// An object number is absent from the xref table.
    #[error("object {0} is not present in the xref table")]
    ObjectNotFound(u32),
    /// An object number is present but marked free.
    #[error("object {0} is free in the xref table")]
    FreeObject(u32),
    /// An indirect object body did not match its xref entry.
    #[error("malformed object {number}: {reason}")]
    MalformedObject {
        /// The object number from the xref table.
        number: u32,
        /// Why the object body was rejected.
        reason: String,
    },
    /// The `/Pages` tree could not be resolved.
    #[error("malformed page tree: {0}")]
    MalformedPageTree(String),
    /// A `/MediaBox` entry was present but invalid.
    #[error("invalid /MediaBox on page object {0}")]
    InvalidMediaBox(u32),
    /// A page box array was missing, too short, or held a non-numeric value.
    #[error("invalid /{name} array on page object {page}")]
    InvalidBoxArray {
        /// The object number of the page whose box was rejected.
        page: u32,
        /// The dictionary key of the rejected box, without its slash.
        name: &'static str,
    },
    /// A page box had a coordinate that was not a finite number.
    #[error("page box /{name} has a non-finite coordinate")]
    NonFiniteBoxCoordinate {
        /// The dictionary key of the rejected box, without its slash.
        name: &'static str,
    },
    /// A page box had zero or negative width or height.
    #[error("page box /{name} is degenerate")]
    DegenerateBox {
        /// The dictionary key of the rejected box, without its slash.
        name: &'static str,
    },
    /// A page box lay outside the box that must contain it.
    #[error("page box /{inner} is not contained within /{outer}")]
    BoxNotContained {
        /// The dictionary key of the box that was outside.
        inner: &'static str,
        /// The dictionary key of the box that should contain it.
        outer: &'static str,
    },
    /// A printer-mark option was non-finite or outside its allowed range.
    #[error("invalid printer-mark option: {0}")]
    InvalidPrinterMarkOptions(&'static str),
    /// Printer marks cannot fit between the TrimBox and the MediaBox.
    #[error(
        "printer marks exceed the sheet: {what} requires {required} pt of trim margin but only {available} pt is available"
    )]
    PrinterMarksOutOfSheet {
        /// The mark whose placement does not fit (for example `page information`).
        what: &'static str,
        /// Minimum trim margin required, in default user space units.
        required: f64,
        /// Trim margin actually available, in default user space units.
        available: f64,
    },
    /// The requested page index does not exist.
    #[error("page index {0} is out of range")]
    PageNotFound(usize),
    /// A page operation was given an empty selection.
    #[error("page operation requires at least one page")]
    EmptyPageSelection,
    /// A page index supplied to a page plan is outside the valid range.
    #[error("page index {index} is out of range for {context} (length {len})")]
    InvalidPageIndex {
        /// The rejected index.
        index: usize,
        /// The number of valid positions.
        len: usize,
        /// A human-readable description of what was being indexed.
        context: &'static str,
    },
    /// An order plan contained the same page position more than once.
    #[error("duplicate page index {0} in page order")]
    DuplicatePageIndex(usize),
    /// A merge referenced a source document that is not part of the plan.
    #[error("source document index {index} is out of range (sources: {len})")]
    InvalidSourceDocument {
        /// The rejected document index.
        index: usize,
        /// The number of source documents.
        len: usize,
    },
    /// A `/Rotate` entry was not a multiple of 90 degrees.
    #[error("page rotation {0} is not a multiple of 90 degrees")]
    InvalidPageRotation(i64),
    /// The input ended before a value was complete.
    #[error("unexpected end of PDF data")]
    UnexpectedEof,
    /// A token was encountered where the grammar did not allow it.
    #[error("unexpected token while parsing: {0}")]
    UnexpectedToken(String),
    /// A nesting or container-size limit was exceeded.
    #[error("PDF nesting limit ({0}) exceeded")]
    NestingTooDeep(usize),
    /// A page content stream uses a filter that this build cannot decode.
    #[error(
        "content stream {id} uses unsupported filter {filter}; text extraction cannot decode it"
    )]
    FilteredContent {
        /// The content stream object that could not be decoded.
        id: ObjectId,
        /// The PDF name of the unsupported filter.
        filter: String,
    },
    /// A stream filter is recognized but not implemented in this build.
    #[error("unsupported stream filter: {0}")]
    UnsupportedFilter(String),
    /// The zlib wrapper of a Flate stream is invalid.
    #[error("invalid zlib header in Flate stream: {0}")]
    InvalidZlibHeader(String),
    /// A DEFLATE payload is truncated or violates RFC 1951.
    #[error("malformed DEFLATE stream: {0}")]
    MalformedDeflate(String),
    /// A decoded stream would exceed the configured output cap.
    #[error("decoded stream exceeds the {limit} byte output cap")]
    DecodeLimitExceeded {
        /// The output cap that would be exceeded.
        limit: usize,
    },
    /// Decode parameters such as `/Predictor` are invalid or unsupported.
    #[error("invalid stream decode parameters: {0}")]
    InvalidDecodeParameters(String),
    /// A `/Contents` entry did not resolve to a stream object.
    #[error("content object {0} is not a stream")]
    ContentNotStream(ObjectId),
    /// The requested feature is not implemented in this build.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
    /// The document uses a feature this build cannot save.
    #[error("unsupported document state: {0}")]
    UnsupportedState(&'static str),
    /// An object identifier cannot be used for an in-use object.
    #[error("invalid object id: {0}")]
    InvalidObjectId(String),
    /// An object value could not be serialized to PDF syntax.
    #[error("cannot serialize object: {0}")]
    CannotSerialize(String),
    /// An XMP packet was malformed or used an unsupported XML construct.
    #[error("malformed XMP packet: {0}")]
    MalformedXmp(String),
    /// A document metadata entry did not have the expected object type.
    #[error("invalid document metadata: {0}")]
    InvalidMetadata(String),
    /// A long-running operation was cancelled by the caller.
    #[error("operation cancelled")]
    Cancelled,
    /// The document optimizer could not produce a valid result.
    #[error("optimization failed: {0}")]
    OptimizationFailed(String),
}

/// Result type used by the core crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A loaded PDF document.
///
/// A document owns the source bytes and the parsed structural layer. The
/// header is the only part that can make [`Document::from_bytes`] fail
/// directly; all other parse failures are reported by [`Document::page_count`],
/// [`Document::page`] and the other accessors.
#[derive(Debug, Clone)]
pub struct Document {
    bytes: Vec<u8>,
    version: String,
    structure: std::result::Result<Structure, Error>,
    changes: BTreeMap<ObjectId, Object>,
    /// Trailer keys staged by metadata writes, applied on top of the source
    /// trailer by both save paths. Copy-on-write like [`Self::changes`].
    trailer_entries: Dictionary,
}

impl Document {
    /// Load a document from an in-memory byte buffer.
    ///
    /// The header must begin with `%PDF-`; the version is read from the header
    /// line when present. The structural layer is parsed immediately and any
    /// error is stored for the accessors to report.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if !bytes.starts_with(b"%PDF-") {
            return Err(Error::InvalidHeader);
        }
        let version = bytes
            .get(5..8)
            .and_then(|slice| std::str::from_utf8(slice).ok())
            .unwrap_or("1.x")
            .to_owned();
        let structure = parser::parse_document(&bytes);
        Ok(Self {
            bytes,
            version,
            structure,
            changes: BTreeMap::new(),
            trailer_entries: Dictionary::new(),
        })
    }

    /// Return the declared PDF version, for example `1.7`.
    pub fn version(&self) -> &str {
        &self.version
    }

    /// Return the effective PDF version, honoring a catalog `/Version` override.
    ///
    /// This is the version that actually governs the document: the catalog
    /// value when present, otherwise the value from the header. When neither is
    /// readable, [`PdfVersion::DEFAULT`] is returned.
    pub fn effective_version(&self) -> PdfVersion {
        self.compatibility_report().effective_version()
    }

    /// Return a typed compatibility report derived from the source bytes.
    ///
    /// The report is produced even for a document whose structural layer is
    /// damaged, so callers can inspect the version and any header/catalog
    /// irregularities before deciding whether to attempt a repair.
    pub fn compatibility_report(&self) -> CompatibilityReport {
        compatibility::analyze_compatibility(&self.bytes)
    }

    /// Return the number of bytes in the source file.
    pub fn len(&self) -> usize {
        self.bytes.len()
    }

    /// Return `true` when the document has no bytes.
    pub fn is_empty(&self) -> bool {
        self.bytes.is_empty()
    }

    /// Return the raw source bytes.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the number of leaf pages resolved from the page tree.
    pub fn page_count(&self) -> Result<usize> {
        self.structure
            .as_ref()
            .map(|structure| structure.pages.len())
            .map_err(Clone::clone)
    }

    /// Return metadata for the page at `index` in document order.
    pub fn page(&self, index: usize) -> Result<Page> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        structure
            .pages
            .get(index)
            .cloned()
            .ok_or(Error::PageNotFound(index))
    }

    /// Return metadata for every page in document order.
    pub fn pages(&self) -> Result<Vec<Page>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        Ok(structure.pages.clone())
    }

    /// Return the decoded content bytes of the page at `index`.
    ///
    /// The byte streams referenced by the page's `/Contents` entry are resolved
    /// through the cross-reference table, decoded with [`decode_stream`], and
    /// concatenated in draw order. `FlateDecode` streams (including PNG
    /// predictors) are decoded transparently. A stream using any other filter
    /// is rejected with [`Error::FilteredContent`], which names the filter.
    ///
    /// # Errors
    ///
    /// Returns a structural error when the document body is damaged,
    /// [`Error::PageNotFound`] when `index` is out of range,
    /// [`Error::FilteredContent`] when a stream uses an unsupported filter,
    /// [`Error::ContentNotStream`] when a `/Contents` entry is not a stream,
    /// and any filter error from [`decode_stream`] for corrupt compressed data.
    pub fn page_content(&self, index: usize) -> Result<Vec<u8>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        let page = structure
            .pages
            .get(index)
            .ok_or(Error::PageNotFound(index))?;
        parser::page_content(&self.bytes, structure, page)
    }

    /// Extract the text shown by the page at `index`.
    ///
    /// This is a convenience wrapper that reads the page's unfiltered content
    /// bytes with [`Document::page_content`] and passes them to
    /// [`extract_text`]. It supports the `Tj`, `TJ`, `'` and `"` operators.
    ///
    /// # Errors
    ///
    /// Returns any error from [`Document::page_content`] or [`extract_text`].
    pub fn page_text(&self, index: usize) -> Result<String> {
        let bytes = self.page_content(index)?;
        extract_text(&bytes)
    }

    /// Return the indirect identifier of the catalog referenced by `/Root`.
    pub fn root(&self) -> Result<ObjectId> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        Ok(structure.root_id)
    }

    /// Load and return the indirect object identified by `id`.
    ///
    /// Both cross-reference forms are supported: objects stored in an
    /// `/ObjStm` are expanded through the same reader used for the page tree.
    ///
    /// # Errors
    ///
    /// Returns the stored structural error, [`Error::ObjectNotFound`] or
    /// [`Error::MalformedObjectStream`] when the object cannot be resolved.
    pub fn object(&self, id: ObjectId) -> Result<Object> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        parser::load_object(&self.bytes, &structure.xref, id)
    }

    /// Return the validated linearization hint, if this file carries one.
    ///
    /// `Ok(None)` means the first few objects contain no `/Linearized`
    /// dictionary; a present but malformed dictionary is reported as
    /// [`Error::MalformedLinearization`]. This reads the hint only — it does
    /// not verify or produce a linearized file layout.
    ///
    /// # Errors
    ///
    /// Returns [`Error::MalformedLinearization`] for an invalid hint.
    pub fn linearization(&self) -> Result<Option<Linearization>> {
        linearization::parse_linearization(&self.bytes)
    }

    /// Save this document with object streams and an xref stream.
    ///
    /// Eligible objects are packed into `/ObjStm` streams, streams and the
    /// catalog/page tree remain standalone with type-1 xref entries, and a
    /// single `/Type /XRef` stream replaces the classic xref table and trailer.
    /// Staged changes from [`Document::set_object`] replace the corresponding
    /// source objects. Unlike [`Document::save`], this path can read a source
    /// that already uses xref streams because compressed objects are resolved
    /// through the parser. Encrypted documents are declined.
    ///
    /// # Errors
    ///
    /// Returns the stored structural error, [`Error::UnsupportedState`] for
    /// encrypted input, or a serialization error for malformed object data.
    pub fn save_with_object_streams(&self, options: &ObjectStreamOptions) -> Result<Vec<u8>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        if structure.xref.trailer().contains_key("Encrypt") {
            return Err(Error::UnsupportedState(
                "encrypted documents cannot be saved",
            ));
        }
        let mut objects = Vec::new();
        for number in structure.xref.all_object_numbers() {
            if number == 0 {
                continue;
            }
            let generation = parser::object_generation(&self.bytes, &structure.xref, number)?;
            let id = ObjectId::new(number, generation);
            let object = match self.changes.get(&id) {
                Some(changed) => changed.clone(),
                None => parser::load_object(&self.bytes, &structure.xref, id)?,
            };
            objects.push((id, object));
        }
        for (id, object) in &self.changes {
            if !objects.iter().any(|(existing, _)| existing == id) {
                objects.push((*id, object.clone()));
            }
        }
        objects.sort_by_key(|(id, _)| id.number);
        objstm::write_object_stream_document(&objects, structure.root_id, options)
    }

    /// Save this document as a fully linearized, classic-xref PDF.
    ///
    /// The first page (the first leaf of the page tree) and every object it
    /// needs are written ahead of the rest of the file, followed by a first-page
    /// `xref` table, the remaining objects, and a main `xref` table whose
    /// `/Prev` entry points back at the first-page section. A `/Linearized`
    /// parameter dictionary and a hint stream with page-offset and shared-
    /// object tables are emitted at the front. Staged changes from
    /// [`Document::set_object`] are applied just like in
    /// [`Document::save_with_object_streams`], and the result can be read back
    /// with [`Document::from_bytes`] and inspected with
    /// [`Document::linearization`].
    ///
    /// Unlike [`Document::save`], this path can read a source that already uses
    /// xref streams because compressed objects are resolved through the parser.
    /// Encrypted documents are declined.
    ///
    /// # Errors
    ///
    /// Returns the stored structural error, [`Error::UnsupportedState`] for
    /// encrypted input, [`Error::LinearizationFailed`] for an empty document,
    /// and any serialization error from [`linearize::write_linearized_document`].
    pub fn save_linearized(&self, options: &LinearizationOptions) -> Result<Vec<u8>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        if structure.xref.trailer().contains_key("Encrypt") {
            return Err(Error::UnsupportedState(
                "encrypted documents cannot be saved",
            ));
        }
        let mut objects = Vec::new();
        for number in structure.xref.all_object_numbers() {
            if number == 0 {
                continue;
            }
            let generation = parser::object_generation(&self.bytes, &structure.xref, number)?;
            let id = ObjectId::new(number, generation);
            let object = match self.changes.get(&id) {
                Some(changed) => changed.clone(),
                None => parser::load_object(&self.bytes, &structure.xref, id)?,
            };
            objects.push((id, object));
        }
        for (id, object) in &self.changes {
            if !objects.iter().any(|(existing, _)| existing == id) {
                objects.push((*id, object.clone()));
            }
        }
        objects.sort_by_key(|(id, _)| id.number);
        let first_page = structure
            .pages
            .first()
            .ok_or_else(|| Error::LinearizationFailed("document has no pages".to_owned()))?
            .id;
        linearize::write_linearized_document(&objects, structure.root_id, first_page, options)
    }

    /// Return the trailer `/Size` value when present and non-negative.
    pub fn trailer_size(&self) -> Result<Option<u32>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        Ok(structure.trailer_size)
    }

    /// Return the `/Count` declared by the root `/Pages` node, when valid.
    ///
    /// [`Document::page_count`] reports the number of leaf pages actually
    /// found, which is normally equal to this value.
    pub fn declared_page_count(&self) -> Result<Option<u32>> {
        let structure = self.structure.as_ref().map_err(Clone::clone)?;
        Ok(structure.declared_page_count)
    }

    /// Stage an indirect object to be replaced or added on the next save.
    ///
    /// The object is held as a copy-on-write change: the original bytes are
    /// never modified in place, and both [`Document::save`] and
    /// [`Document::save_incremental`] write the staged value. Replacing an
    /// existing object number causes its new xref entry to supersede the old
    /// one. Adding a number larger than the original `/Size` grows the table.
    ///
    /// # Errors
    ///
    /// Returns [`Error::InvalidObjectId`] when `id.number` is zero (reserved
    /// for the free-object list head) or when `id.generation` is non-zero
    /// (non-zero generations are not supported by this writer).
    pub fn set_object(&mut self, id: ObjectId, object: Object) -> Result<()> {
        if id.number == 0 {
            return Err(Error::InvalidObjectId(
                "object number 0 is reserved for the free list".to_owned(),
            ));
        }
        if id.generation != 0 {
            return Err(Error::InvalidObjectId(
                "non-zero generation numbers are not supported".to_owned(),
            ));
        }
        self.changes.insert(id, object);
        Ok(())
    }

    /// Serialize the whole document as a standalone PDF byte stream.
    ///
    /// Object numbers are preserved and unchanged objects are copied from the
    /// source verbatim, so stream bytes are not re-compressed or rewritten.
    /// Staged changes from [`Document::set_object`] replace or extend the
    /// object set. A single classic xref table, trailer, `startxref` and
    /// `%%EOF` are regenerated with byte-accurate offsets.
    ///
    /// # Errors
    ///
    /// Returns the stored structural error when the source is malformed,
    /// [`Error::UnsupportedState`] for encrypted documents or cross-reference
    /// streams, and serialization errors when a staged object cannot be
    /// encoded (see [`Error::CannotSerialize`]).
    pub fn save(&self) -> Result<Vec<u8>> {
        let structure = self.supported_structure()?;
        let mut out = Vec::with_capacity(self.bytes.len() + 64);
        out.extend_from_slice(format!("%PDF-{}\n%PDFXI\n", self.version).as_bytes());

        let mut objects: Vec<(u32, Vec<u8>)> = Vec::new();
        for (number, offset) in structure.xref.in_use_entries() {
            if self.changes.keys().any(|id| id.number == number) {
                continue;
            }
            let range = parser::object_range(&self.bytes, offset)?;
            let raw = self
                .bytes
                .get(range)
                .ok_or_else(|| Error::MalformedObject {
                    number,
                    reason: "object range lies outside the source".to_owned(),
                })?;
            objects.push((number, raw.to_vec()));
        }
        for (id, object) in &self.changes {
            let mut body = Vec::new();
            writer::serialize_indirect(*id, object, &mut body)?;
            objects.push((id.number, body));
        }
        objects.sort_by_key(|(number, _)| *number);

        let mut offsets = BTreeMap::new();
        for (number, body) in &objects {
            offsets.insert(*number, out.len());
            out.extend_from_slice(body);
            out.push(b'\n');
        }

        let xref_offset = out.len();
        writer::write_xref_section(&mut out, &offsets, true);

        let highest = objects.last().map_or(0, |(number, _)| *number);
        let mut trailer = structure.xref.trailer();
        for (key, value) in &self.trailer_entries {
            trailer.insert(key.clone(), value.clone());
        }
        let size = trailer_size(&trailer, highest);
        trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));
        trailer.insert("Root".to_owned(), Object::Reference(structure.root_id));
        writer::write_trailer(&mut out, &trailer, xref_offset)?;
        Ok(out)
    }

    /// Append a revision to the original bytes without rewriting them.
    ///
    /// Staged changes from [`Document::set_object`] are written as new indirect
    /// objects, followed by a new xref section whose trailer carries a `/Prev`
    /// entry pointing at the original `startxref`. The returned bytes start
    /// with the complete, byte-for-byte source document.
    ///
    /// # Errors
    ///
    /// Returns the stored structural error when the source is malformed,
    /// [`Error::UnsupportedState`] for encrypted documents or cross-reference
    /// streams, and serialization errors when a staged object cannot be
    /// encoded (see [`Error::CannotSerialize`]).
    pub fn save_incremental(&self) -> Result<Vec<u8>> {
        let structure = self.supported_structure()?;
        let previous = parser::startxref_offset(&self.bytes)?;
        let mut out = self.bytes.clone();
        out.push(b'\n');

        let mut offsets = BTreeMap::new();
        let mut highest = 0u32;
        for (id, object) in &self.changes {
            offsets.insert(id.number, out.len());
            writer::serialize_indirect(*id, object, &mut out)?;
            out.push(b'\n');
            highest = highest.max(id.number);
        }

        let xref_offset = out.len();
        writer::write_xref_section(&mut out, &offsets, false);

        let mut trailer = structure.xref.trailer();
        for (key, value) in &self.trailer_entries {
            trailer.insert(key.clone(), value.clone());
        }
        let size = trailer_size(&trailer, highest);
        trailer.insert("Size".to_owned(), Object::Integer(i64::from(size)));
        trailer.insert("Root".to_owned(), Object::Reference(structure.root_id));
        let previous = i64::try_from(previous)
            .map_err(|_| Error::CannotSerialize("previous startxref exceeds i64".to_owned()))?;
        trailer.insert("Prev".to_owned(), Object::Integer(previous));
        writer::write_trailer(&mut out, &trailer, xref_offset)?;
        Ok(out)
    }

    /// Begin a page plan whose sole source and initial selection are this
    /// document's pages in document order.
    ///
    /// The returned plan is a value object: it can be inspected, cloned and
    /// chained with additional [`PageOperation`]s before being handed back to
    /// [`Document::apply_page_plan`]. Use the static constructors on
    /// [`PagePlan`] when the desired operation is already known.
    pub fn page_plan(&self) -> PagePlan {
        PagePlan::identity()
    }

    /// Apply `plan` to this document (the plan's primary source) and return a
    /// standalone, classic-table PDF byte stream.
    ///
    /// This is the single entry point for merge, split, extract, reorder and
    /// rotate. The plan's operations are validated against the real page counts
    /// of this document and any documents added through
    /// [`PagePlan::merge`]; hostile or out-of-range indices are rejected with
    /// structured errors before any bytes are written.
    ///
    /// # Errors
    ///
    /// Returns [`Error::EmptyPageSelection`], [`Error::InvalidPageIndex`],
    /// [`Error::InvalidSourceDocument`], [`Error::DuplicatePageIndex`] or
    /// [`Error::InvalidPageRotation`] for invalid plans,
    /// [`Error::UnsupportedState`] for encrypted or xref-stream sources, and
    /// any structural or serialization error from the underlying parser and
    /// writer.
    pub fn apply_page_plan(&self, plan: &PagePlan) -> Result<Vec<u8>> {
        pages::apply(self, plan)
    }

    /// Optimize this document into a smaller, semantically equivalent PDF.
    ///
    /// The optimizer compacts the object graph (deduplicating identical
    /// objects, dropping unreachable ones and renumbering survivors), compresses
    /// unfiltered streams with Flate when strictly beneficial, passes encoded
    /// images through untouched, and regenerates a correct classic xref table
    /// and `startxref`. Metadata and optional entries are removed only when the
    /// corresponding [`OptimizeOptions`] flags are enabled. The returned
    /// [`OptimizationReport`] is deterministic and records every transformation
    /// and skipped item. Progress can be cancelled through `cancel`.
    ///
    /// The source document is never mutated, and unknown dictionary keys are
    /// preserved. Incremental-save inputs (classic tables with `/Prev` chains)
    /// are fully supported; the result is always a single-revision standalone
    /// file.
    ///
    /// # Errors
    ///
    /// Returns [`Error::UnsupportedState`] for encrypted or xref-stream inputs,
    /// [`Error::Cancelled`] when `cancel` is signalled, and structural or
    /// serialization errors for malformed documents.
    pub fn optimize(
        &self,
        options: &OptimizeOptions,
        cancel: &CancellationToken,
    ) -> Result<OptimizationResult> {
        optimizer::optimize(self, options, cancel)
    }

    /// Return the parsed structure only when the document can be saved.
    ///
    /// Xref-stream inputs (including documents that reach one through a
    /// `/Prev` chain) are declined with [`Error::UnsupportedState`] rather than
    /// silently rewritten: the writer emits only classic tables and would drop
    /// objects held in object streams. Encrypted documents are declined for the
    /// same reason — the writer does not encrypt its output.
    pub(crate) fn supported_structure(&self) -> Result<&Structure> {
        match &self.structure {
            Ok(structure) => {
                if structure.xref.trailer().contains_key("Encrypt") {
                    return Err(Error::UnsupportedState(
                        "encrypted documents cannot be saved",
                    ));
                }
                if structure.xref.uses_xref_stream {
                    return Err(Error::UnsupportedState(
                        "cross-reference streams are not supported",
                    ));
                }
                Ok(structure)
            }
            Err(error) => {
                if let Ok(offset) = parser::startxref_offset(&self.bytes)
                    && parser::looks_like_xref_stream(&self.bytes, offset)
                {
                    return Err(Error::UnsupportedState(
                        "cross-reference streams are not supported",
                    ));
                }
                Err(error.clone())
            }
        }
    }
}

/// Compute a `/Size` value that covers both the declared size and the highest
/// object number written by a save.
fn trailer_size(trailer: &Dictionary, highest_object: u32) -> u32 {
    let declared = trailer
        .get("Size")
        .and_then(Object::as_i64)
        .filter(|value| *value >= 0)
        .and_then(|value| u32::try_from(value).ok())
        .unwrap_or(0);
    declared.max(highest_object.saturating_add(1)).max(1)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Replace the numeric value following `startxref` with `value`.
    fn set_startxref(pdf: &[u8], value: &str) -> Vec<u8> {
        let text = String::from_utf8(pdf.to_vec()).expect("fixture is ASCII");
        let marker = "startxref\n";
        let start = text.find(marker).expect("startxref marker") + marker.len();
        let end = start + text[start..].find('\n').expect("line ending");
        let mut result = String::with_capacity(text.len());
        result.push_str(&text[..start]);
        result.push_str(value);
        result.push_str(&text[end..]);
        result.into_bytes()
    }

    /// Turn the first in-use xref entry's `n` marker into an invalid type.
    fn corrupt_xref_entry(pdf: &[u8]) -> Vec<u8> {
        let mut bytes = pdf.to_vec();
        let pattern = b" 00000 n ";
        let position = bytes
            .windows(pattern.len())
            .position(|window| window == pattern)
            .expect("in-use xref entry");
        bytes[position + pattern.len() - 2] = b'x';
        bytes
    }

    /// Replace the first occurrence of `from` with `to` in an ASCII fixture.
    ///
    /// The replacement is padded with white space so every byte offset in the
    /// xref table stays valid; this keeps each test focused on one defect.
    fn replace_once(pdf: &[u8], from: &str, to: &str) -> Vec<u8> {
        assert!(
            to.len() <= from.len(),
            "replacement must not grow the fixture"
        );
        let text = String::from_utf8(pdf.to_vec()).expect("fixture is ASCII");
        let position = text.find(from).expect("pattern must exist");
        let mut result = String::with_capacity(text.len());
        result.push_str(&text[..position]);
        result.push_str(to);
        for _ in 0..from.len().saturating_sub(to.len()) {
            result.push(' ');
        }
        result.push_str(&text[position + from.len()..]);
        result.into_bytes()
    }

    /// Build a deterministic PDF with `count` pages and distinct MediaBoxes.
    fn multi_page_pdf(count: usize) -> Vec<u8> {
        let mut objects: Vec<String> = Vec::new();
        let page_ids: Vec<usize> = (3..3 + count).collect();
        let kids = page_ids
            .iter()
            .map(|id| format!("{id} 0 R"))
            .collect::<Vec<_>>()
            .join(" ");
        objects.push("<< /Type /Catalog /Pages 2 0 R >>".to_owned());
        objects.push(format!(
            "<< /Type /Pages /Kids [ {kids} ] /Count {count} >>"
        ));
        for index in 0..count {
            let width = 612 + 10 * index;
            let height = 792 + 5 * index;
            let content_id = 3 + count + index;
            objects.push(format!(
                "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 {width} {height}] \
                 /Contents {content_id} 0 R >>"
            ));
        }
        for index in 0..count {
            let content = format!("BT (Page {}) Tj ET", index + 1);
            objects.push(format!(
                "<< /Length {} >>\nstream\n{}\nendstream",
                content.len(),
                content
            ));
        }

        let mut out = String::from("%PDF-1.7\n%PDFXI\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, body) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, body));
        }
        let xref_offset = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{:010} 00000 n \n", offset));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref_offset
        ));
        out.into_bytes()
    }

    /// Build a one-page PDF whose object 4 is the supplied content object body.
    fn one_page_pdf_with_content(body: &str) -> Vec<u8> {
        let objects = [
            "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
            "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
            "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] \
             /Contents 4 0 R >>"
                .to_owned(),
            body.to_owned(),
        ];
        let mut out = String::from("%PDF-1.7\n");
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, object));
        }
        let xref = out.len();
        out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
        out.push_str("0000000000 65535 f \n");
        for offset in &offsets {
            out.push_str(&format!("{:010} 00000 n \n", offset));
        }
        out.push_str(&format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
            objects.len() + 1,
            xref
        ));
        out.into_bytes()
    }

    #[test]
    fn accepts_pdf_header_and_reads_version() {
        let doc = Document::from_bytes(b"%PDF-1.7\n%%EOF\n".to_vec()).expect("valid header");
        assert_eq!(doc.version(), "1.7");
        assert!(!doc.is_empty());
        assert_eq!(doc.len(), 15);
        // A header-only file has no cross-reference table; the placeholder
        // page count from T-0006 is now an explicit structural error.
        assert!(matches!(doc.page_count(), Err(Error::MissingStartXref)));
    }

    #[test]
    fn rejects_non_pdf_input() {
        let err = Document::from_bytes(b"hello".to_vec()).expect_err("must reject");
        assert!(matches!(err, Error::InvalidHeader));
    }

    #[test]
    fn parses_one_page_pdf_from_testkit() {
        let doc = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
        assert_eq!(doc.page_count().expect("page count"), 1);
        assert_eq!(doc.root().expect("root"), ObjectId::new(1, 0));
        assert_eq!(doc.trailer_size().expect("size"), Some(6));
        assert_eq!(doc.declared_page_count().expect("count"), Some(1));

        let page = doc.page(0).expect("first page");
        assert_eq!(page.index, 0);
        assert_eq!(page.id, ObjectId::new(3, 0));
        assert_eq!(page.media_box, Rectangle::new(0.0, 0.0, 612.0, 792.0));
        assert_eq!(page.width(), 612.0);
        assert_eq!(page.height(), 792.0);
        assert_eq!(page.contents, vec![ObjectId::new(4, 0)]);
        assert!(page.resources.is_none());
    }

    #[test]
    fn parses_multi_page_pdf_and_preserves_order() {
        let doc = Document::from_bytes(multi_page_pdf(3)).expect("valid fixture");
        assert_eq!(doc.page_count().expect("page count"), 3);
        assert_eq!(doc.declared_page_count().expect("count"), Some(3));
        assert_eq!(doc.pages().expect("pages").len(), 3);

        for index in 0..3 {
            let page = doc.page(index).expect("page in range");
            assert_eq!(page.index, index);
            assert_eq!(page.id, ObjectId::new(3 + index as u32, 0));
            assert_eq!(page.width(), 612.0 + 10.0 * index as f64);
            assert_eq!(page.height(), 792.0 + 5.0 * index as f64);
            assert_eq!(page.contents, vec![ObjectId::new((6 + index) as u32, 0)]);
        }
        let err = doc.page(3).expect_err("index out of range");
        assert!(matches!(err, Error::PageNotFound(3)));
    }

    #[test]
    fn rejects_missing_xref() {
        let header_only = Document::from_bytes(b"%PDF-1.7\n%%EOF\n".to_vec()).expect("header");
        assert!(matches!(
            header_only.page_count(),
            Err(Error::MissingStartXref)
        ));

        let no_table = set_startxref(&pdfxi_testkit::minimal_pdf(), "8");
        let doc = Document::from_bytes(no_table).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::MissingXref(8))));
    }

    #[test]
    fn rejects_bad_startxref() {
        let out_of_bounds = set_startxref(&pdfxi_testkit::minimal_pdf(), "999999999");
        let doc = Document::from_bytes(out_of_bounds).expect("header");
        assert!(matches!(
            doc.page_count(),
            Err(Error::StartXrefOutOfBounds { .. })
        ));

        let not_a_number = set_startxref(&pdfxi_testkit::minimal_pdf(), "abc");
        let doc = Document::from_bytes(not_a_number).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::InvalidStartXref(_))));
    }

    #[test]
    fn rejects_malformed_xref_entry() {
        let doc = Document::from_bytes(corrupt_xref_entry(&pdfxi_testkit::minimal_pdf()))
            .expect("header");
        assert!(matches!(doc.page_count(), Err(Error::MalformedXref(_))));
    }

    #[test]
    fn rejects_missing_root_and_pages() {
        let no_root = replace_once(&pdfxi_testkit::minimal_pdf(), "/Root 1 0 R", "/Foo 1 0 R");
        let doc = Document::from_bytes(no_root).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::MissingRoot)));

        let no_pages = replace_once(
            &pdfxi_testkit::minimal_pdf(),
            "/Catalog /Pages 2 0 R",
            "/Catalog /Foo 2 0 R",
        );
        let doc = Document::from_bytes(no_pages).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::MissingPages)));
    }

    #[test]
    fn rejects_invalid_media_box_and_page_tree_cycle() {
        let bad_box = replace_once(
            &pdfxi_testkit::minimal_pdf(),
            "/MediaBox [0 0 612 792]",
            "/MediaBox [0 0]",
        );
        let doc = Document::from_bytes(bad_box).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::InvalidMediaBox(3))));

        let cycle = replace_once(&multi_page_pdf(1), "/Kids [ 3 0 R ]", "/Kids [ 2 0 R ]");
        let doc = Document::from_bytes(cycle).expect("header");
        assert!(matches!(doc.page_count(), Err(Error::MalformedPageTree(_))));
    }

    #[test]
    fn page_content_returns_raw_unfiltered_bytes() {
        let doc = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
        let bytes = doc.page_content(0).expect("content bytes");
        let text = String::from_utf8(bytes).expect("ASCII stream");
        assert!(text.contains("BT /F1 24 Tf 72 720 Td (Hello, PDFXI!) Tj ET"));
    }

    #[test]
    fn page_text_extracts_minimal_fixture() {
        let doc = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
        assert_eq!(doc.page_text(0).expect("page text"), "Hello, PDFXI!");
    }

    #[test]
    fn page_text_extracts_each_page_of_multipage_fixture() {
        let doc = Document::from_bytes(multi_page_pdf(3)).expect("valid fixture");
        for index in 0..3 {
            assert_eq!(
                doc.page_text(index).expect("page text"),
                format!("Page {}", index + 1)
            );
        }
    }

    #[test]
    fn page_content_rejects_unsupported_filter_and_names_it() {
        let content = b"BT (Hi) Tj ET";
        let dict = b"<< /Length 13 /Filter /LZWDecode >>";
        let pdf = one_page_pdf_with_binary_stream(dict, content);
        let doc = Document::from_bytes(pdf).expect("valid fixture");
        let err = doc.page_content(0).expect_err("LZW is unsupported");
        assert!(
            matches!(&err, Error::FilteredContent { id, filter }
                if *id == ObjectId::new(4, 0) && filter == "LZWDecode"),
            "unexpected error: {err}"
        );
        assert!(err.to_string().contains("LZWDecode"));
    }

    /// Build a one-page PDF whose object 4 is a binary content stream.
    ///
    /// The dictionary and payload are byte slices so compressed streams with
    /// arbitrary bytes and an exact `/Length` can be embedded without breaking
    /// the cross-reference offsets.
    fn one_page_pdf_with_binary_stream(dict: &[u8], stream: &[u8]) -> Vec<u8> {
        let mut objects: Vec<Vec<u8>> = vec![
            b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
            b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
            b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_vec(),
        ];
        let mut fourth = dict.to_vec();
        fourth.extend_from_slice(b"\nstream\n");
        fourth.extend_from_slice(stream);
        fourth.extend_from_slice(b"\nendstream");
        objects.push(fourth);

        let mut out = b"%PDF-1.7\n".to_vec();
        let mut offsets = Vec::with_capacity(objects.len());
        for (index, object) in objects.iter().enumerate() {
            offsets.push(out.len());
            out.extend_from_slice(format!("{} 0 obj\n", index + 1).as_bytes());
            out.extend_from_slice(object);
            out.extend_from_slice(b"\nendobj\n");
        }
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", objects.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for offset in &offsets {
            out.extend_from_slice(format!("{offset:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                objects.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    #[test]
    fn flate_compressed_page_content_is_decoded_transparently() {
        // A genuine zlib stream produced by .NET DeflateStream for the content
        // "BT /F1 24 Tf 72 720 Td (Hello, Flate PDFXI!) Tj ET".
        const COMPRESSED: &[u8] = &[
            0x78, 0x9C, 0x73, 0x0A, 0x51, 0xD0, 0x77, 0x33, 0x54, 0x30, 0x32, 0x51, 0x08, 0x49,
            0x53, 0x30, 0x37, 0x02, 0x22, 0x03, 0x85, 0x90, 0x14, 0x05, 0x0D, 0x8F, 0xD4, 0x9C,
            0x9C, 0x7C, 0x1D, 0x05, 0xB7, 0x9C, 0xC4, 0x92, 0x54, 0x85, 0x00, 0x17, 0xB7, 0x08,
            0x4F, 0x45, 0x4D, 0x85, 0x90, 0x2C, 0x05, 0xD7, 0x10, 0x00, 0x34, 0x63, 0x0C, 0xC7,
        ];
        let dict = format!(
            "<< /Length {} /Filter /FlateDecode /DecodeParms << /Predictor 1 /Colors 1 /BitsPerComponent 8 /Columns 1 >> >>",
            COMPRESSED.len()
        )
        .into_bytes();
        let pdf = one_page_pdf_with_binary_stream(&dict, COMPRESSED);
        let doc = Document::from_bytes(pdf).expect("valid compressed fixture");
        assert_eq!(
            doc.page_text(0).expect("decoded page text"),
            "Hello, Flate PDFXI!"
        );
    }

    #[test]
    fn page_content_rejects_non_stream_object() {
        let doc = Document::from_bytes(one_page_pdf_with_content("<< /Type /Font >>"))
            .expect("valid fixture");
        assert!(matches!(
            doc.page_content(0),
            Err(Error::ContentNotStream(id)) if id == ObjectId::new(4, 0)
        ));
    }

    #[test]
    fn page_content_rejects_out_of_range_index() {
        let doc = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
        assert!(matches!(doc.page_content(9), Err(Error::PageNotFound(9))));
        assert!(matches!(doc.page_text(9), Err(Error::PageNotFound(9))));
    }

    /// Return `true` when `needle` occurs anywhere in `haystack`.
    fn contains_bytes(haystack: &[u8], needle: &[u8]) -> bool {
        !needle.is_empty() && haystack.windows(needle.len()).any(|w| w == needle)
    }

    /// Return the byte position of `needle`, or zero when absent.
    fn find_bytes(haystack: &[u8], needle: &[u8]) -> usize {
        haystack
            .windows(needle.len())
            .position(|w| w == needle)
            .unwrap_or(0)
    }

    /// Insert an extra entry into the trailer dictionary of an ASCII fixture.
    ///
    /// The trailer follows the xref table, so growing it does not invalidate
    /// any object offsets or the `startxref` value.
    fn with_trailer_entry(pdf: &[u8], entry: &str) -> Vec<u8> {
        let text = String::from_utf8(pdf.to_vec()).expect("fixture is ASCII");
        let marker = "trailer\n<<";
        let position = text.find(marker).expect("trailer dictionary") + marker.len();
        let mut result = String::with_capacity(text.len() + entry.len() + 1);
        result.push_str(&text[..position]);
        result.push(' ');
        result.push_str(entry);
        result.push_str(&text[position..]);
        result.into_bytes()
    }

    /// Assert that `saved` reparses to the same structural facts as `source`.
    fn assert_round_trip(source: &[u8], saved: &[u8]) {
        let before = Document::from_bytes(source.to_vec()).expect("source parses");
        let after = Document::from_bytes(saved.to_vec()).expect("saved PDF parses");
        assert_eq!(after.version(), before.version());
        assert_eq!(
            after.page_count().expect("saved page count"),
            before.page_count().expect("source page count")
        );
        assert_eq!(
            after.root().expect("saved root"),
            before.root().expect("source root")
        );
        assert_eq!(
            after
                .pages()
                .expect("saved pages")
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            before
                .pages()
                .expect("source pages")
                .iter()
                .map(|p| p.id)
                .collect::<Vec<_>>(),
            "page order and object ids must be preserved"
        );
        let count = before.page_count().expect("page count");
        for index in 0..count {
            assert_eq!(
                after.page_text(index).expect("saved page text"),
                before.page_text(index).expect("source page text"),
                "page {index} text changed across save"
            );
        }
    }

    #[test]
    fn save_round_trips_minimal_fixture() {
        let source = pdfxi_testkit::minimal_pdf();
        let saved = Document::from_bytes(source.clone())
            .expect("valid fixture")
            .save()
            .expect("full save");
        assert!(saved.starts_with(b"%PDF-1.7\n"));
        assert!(saved.ends_with(b"%%EOF\n"));
        assert!(contains_bytes(&saved, b"startxref\n"));
        assert_round_trip(&source, &saved);
    }

    #[test]
    fn save_round_trips_text_fixture() {
        let source = pdfxi_testkit::text_pdf("Round-trip payload 42");
        let saved = Document::from_bytes(source.clone())
            .expect("valid fixture")
            .save()
            .expect("full save");
        assert_round_trip(&source, &saved);
        let reparsed = Document::from_bytes(saved).expect("saved PDF parses");
        assert_eq!(
            reparsed.page_text(0).expect("page text"),
            "Round-trip payload 42"
        );
    }

    #[test]
    fn save_round_trips_multipage_fixture_in_order() {
        let source = pdfxi_testkit::multipage_pdf(4);
        let saved = Document::from_bytes(source.clone())
            .expect("valid fixture")
            .save()
            .expect("full save");
        assert_round_trip(&source, &saved);
    }

    #[test]
    fn save_preserves_object_numbers_and_compressed_stream_bytes() {
        const COMPRESSED: &[u8] = &[
            0x78, 0x9C, 0x73, 0x0A, 0x51, 0xD0, 0x77, 0x33, 0x54, 0x30, 0x32, 0x51, 0x08, 0x49,
            0x53, 0x30, 0x37, 0x02, 0x22, 0x03, 0x85, 0x90, 0x14, 0x05, 0x0D, 0x8F, 0xD4, 0x9C,
            0x9C, 0x7C, 0x1D, 0x05, 0xB7, 0x9C, 0xC4, 0x92, 0x54, 0x85, 0x00, 0x17, 0xB7, 0x08,
            0x4F, 0x45, 0x4D, 0x85, 0x90, 0x2C, 0x05, 0xD7, 0x10, 0x00, 0x34, 0x63, 0x0C, 0xC7,
        ];
        let dict = format!(
            "<< /Length {} /Filter /FlateDecode /DecodeParms << /Predictor 1 /Colors 1 /BitsPerComponent 8 /Columns 1 >> >>",
            COMPRESSED.len()
        );
        let source = one_page_pdf_with_binary_stream(dict.as_bytes(), COMPRESSED);
        let saved = Document::from_bytes(source.clone())
            .expect("valid compressed fixture")
            .save()
            .expect("full save");

        assert!(contains_bytes(&saved, COMPRESSED), "stream bytes changed");
        for number in 1..=4u32 {
            assert!(
                contains_bytes(&saved, format!("{number} 0 obj").as_bytes()),
                "object {number} missing"
            );
        }
        assert_round_trip(&source, &saved);
    }

    #[test]
    fn save_writes_staged_change_with_preserved_object_numbers() {
        let source = pdfxi_testkit::minimal_pdf();
        let mut doc = Document::from_bytes(source.clone()).expect("valid fixture");
        let mut dict = Dictionary::new();
        dict.insert("Type".to_owned(), Object::Name("Custom".to_owned()));
        doc.set_object(ObjectId::new(9, 0), Object::Dictionary(dict))
            .expect("valid object id");

        let saved = doc.save().expect("full save with new object");
        assert!(contains_bytes(&saved, b"9 0 obj"));
        assert!(contains_bytes(&saved, b"/Size 10"));
        assert_round_trip(&source, &saved);
    }

    #[test]
    fn incremental_save_preserves_original_bytes_as_prefix() {
        let source = pdfxi_testkit::minimal_pdf();
        let mut doc = Document::from_bytes(source.clone()).expect("valid fixture");
        let mut dict = Dictionary::new();
        dict.insert("Type".to_owned(), Object::Name("Marker".to_owned()));
        doc.set_object(ObjectId::new(7, 0), Object::Dictionary(dict))
            .expect("valid object id");

        let saved = doc.save_incremental().expect("incremental save");
        assert!(
            saved.starts_with(&source),
            "the original bytes must be an exact prefix"
        );
        assert!(saved.len() > source.len());
        assert!(contains_bytes(&saved, b"7 0 obj"));
    }

    #[test]
    fn incremental_save_links_new_xref_to_previous_with_prev() {
        let source = pdfxi_testkit::minimal_pdf();
        let previous_startxref = parser::startxref_offset(&source).expect("original startxref");
        let mut doc = Document::from_bytes(source.clone()).expect("valid fixture");
        doc.set_object(ObjectId::new(8, 0), Object::Integer(99))
            .expect("valid object id");

        let saved = doc.save_incremental().expect("incremental save");
        let prev_entry = format!("/Prev {previous_startxref}");
        assert!(
            contains_bytes(&saved, prev_entry.as_bytes()),
            "new trailer must point /Prev at the original startxref"
        );
        let marker = b"startxref";
        let markers = saved.windows(marker.len()).filter(|w| *w == marker).count();
        assert_eq!(markers, 2, "two xref sections should be present");

        let new_startxref = parser::startxref_offset(&saved).expect("new startxref");
        assert_ne!(new_startxref, previous_startxref);
        assert!(saved[new_startxref..].starts_with(b"xref"));
        assert_round_trip(&source, &saved);
    }

    #[test]
    fn incremental_save_changed_stream_is_visible_after_reparse() {
        let source = pdfxi_testkit::minimal_pdf();
        let mut doc = Document::from_bytes(source.clone()).expect("valid fixture");
        let replacement = b"BT (Replaced content) Tj ET";
        let mut dictionary = Dictionary::new();
        dictionary.insert(
            "Length".to_owned(),
            Object::Integer(i64::try_from(replacement.len()).expect("small length")),
        );
        doc.set_object(
            ObjectId::new(4, 0),
            Object::Stream {
                dictionary,
                data: replacement.to_vec(),
            },
        )
        .expect("valid object id");

        let saved = doc.save_incremental().expect("incremental save");
        assert!(saved.starts_with(&source));
        let reparsed = Document::from_bytes(saved).expect("incremental PDF parses");
        assert_eq!(reparsed.page_count().expect("page count"), 1);
        assert_eq!(
            reparsed.page_text(0).expect("replaced page text"),
            "Replaced content"
        );
    }

    #[test]
    fn set_object_rejects_invalid_object_ids() {
        let mut doc = Document::from_bytes(pdfxi_testkit::minimal_pdf()).expect("valid fixture");
        assert!(matches!(
            doc.set_object(ObjectId::new(0, 0), Object::Null),
            Err(Error::InvalidObjectId(_))
        ));
        assert!(matches!(
            doc.set_object(ObjectId::new(3, 1), Object::Null),
            Err(Error::InvalidObjectId(_))
        ));
    }

    #[test]
    fn save_rejects_encrypted_document_with_structured_error() {
        let source = with_trailer_entry(&pdfxi_testkit::minimal_pdf(), "/Encrypt 9 0 R");
        let doc = Document::from_bytes(source).expect("encrypted trailer still parses");
        assert_eq!(doc.page_count().expect("page count"), 1);
        assert!(matches!(
            doc.save(),
            Err(Error::UnsupportedState(
                "encrypted documents cannot be saved"
            ))
        ));
        assert!(matches!(
            doc.save_incremental(),
            Err(Error::UnsupportedState(
                "encrypted documents cannot be saved"
            ))
        ));
    }

    #[test]
    fn save_rejects_xref_stream_with_structured_error() {
        let source = pdfxi_testkit::minimal_pdf();
        let first_object = find_bytes(&source, b"1 0 obj");
        assert_ne!(first_object, 0, "fixture must contain object 1");
        let stream_like = set_startxref(&source, &first_object.to_string());
        let doc = Document::from_bytes(stream_like).expect("header is still valid");
        assert!(matches!(
            doc.save(),
            Err(Error::UnsupportedState(
                "cross-reference streams are not supported"
            ))
        ));
    }

    #[test]
    fn save_rejects_malformed_document_with_structured_error() {
        let doc = Document::from_bytes(b"%PDF-1.7\n%%EOF\n".to_vec()).expect("valid header");
        assert!(matches!(doc.save(), Err(Error::MissingStartXref)));
        assert!(matches!(
            doc.save_incremental(),
            Err(Error::MissingStartXref)
        ));
    }
}
