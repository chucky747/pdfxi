//! Typed errors returned by the document session.

use std::path::PathBuf;

/// Errors returned by [`crate::DocumentSession`].
///
/// The variants are deliberately distinct so a caller can drive UI without
/// matching on strings: a locked document asks for a password, a stale file
/// offers "save a copy", and a rewrite boundary explains why undo stops.
#[derive(Debug, thiserror::Error)]
pub enum SessionError {
    /// The document carries an `/Encrypt` entry and no open password was given.
    #[error("the document is encrypted; an open password is required")]
    PasswordRequired,
    /// The supplied open password did not authenticate against the document.
    #[error("the supplied password does not open the document")]
    WrongPassword,
    /// The document was opened with a password, and re-encrypting on save is not
    /// wired in this round.
    ///
    /// The decrypted plaintext stays available in memory for viewing, editing
    /// and undo; only writing it back to disk is refused. Saving a decrypted
    /// copy as plaintext would silently remove protection, so it is not offered
    /// here (see ADR 0010, point 5).
    #[error("saving an encrypted document is not supported yet")]
    EncryptedSaveUnsupported,
    /// The session was opened from memory and has no destination path.
    #[error("the session has no file path; use save_as")]
    NoPath,
    /// The file on disk no longer matches the identity captured at load or save.
    ///
    /// Saving would overwrite another program's change. The caller can offer
    /// "save a copy" through [`crate::DocumentSession::save_as`] or force the
    /// write through [`crate::DocumentSession::save_forced`].
    #[error("the file changed on disk since it was loaded or last saved: {path}")]
    ExternallyModified {
        /// The destination path whose identity no longer matches.
        path: PathBuf,
    },
    /// The requested undo crosses a full rewrite, where earlier bytes no longer
    /// exist and cannot be restored.
    #[error("the operation rewrote the whole file, so earlier revisions cannot be restored")]
    UndoAcrossRewrite,
    /// There is no incremental revision left to undo.
    #[error("there is no revision to undo")]
    NothingToUndo,
    /// There is no undone revision left to redo.
    #[error("there is no revision to redo")]
    NothingToRedo,
    /// A stored revision does not match the bytes it claims to describe.
    ///
    /// This is an internal integrity check; a mismatch means the session state
    /// was corrupted rather than that the file changed on disk.
    #[error("stored revision does not match the current bytes")]
    RevisionMismatch,
    /// Reading or writing the document failed.
    #[error("document I/O failed: {0}")]
    Io(#[from] std::io::Error),
    /// The core parser or writer rejected the document.
    #[error(transparent)]
    Core(#[from] pdfxi_core::Error),
    /// The security layer rejected the document or its password.
    #[error(transparent)]
    Security(#[from] pdfxi_security::SecurityError),
}
