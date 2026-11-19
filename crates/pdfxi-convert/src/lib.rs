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
mod options;
mod source;

pub use converter::{Converter, MockConverter};
pub use error::ConversionError;
pub use export::ExportKind;
pub use options::ConversionOptions;
pub use source::SourceKind;

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-convert"
}
