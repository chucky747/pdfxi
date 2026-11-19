//! Converter configuration shared by import and export adapters.
//!
//! [`ConversionOptions`] is a small, dependency-free value object. Real
//! adapters read it from [`crate::Converter::options`]; the mock converter in
//! this crate also validates it before doing work. `ConversionOptions::validate`
//! catches malformed page ranges and empty OCR languages before an external
//! tool is invoked.

use crate::ConversionError;

/// Configuration applied by a [`crate::Converter`] during import and export.
///
/// Defaults are conservative: every page is processed, raster images are kept,
/// annotations are preserved, and OCR is disabled. Build a modified value with
/// the `with_*` methods and pass it to a converter at construction time.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ConversionOptions {
    page_range: Option<(usize, usize)>,
    embed_images: bool,
    flatten_annotations: bool,
    ocr_language: Option<String>,
}

impl ConversionOptions {
    /// Limits processing to the inclusive, one-based page range
    /// `first..=last`.
    #[must_use]
    pub fn with_page_range(mut self, first: usize, last: usize) -> Self {
        self.page_range = Some((first, last));
        self
    }

    /// Enables or disables embedding raster images in the output.
    #[must_use]
    pub fn with_embed_images(mut self, enabled: bool) -> Self {
        self.embed_images = enabled;
        self
    }

    /// Enables or disables flattening annotations into page content.
    #[must_use]
    pub fn with_flatten_annotations(mut self, enabled: bool) -> Self {
        self.flatten_annotations = enabled;
        self
    }

    /// Sets an OCR language tag, for example `eng`.
    #[must_use]
    pub fn with_ocr_language(mut self, language: impl Into<String>) -> Self {
        self.ocr_language = Some(language.into());
        self
    }

    /// Returns the configured one-based page range, if any.
    #[must_use]
    pub fn page_range(&self) -> Option<(usize, usize)> {
        self.page_range
    }

    /// Returns whether raster images should be embedded.
    #[must_use]
    pub fn embed_images(&self) -> bool {
        self.embed_images
    }

    /// Returns whether annotations should be flattened.
    #[must_use]
    pub fn flatten_annotations(&self) -> bool {
        self.flatten_annotations
    }

    /// Returns the configured OCR language tag, if any.
    #[must_use]
    pub fn ocr_language(&self) -> Option<&str> {
        self.ocr_language.as_deref()
    }

    /// Checks the options for malformed values.
    ///
    /// A page range must start at page 1 or later and its last page must not
    /// precede its first page. An OCR language, when present, must not be
    /// empty or whitespace-only.
    pub fn validate(&self) -> Result<(), ConversionError> {
        if let Some((first, last)) = self.page_range {
            if first == 0 {
                return Err(ConversionError::InvalidInput {
                    message: "page ranges are one-based; the first page cannot be 0".to_owned(),
                });
            }
            if last < first {
                return Err(ConversionError::InvalidInput {
                    message: format!(
                        "page range ends at page {last} before it starts at page {first}"
                    ),
                });
            }
        }
        if let Some(language) = &self.ocr_language
            && language.trim().is_empty()
        {
            return Err(ConversionError::InvalidInput {
                message: "OCR language must not be empty".to_owned(),
            });
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_options_are_conservative_and_valid() {
        let options = ConversionOptions::default();
        assert_eq!(options.page_range(), None);
        assert!(!options.embed_images());
        assert!(!options.flatten_annotations());
        assert_eq!(options.ocr_language(), None);
        assert!(options.validate().is_ok());
    }

    #[test]
    fn builders_store_every_option() {
        let options = ConversionOptions::default()
            .with_page_range(2, 5)
            .with_embed_images(true)
            .with_flatten_annotations(true)
            .with_ocr_language("eng");
        assert_eq!(options.page_range(), Some((2, 5)));
        assert!(options.embed_images());
        assert!(options.flatten_annotations());
        assert_eq!(options.ocr_language(), Some("eng"));
        assert!(options.validate().is_ok());
    }

    #[test]
    fn validate_rejects_zero_first_page() {
        let error = ConversionOptions::default()
            .with_page_range(0, 3)
            .validate()
            .expect_err("a zero first page must be rejected");
        assert!(matches!(error, ConversionError::InvalidInput { .. }));
        assert!(error.to_string().contains("one-based"));
    }

    #[test]
    fn validate_rejects_reversed_page_range() {
        let error = ConversionOptions::default()
            .with_page_range(7, 4)
            .validate()
            .expect_err("a reversed page range must be rejected");
        assert!(error.to_string().contains("ends at page 4"));
    }

    #[test]
    fn validate_rejects_blank_ocr_language() {
        let error = ConversionOptions::default()
            .with_ocr_language("   ")
            .validate()
            .expect_err("a blank OCR language must be rejected");
        assert!(error.to_string().contains("OCR language"));
    }
}
