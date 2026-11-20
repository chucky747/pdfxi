//! Typed errors for the Office export adapters.
//!
//! Model validation and ZIP packaging are shared, but each adapter has its own
//! error type so callers can match a precise failure. The [`OfficeExporter`]
//! contract wraps those in [`OfficeExportError`].

use super::zip::ZipError;
use thiserror::Error;

/// A model-level validation failure shared by every adapter.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum ModelError {
    /// The document contains no pages.
    #[error("empty document: at least one page is required")]
    EmptyDocument,
    /// The page count exceeds the documented limit.
    #[error("document has more than {limit} pages")]
    TooManyPages {
        /// The enforced page limit.
        limit: usize,
    },
    /// The same zero-based page index appears twice.
    #[error("duplicate page index {page}")]
    DuplicatePage {
        /// The duplicated page index.
        page: usize,
    },
    /// A page has non-positive or non-finite geometry.
    #[error("invalid geometry on page {page}: width and height must be finite and positive")]
    InvalidPageGeometry {
        /// The offending page index.
        page: usize,
    },
    /// A page carries more blocks than the documented limit.
    #[error("page {page} has more than {limit} blocks")]
    TooManyBlocks {
        /// The offending page index.
        page: usize,
        /// The enforced block limit.
        limit: usize,
    },
    /// A numeric or coordinate field is not finite.
    #[error("non-finite {field} on page {page}")]
    NonFinite {
        /// The offending page index.
        page: usize,
        /// Name of the field that failed validation.
        field: &'static str,
    },
    /// A text field is longer than the documented limit.
    #[error("{field} on page {page} exceeds {limit} characters")]
    TextTooLong {
        /// The offending page index.
        page: usize,
        /// Name of the field that failed validation.
        field: &'static str,
        /// The enforced character limit.
        limit: usize,
    },
    /// A table has too many rows.
    #[error("table on page {page} has more than {limit} rows")]
    TooManyTableRows {
        /// The offending page index.
        page: usize,
        /// The enforced row limit.
        limit: usize,
    },
    /// A table row has too many columns.
    #[error("table on page {page} has more than {limit} columns")]
    TooManyTableCols {
        /// The offending page index.
        page: usize,
        /// The enforced column limit.
        limit: usize,
    },
    /// A heading level is outside `1..=9`.
    #[error("invalid heading level {level} on page {page}")]
    InvalidHeadingLevel {
        /// The offending page index.
        page: usize,
        /// The rejected level.
        level: u8,
    },
    /// A list nesting level is outside `0..=8`.
    #[error("invalid list level {level} on page {page}")]
    InvalidListLevel {
        /// The offending page index.
        page: usize,
        /// The rejected level.
        level: u8,
    },
    /// A negative measurement cannot be expressed in the target format.
    #[error("negative {field} on page {page}")]
    NegativeMeasurement {
        /// The offending page index.
        page: usize,
        /// Name of the field that failed validation.
        field: &'static str,
    },
}

/// DOCX-specific failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum DocxExportError {
    /// The caller-supplied model failed validation.
    #[error(transparent)]
    Invalid(#[from] ModelError),
    /// ZIP packaging or validation failed.
    #[error(transparent)]
    Zip(#[from] ZipError),
}

/// XLSX-specific failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum XlsxExportError {
    /// The caller-supplied model failed validation.
    #[error(transparent)]
    Invalid(#[from] ModelError),
    /// ZIP packaging or validation failed.
    #[error(transparent)]
    Zip(#[from] ZipError),
}

/// PPTX-specific failure.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum PptxExportError {
    /// The caller-supplied model failed validation.
    #[error(transparent)]
    Invalid(#[from] ModelError),
    /// ZIP packaging or validation failed.
    #[error(transparent)]
    Zip(#[from] ZipError),
    /// Page geometry cannot be expressed in EMU drawing coordinates.
    #[error("page geometry cannot be expressed in EMU drawing coordinates")]
    SlideGeometry,
}

/// Typed failure at the [`super::OfficeExporter`] contract boundary.
#[derive(Debug, Error, Clone, PartialEq, Eq)]
pub enum OfficeExportError {
    /// The DOCX adapter failed.
    #[error(transparent)]
    Docx(#[from] DocxExportError),
    /// The XLSX adapter failed.
    #[error(transparent)]
    Xlsx(#[from] XlsxExportError),
    /// The PPTX adapter failed.
    #[error(transparent)]
    Pptx(#[from] PptxExportError),
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn adapter_errors_display_their_cause() {
        let docx = DocxExportError::Invalid(ModelError::EmptyDocument);
        assert!(docx.to_string().contains("empty document"));
        let wrapped = OfficeExportError::from(docx);
        assert_eq!(
            wrapped,
            OfficeExportError::Docx(DocxExportError::Invalid(ModelError::EmptyDocument))
        );
    }

    #[test]
    fn zip_errors_are_transparently_nested() {
        let err = XlsxExportError::from(ZipError::MissingPart("xl/workbook.xml".to_owned()));
        assert!(err.to_string().contains("xl/workbook.xml"));
        let pptx = PptxExportError::SlideGeometry;
        assert!(pptx.to_string().contains("EMU"));
    }
}
