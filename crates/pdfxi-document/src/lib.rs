//! One document session for PDFXI: the current file bytes, the parsed
//! [`pdfxi_core::Document`], the open password, dirty state, undo/redo history,
//! mutations, and atomic save, as specified by ADR 0010
//! (`docs/decisions/0010-document-session-and-write-path.md`).
//!
//! The session owns the current file bytes, the parsed document, the open
//! password, the dirty flag, and the incremental undo/redo history. Mutations
//! are appended by [`pdfxi_core::Document::save_incremental`], undo truncates
//! back to the stored revision boundary, and saves are written through a
//! temporary file in the destination directory and renamed into place.
//!
//! Signing goes through [`DocumentSession::sign`], which calls
//! [`pdfxi_security::sign_pdf`] and records the signature as one more
//! incremental revision, so bytes signed earlier stay verifiable.
//!
//! # Round A limits
//!
//! - An encrypted source is decrypted on open with
//!   [`pdfxi_security::open_encrypted`], so reading, editing and undo work on
//!   the plaintext. Writing it back is refused with
//!   [`SessionError::EncryptedSaveUnsupported`] until re-encryption is wired.
//! - External-change detection compares length, last-write time, and a SHA-256
//!   of the first and last 4 KiB, not the whole file.
//! - `/Encrypt` detection scans the trailing 64 KiB for a name token, so a
//!   literal `/Encrypt` in a trailing stream payload is reported by the
//!   security layer as an invalid document rather than treated as plaintext.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod atomic;
mod error;
mod identity;
mod session;

pub use error::SessionError;
pub use session::{DocumentSession, DocumentSnapshot, MAX_REVISIONS};
