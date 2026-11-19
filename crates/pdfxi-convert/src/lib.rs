//! Import and export converters for images, text, HTML, Office formats, and print capture.
//!
//! This crate is scaffolded in milestone M0. The public surface is kept small
//! while the contracts in `docs/ARCHITECTURE.md` are frozen.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Return the implementation name for diagnostics.
pub fn engine_name() -> &'static str {
    "pdfxi-convert"
}
