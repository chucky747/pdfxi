//! Core PDF object model, parsing, and document lifecycle for PDFXI.
//!
//! This crate is deliberately free of UI and rendering dependencies. It owns the
//! typed representation of a PDF document, the cross-reference table, stream
//! filters, encryption hooks, and incremental save. Rendering lives in
//! `pdfxi-render`; editing semantics live in `pdfxi-edit`.
//!
//! [`Document::from_bytes`] validates the file header immediately and parses
//! the structural layer (classic xref table, trailer, catalog and page tree)
//! eagerly. Structural errors are retained on the handle and surfaced by the
//! page accessors, so callers can still inspect the version of a file whose
//! body is damaged.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod content;
pub mod filter;
pub mod object;
pub mod page;

mod lexer;
mod parser;

pub use content::extract_text;
pub use filter::{
    DEFAULT_DECODE_CAP, Filter, FlateParams, Predictor, decode_stream, decode_stream_with_limit,
};
pub use object::{Object, ObjectId};
pub use page::{Page, Rectangle};

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
    /// The requested page index does not exist.
    #[error("page index {0} is out of range")]
    PageNotFound(usize),
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
        })
    }

    /// Return the declared PDF version, for example `1.7`.
    pub fn version(&self) -> &str {
        &self.version
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
}
