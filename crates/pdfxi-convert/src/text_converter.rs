//! A [`Converter`] adapter that exports pre-extracted text as text or JSON.
//!
//! [`TextConverter`] deliberately sits *after* text extraction. The caller
//! first recovers text and spans from a document (for example with
//! `pdfxi_text::TextExtractor` or `pdfxi_core::page_text`), maps them into
//! [`JsonPage`]/[`JsonSpan`] values, and hands those to the adapter. This keeps
//! rendering and PDF parsing out of the export path: the adapter performs no
//! I/O, launches no external process, and never touches a native backend.
//!
//! # Boundary contract
//!
//! - [`DocumentMetadata::pages`] declares the source document's page count.
//! - [`JsonPage::page`] values are **zero-based** and must cover every page
//!   `0..pages`. Missing page text is reported as
//!   [`ConversionError::InvalidInput`] before any bytes are produced.
//! - [`Converter::export`] ignores its `bytes` argument (the raw PDF bytes):
//!   the text was supplied at construction time. This is intentional and is
//!   why the adapter is documented as consuming *pre-extracted* data.
//! - Text output uses the plan's [`TextEncoding`]; JSON output is always UTF-8
//!   as required by RFC 8259.

use crate::json::{DocumentJson, DocumentMetadata, JsonPage};
use crate::text::{TextExportPlan, export_text};
use crate::{ConversionError, ConversionOptions, Converter, ExportKind, SourceKind};
use std::path::Path;

/// A converter that exports pre-extracted page data as text or structured JSON.
///
/// Construct one with [`TextConverter::new`], then call [`Converter::export`]
/// with [`ExportKind::Text`] or [`ExportKind::Json`]. Any other target yields
/// [`ConversionError::UnsupportedExport`]. This adapter is export-only:
/// [`Converter::can_import`] always returns `false` and [`Converter::import`]
/// always fails.
#[derive(Debug, Clone)]
pub struct TextConverter {
    options: ConversionOptions,
    metadata: DocumentMetadata,
    pages: Vec<JsonPage>,
    plan: TextExportPlan,
}

impl TextConverter {
    /// Create a converter from document metadata, pre-extracted pages, and a
    /// text export plan.
    #[must_use]
    pub fn new(metadata: DocumentMetadata, pages: Vec<JsonPage>, plan: TextExportPlan) -> Self {
        Self {
            options: ConversionOptions::default(),
            metadata,
            pages,
            plan,
        }
    }

    /// Replace the conversion options.
    #[must_use]
    pub fn with_options(mut self, options: ConversionOptions) -> Self {
        self.options = options;
        self
    }

    /// Return the document metadata supplied at construction.
    #[must_use]
    pub fn metadata(&self) -> &DocumentMetadata {
        &self.metadata
    }

    /// Return the pre-extracted pages supplied at construction.
    #[must_use]
    pub fn pages(&self) -> &[JsonPage] {
        &self.pages
    }

    /// Return the text export plan.
    #[must_use]
    pub fn plan(&self) -> &TextExportPlan {
        &self.plan
    }

    /// Check that every declared page has extracted text.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] naming the first zero-based
    /// page in `0..metadata.pages` that has no [`JsonPage`] entry.
    fn check_page_coverage(&self) -> Result<(), ConversionError> {
        for page in 0..self.metadata.pages {
            if !self.pages.iter().any(|entry| entry.page == page) {
                return Err(ConversionError::InvalidInput {
                    message: format!("missing extracted text for page {page}"),
                });
            }
        }
        Ok(())
    }

    /// Build the plain-text output as a UTF-8 string.
    ///
    /// This applies [`export_text`] to the adapter's pages and is the same work
    /// performed by `Converter::export` with [`ExportKind::Text`], before the
    /// plan's encoding is applied.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] when the plan is invalid or a
    /// declared page has no extracted text.
    pub fn export_text_string(&self) -> Result<String, ConversionError> {
        self.options.validate()?;
        self.plan.validate()?;
        self.check_page_coverage()?;
        Ok(export_text(
            self.pages.iter().map(|page| (page.page, page.text.clone())),
            self.plan(),
        ))
    }

    /// Build the structured JSON output as a UTF-8 string.
    ///
    /// # Errors
    ///
    /// Returns [`ConversionError::InvalidInput`] when the options are invalid or
    /// a declared page has no extracted text.
    pub fn export_json_string(&self) -> Result<String, ConversionError> {
        self.options.validate()?;
        self.check_page_coverage()?;
        Ok(DocumentJson::new(self.metadata.clone(), self.pages.clone()).to_json_string())
    }
}

impl Converter for TextConverter {
    fn options(&self) -> &ConversionOptions {
        &self.options
    }

    fn can_import(&self, _kind: SourceKind) -> bool {
        false
    }

    fn import(&self, input: &Path) -> Result<Vec<u8>, ConversionError> {
        Err(ConversionError::UnsupportedSource {
            kind: SourceKind::from_path(input),
        })
    }

    fn export(&self, _bytes: &[u8], target: ExportKind) -> Result<Vec<u8>, ConversionError> {
        self.options.validate()?;
        match target {
            ExportKind::Text => {
                let text = self.export_text_string()?;
                Ok(self.plan.encoding().encode(&text))
            }
            ExportKind::Json => Ok(self.export_json_string()?.into_bytes()),
            other => Err(ConversionError::UnsupportedExport { target: other }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::JsonSpan;
    use crate::text::{TextEncoding, TextPageSelection};
    use std::path::Path;

    fn page(index: usize, text: &str) -> JsonPage {
        JsonPage::new(index, text, vec![JsonSpan::new(text, 0.0, 0.0, 2.0, 3.0)])
    }

    fn converter() -> TextConverter {
        TextConverter::new(
            DocumentMetadata::new("Title", "Author", 2),
            vec![page(1, "second"), page(0, "first")],
            TextExportPlan::new(),
        )
    }

    #[test]
    fn text_converter_exports_text_bytes_with_plan_encoding() {
        let mut converter = converter();
        assert!(!converter.can_import(SourceKind::Text));
        assert_eq!(converter.options(), &ConversionOptions::default());

        let bytes = converter
            .export(b"ignored pdf bytes", ExportKind::Text)
            .expect("text export");
        assert_eq!(bytes, b"first\nsecond".to_vec());
        assert_eq!(
            converter.export_text_string().expect("text string"),
            "first\nsecond"
        );

        converter = TextConverter::new(
            DocumentMetadata::new("", "", 1),
            vec![page(0, "café")],
            TextExportPlan::new().with_encoding(TextEncoding::Utf16Be),
        );
        let encoded = converter
            .export(&[], ExportKind::Text)
            .expect("utf-16 text export");
        assert_eq!(&encoded[..2], &[0xFE, 0xFF]);
        assert_eq!(encoded, TextEncoding::Utf16Be.encode("café"));
    }

    #[test]
    fn text_converter_exports_schema_versioned_json_bytes() {
        let converter = converter();
        let bytes = converter
            .export(&[], ExportKind::Json)
            .expect("json export");
        let json = String::from_utf8(bytes).expect("JSON is UTF-8");
        assert!(json.starts_with("{\"json_schema_version\":1,"), "{json}");
        assert!(json.contains("\"title\":\"Title\""), "{json}");
        assert!(json.contains("\"author\":\"Author\""), "{json}");
        assert!(json.contains("\"pages\":2"), "{json}");
        assert!(json.contains("\"width\":2,\"height\":3"), "{json}");
        assert_eq!(json, converter.export_json_string().expect("json string"));
    }

    #[test]
    fn text_converter_errors_on_missing_page_text() {
        let converter = TextConverter::new(
            DocumentMetadata::new("", "", 2),
            vec![page(0, "only first")],
            TextExportPlan::new(),
        );
        for target in [ExportKind::Text, ExportKind::Json] {
            let error = converter
                .export(&[], target)
                .expect_err("missing page 1 must fail");
            assert!(matches!(error, ConversionError::InvalidInput { .. }));
            assert!(error.to_string().contains("page 1"), "{error}");
        }
        assert!(converter.export_text_string().is_err());
        assert!(converter.export_json_string().is_err());
    }

    #[test]
    fn text_converter_rejects_unsupported_targets_and_import() {
        let converter = converter();
        for &target in ExportKind::all() {
            if target == ExportKind::Text || target == ExportKind::Json {
                continue;
            }
            assert!(matches!(
                converter.export(&[], target),
                Err(ConversionError::UnsupportedExport { .. })
            ));
        }
        let error = converter
            .import(Path::new("report.txt"))
            .expect_err("export-only adapter must reject import");
        assert!(matches!(
            error,
            ConversionError::UnsupportedSource {
                kind: SourceKind::Text
            }
        ));
    }

    #[test]
    fn text_converter_validates_options_before_exporting() {
        let converter =
            converter().with_options(ConversionOptions::default().with_page_range(4, 2));
        for target in [ExportKind::Text, ExportKind::Json] {
            assert!(matches!(
                converter.export(&[], target),
                Err(ConversionError::InvalidInput { .. })
            ));
        }
    }

    #[test]
    fn text_converter_respects_page_selection_and_getters() {
        let converter = TextConverter::new(
            DocumentMetadata::new("T", "A", 3),
            vec![page(0, "a"), page(1, "b"), page(2, "c")],
            TextExportPlan::new().with_pages(TextPageSelection::Pages(vec![2, 0])),
        );
        assert_eq!(converter.metadata().pages, 3);
        assert_eq!(converter.pages().len(), 3);
        assert_eq!(
            converter.plan().pages(),
            &TextPageSelection::Pages(vec![2, 0])
        );
        assert_eq!(converter.export_text_string().expect("selection"), "c\na");
    }
}
