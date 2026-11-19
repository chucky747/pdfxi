//! The [`Converter`] trait and a deterministic in-memory test double.
//!
//! External tools such as LibreOffice or a headless browser are optional at
//! runtime and live behind this trait. [`MockConverter`] needs no external
//! process: it stores registered inputs in memory and produces deterministic
//! output, which makes routing and error behaviour testable without I/O.

use crate::ConversionError;
use crate::ConversionOptions;
use crate::ExportKind;
use crate::SourceKind;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// A format importer/exporter that can be swapped at runtime.
///
/// The interface follows `docs/ARCHITECTURE.md`, with one deliberate
/// simplification: a PDF document is passed as raw bytes rather than as a
/// `pdfxi_core::Document`. That keeps this crate dependency-free; an adapter
/// with a real `Document` can pass its serialized bytes instead.
///
/// [`ConversionOptions`] are exposed through [`Converter::options`] so the same
/// configuration is visible to both import and export. Implementations must
/// validate those options themselves; [`MockConverter`] does so in both
/// methods.
pub trait Converter {
    /// Returns the configuration applied to conversions.
    fn options(&self) -> &ConversionOptions;

    /// Returns `true` when this converter can import the given source kind.
    fn can_import(&self, kind: SourceKind) -> bool;

    /// Imports `input` and returns the resulting PDF bytes.
    ///
    /// Implementations should return [`ConversionError::UnsupportedSource`]
    /// for a kind they cannot handle, and should never panic on hostile paths.
    fn import(&self, input: &Path) -> Result<Vec<u8>, ConversionError>;

    /// Exports PDF `bytes` to `target` and returns the produced bytes.
    ///
    /// Implementations should return [`ConversionError::UnsupportedExport`]
    /// when the target is unavailable.
    fn export(&self, bytes: &[u8], target: ExportKind) -> Result<Vec<u8>, ConversionError>;
}

/// A deterministic, dependency-free [`Converter`] for tests and UI previews.
///
/// `MockConverter` registers input files by path and produces output bytes
/// prefixed with the target label. It never touches the file system and never
/// launches an external tool, so tests stay fast and reproducible.
///
/// # Examples
///
/// This example is rendered as text rather than executed as a doctest because
/// the build host denies execution of generated doctest binaries (os error 5).
/// The equivalent behaviour is verified by `converter::tests`.
///
/// ```text
/// use std::path::Path;
/// use pdfxi_convert::{Converter, ExportKind, MockConverter, SourceKind};
///
/// let converter = MockConverter::new().with_input("in.txt", b"hello".to_vec());
/// assert!(converter.can_import(SourceKind::Text));
/// let pdf = converter.import(Path::new("in.txt")).expect("import");
/// let text = converter.export(&pdf, ExportKind::Text).expect("export");
/// assert!(text.starts_with(b"mock:text:"));
/// ```
#[derive(Debug, Clone)]
pub struct MockConverter {
    options: ConversionOptions,
    sources: Vec<SourceKind>,
    exports: Vec<ExportKind>,
    inputs: BTreeMap<PathBuf, Vec<u8>>,
}

impl MockConverter {
    /// Creates a converter that supports all known source and export kinds.
    #[must_use]
    pub fn new() -> Self {
        Self {
            options: ConversionOptions::default(),
            sources: SourceKind::all().to_vec(),
            exports: ExportKind::all().to_vec(),
            inputs: BTreeMap::new(),
        }
    }

    /// Replaces the conversion options.
    #[must_use]
    pub fn with_options(mut self, options: ConversionOptions) -> Self {
        self.options = options;
        self
    }

    /// Replaces the set of importable source kinds.
    #[must_use]
    pub fn with_sources<I>(mut self, sources: I) -> Self
    where
        I: IntoIterator<Item = SourceKind>,
    {
        self.sources = sources.into_iter().collect();
        self
    }

    /// Replaces the set of producible export targets.
    #[must_use]
    pub fn with_exports<I>(mut self, exports: I) -> Self
    where
        I: IntoIterator<Item = ExportKind>,
    {
        self.exports = exports.into_iter().collect();
        self
    }

    /// Registers the bytes returned by [`Converter::import`] for `path`.
    #[must_use]
    pub fn with_input(mut self, path: impl AsRef<Path>, bytes: impl Into<Vec<u8>>) -> Self {
        self.inputs
            .insert(path.as_ref().to_path_buf(), bytes.into());
        self
    }

    /// Returns `true` when this converter can produce `target`.
    #[must_use]
    pub fn can_export(&self, target: ExportKind) -> bool {
        self.exports.contains(&target)
    }

    /// Returns the number of registered in-memory inputs.
    #[must_use]
    pub fn input_count(&self) -> usize {
        self.inputs.len()
    }
}

impl Default for MockConverter {
    fn default() -> Self {
        Self::new()
    }
}

impl Converter for MockConverter {
    fn options(&self) -> &ConversionOptions {
        &self.options
    }

    fn can_import(&self, kind: SourceKind) -> bool {
        self.sources.contains(&kind)
    }

    fn import(&self, input: &Path) -> Result<Vec<u8>, ConversionError> {
        self.options.validate()?;
        let kind = SourceKind::from_path(input);
        if !self.can_import(kind) {
            return Err(ConversionError::UnsupportedSource { kind });
        }
        match self.inputs.get(input) {
            Some(bytes) => Ok(bytes.clone()),
            None => Err(ConversionError::InvalidInput {
                message: format!("no mock input is registered for '{}'", input.display()),
            }),
        }
    }

    fn export(&self, bytes: &[u8], target: ExportKind) -> Result<Vec<u8>, ConversionError> {
        self.options.validate()?;
        if !self.can_export(target) {
            return Err(ConversionError::UnsupportedExport { target });
        }
        let mut output = Vec::with_capacity(bytes.len() + target.label().len() + 6);
        output.extend_from_slice(b"mock:");
        output.extend_from_slice(target.label().as_bytes());
        output.push(b':');
        output.extend_from_slice(bytes);
        Ok(output)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::Path;

    #[test]
    fn mock_import_returns_registered_bytes() {
        let converter = MockConverter::new().with_input("dir/in.txt", b"pdf bytes".to_vec());
        let imported = converter
            .import(Path::new("dir/in.txt"))
            .expect("registered input should import");
        assert_eq!(imported, b"pdf bytes".to_vec());
        assert_eq!(converter.input_count(), 1);
    }

    #[test]
    fn mock_can_import_respects_configured_sources() {
        let converter = MockConverter::new().with_sources([SourceKind::Text, SourceKind::Html]);
        assert!(converter.can_import(SourceKind::Text));
        assert!(converter.can_import(SourceKind::Html));
        assert!(!converter.can_import(SourceKind::Pdf));
        assert!(!converter.can_import(SourceKind::Unknown));
    }

    #[test]
    fn mock_import_rejects_unsupported_source() {
        let converter = MockConverter::new()
            .with_sources([SourceKind::Text])
            .with_input("scan.png", vec![1, 2, 3]);
        let error = converter
            .import(Path::new("scan.png"))
            .expect_err("images are not configured for import");
        assert!(matches!(
            error,
            ConversionError::UnsupportedSource {
                kind: SourceKind::Image
            }
        ));
        assert!(error.is_unsupported());
    }

    #[test]
    fn mock_import_without_registered_input_is_an_error() {
        let converter = MockConverter::new();
        let error = converter
            .import(Path::new("missing.pdf"))
            .expect_err("unregistered paths must fail");
        assert!(matches!(error, ConversionError::InvalidInput { .. }));
        assert!(error.to_string().contains("missing.pdf"));
    }

    #[test]
    fn mock_export_is_deterministic_and_respects_configured_targets() {
        let converter = MockConverter::new().with_exports([ExportKind::Text, ExportKind::Html]);
        let first = converter
            .export(b"doc", ExportKind::Text)
            .expect("text export should succeed");
        let second = converter
            .export(b"doc", ExportKind::Text)
            .expect("text export should succeed");
        assert_eq!(first, second);
        assert_eq!(first, b"mock:text:doc".to_vec());

        let error = converter
            .export(b"doc", ExportKind::Pdf)
            .expect_err("PDF export is not configured");
        assert!(matches!(
            error,
            ConversionError::UnsupportedExport {
                target: ExportKind::Pdf
            }
        ));
        assert!(converter.can_export(ExportKind::Html));
        assert!(!converter.can_export(ExportKind::Pdf));
    }

    #[test]
    fn mock_validates_options_before_converting() {
        let converter = MockConverter::new()
            .with_options(ConversionOptions::default().with_page_range(4, 2))
            .with_input("in.txt", vec![1]);
        assert!(matches!(
            converter.import(Path::new("in.txt")),
            Err(ConversionError::InvalidInput { .. })
        ));
        assert!(matches!(
            converter.export(&[1], ExportKind::Pdf),
            Err(ConversionError::InvalidInput { .. })
        ));
    }

    #[test]
    fn default_mock_supports_all_known_kinds() {
        let converter = MockConverter::default();
        for kind in SourceKind::all() {
            assert!(converter.can_import(*kind));
        }
        for target in ExportKind::all() {
            assert!(converter.can_export(*target));
        }
        assert_eq!(converter.options(), &ConversionOptions::default());
    }
}
