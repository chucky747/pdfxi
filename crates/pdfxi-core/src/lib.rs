//! Core PDF object model, parsing, and document lifecycle for PDFXI.
//!
//! This crate is deliberately free of UI and rendering dependencies. It owns the
//! typed representation of a PDF document, the cross-reference table, stream
//! filters, encryption hooks, and incremental save. Rendering lives in
//! `pdfxi-render`; editing semantics live in `pdfxi-edit`.
//!
//! The initial implementation validates the file header and exposes a stable
//! document handle. Later milestones add the full object graph, xref streams,
//! filters, and incremental updates without changing the public entry points.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Errors returned by the core crate.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// The input does not start with a supported PDF header.
    #[error("invalid PDF header; expected %PDF-")]
    InvalidHeader,
    /// The requested feature is not implemented in this build.
    #[error("not implemented: {0}")]
    NotImplemented(&'static str),
}

/// Result type used by the core crate.
pub type Result<T> = std::result::Result<T, Error>;

/// A loaded PDF document.
///
/// A document owns the source bytes and, in later milestones, the parsed object
/// graph. It is intentionally cheap to construct so callers can validate a file
/// before committing to a full parse.
#[derive(Debug, Clone)]
pub struct Document {
    bytes: Vec<u8>,
    version: String,
}

impl Document {
    /// Load a document from an in-memory byte buffer.
    ///
    /// The header must begin with `%PDF-`; the version is read from the header
    /// line when present.
    pub fn from_bytes(bytes: Vec<u8>) -> Result<Self> {
        if !bytes.starts_with(b"%PDF-") {
            return Err(Error::InvalidHeader);
        }
        let version = bytes
            .get(5..8)
            .and_then(|slice| std::str::from_utf8(slice).ok())
            .unwrap_or("1.x")
            .to_owned();
        Ok(Self { bytes, version })
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

    /// Return the number of pages.
    ///
    /// The full page tree is implemented in milestone M1. Until then this
    /// returns `Ok(0)` for a structurally valid document and keeps the API
    /// stable for callers.
    pub fn page_count(&self) -> Result<usize> {
        Ok(0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_pdf_header_and_reads_version() {
        let doc = Document::from_bytes(b"%PDF-1.7\n%%EOF\n".to_vec()).expect("valid header");
        assert_eq!(doc.version(), "1.7");
        assert!(!doc.is_empty());
        assert_eq!(doc.page_count().expect("page count"), 0);
    }

    #[test]
    fn rejects_non_pdf_input() {
        let err = Document::from_bytes(b"hello".to_vec()).expect_err("must reject");
        assert!(matches!(err, Error::InvalidHeader));
    }
}
