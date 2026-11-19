//! Import and export converters for images, text, HTML, Office formats, and print capture.
//!
//! This crate freezes the conversion contracts described in
//! `docs/ARCHITECTURE.md` without pulling in an external converter:
//!
//! - [`SourceKind`] and [`ExportKind`] classify file names into adapter
//!   families. Classification is pure and never touches the file system.
//! - [`ConversionOptions`] carries page range, image, annotation, and OCR
//!   settings shared by import and export.
//! - [`ConversionError`] reports unsupported formats and invalid input with
//!   actionable messages.
//! - [`Converter`] is the trait implemented by optional external adapters;
//!   [`MockConverter`] is a deterministic in-memory implementation for tests.
//! - [`encode_png`], [`encode_ppm`], and [`encode`] are pure-Rust image encoders
//!   that need no image crate. They are always available, even with
//!   `--no-default-features`.
//! - [`export_pages`], [`ImageExportPlan`], [`PageSelection`], and
//!   [`ExportedImage`] export selected pages through any renderer. They are
//!   gated behind the default `renderer` feature so the crate can be used
//!   dependency-free by callers that only need classification or encoding.
//!   The `pdfium` feature adds `export_pdf_bytes` and `export_pdf_file`,
//!   which construct a `PdfiumRenderer` for convenience; the core API stays
//!   renderer-agnostic.
//! - [`export_text`], [`TextExportPlan`], [`TextPageSelection`], and
//!   [`TextEncoding`] turn pre-extracted `(page, text)` pairs into deterministic
//!   plain text with documented separators, headers, and trailing-newline
//!   rules.
//! - [`DocumentJson`], [`DocumentMetadata`], [`JsonPage`], and [`JsonSpan`]
//!   model a versioned structured snapshot serialized by a hand-written,
//!   dependency-free JSON writer with stable key order and correct escaping.
//! - [`TextConverter`] is a [`Converter`] adapter that exports pre-extracted
//!   page data as [`ExportKind::Text`] or [`ExportKind::Json`], keeping parsing
//!   and rendering out of the export path.
//!
//! The text/JSON modules are always compiled and add no dependencies, so a
//! `--no-default-features` build stays dependency-free.
//!
//! PDF documents cross the trait boundary as raw bytes. A caller holding a
//! `pdfxi_core::Document` can pass its serialized bytes, which keeps this crate
//! free of workspace and third-party dependencies (veraPDF and LibreOffice are
//! never run here).
//!
//! # Examples
//!
//! The example below is rendered as text rather than compiled as a doctest:
//! the build host denies execution of generated doctest binaries (os error 5).
//! The same behaviour is covered by executable unit tests in
//! `converter::tests`.
//!
//! ```text
//! use std::path::Path;
//! use pdfxi_convert::{Converter, ExportKind, MockConverter, SourceKind};
//!
//! let converter = MockConverter::new().with_input("report.txt", b"pdf".to_vec());
//! assert!(converter.can_import(SourceKind::Text));
//! let pdf = converter.import(Path::new("report.txt")).expect("import");
//! let text = converter.export(&pdf, ExportKind::Text).expect("export");
//! assert!(text.starts_with(b"mock:text:"));
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod converter;
mod error;
mod export;
pub mod image;
mod json;
mod options;
#[cfg(feature = "renderer")]
mod page_export;
mod source;
mod text;
mod text_converter;

pub use converter::{Converter, MockConverter};
pub use error::ConversionError;
pub use export::ExportKind;
pub use image::{ImageFormat, encode, encode_png, encode_ppm};
pub use json::{DocumentJson, DocumentMetadata, JSON_SCHEMA_VERSION, JsonPage, JsonSpan};
pub use options::ConversionOptions;
#[cfg(feature = "renderer")]
pub use page_export::{
    ExportedImage, ImageExportPlan, PageSelection, RenderResolution, export_pages,
};
#[cfg(feature = "pdfium")]
pub use page_export::{export_pdf_bytes, export_pdf_file};
pub use source::SourceKind;
pub use text::{
    DEFAULT_PAGE_SEPARATOR, PAGE_HEADER_PATTERN, TextEncoding, TextExportPlan, TextPageSelection,
    export_text,
};
pub use text_converter::TextConverter;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-convert"
}
