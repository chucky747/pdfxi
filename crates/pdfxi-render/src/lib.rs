//! Rendering abstraction, PDFium backend, display lists, tiles, fonts, and color.
//!
//! This crate is scaffolded in milestone M0. The public surface is kept small
//! while the contracts in `docs/ARCHITECTURE.md` are frozen.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

/// Return the implementation name for diagnostics.
pub fn backend_name() -> &'static str {
    "pdfium"
}
