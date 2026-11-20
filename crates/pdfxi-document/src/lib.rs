//! One document session for PDFXI: the current file bytes, the parsed
//! [`pdfxi_core::Document`], the open password, dirty state, undo/redo history,
//! mutations, and atomic save, as specified by ADR 0010
//! (`docs/decisions/0010-document-session-and-write-path.md`).
//!
//! This crate is a scaffold only. The session type, the mutation and undo/redo
//! model, external-change detection, and the atomic save path are not
//! implemented yet; task T-0214 fills in this body.

#![deny(missing_docs)]
#![forbid(unsafe_code)]
