//! The document session: bytes, parsed document, mutations, undo/redo, save.

use std::collections::VecDeque;
use std::fmt;
use std::fs;
use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_security::crypto::sha::sha256;
use pdfxi_security::{DecryptionReport, SignOptions, SignatureReport, SigningIdentity};

use crate::atomic;
use crate::error::SessionError;
use crate::identity::{self, FileIdentity};

/// Maximum number of incremental revisions kept for undo.
///
/// Older revisions are discarded from the front of the history; the session
/// reports that through [`DocumentSession::dropped_oldest_revision`]. The file
/// bytes are unaffected, only the ability to undo beyond the retained window is
/// lost.
pub const MAX_REVISIONS: usize = 50;

/// How many trailing bytes are scanned for an `/Encrypt` name token.
const ENCRYPTION_SCAN_BYTES: usize = 64 * 1024;

/// The name token that marks an encrypted document in its newest trailer.
const ENCRYPT_KEY: &[u8] = b"/Encrypt";

/// One incremental revision: where it starts, what it appended, and the hashes
/// of the bytes on either side.
struct Revision {
    /// Session byte length before the append.
    start_len: usize,
    /// Bytes appended by this revision.
    delta: Vec<u8>,
    /// SHA-256 of the bytes before the append.
    before: [u8; 32],
    /// SHA-256 of the bytes after the append.
    after: [u8; 32],
}

/// One open document: its bytes, parsed form, history, and save destination.
///
/// A session is the single owner described by ADR 0010. Open it with
/// [`DocumentSession::open`] or [`DocumentSession::open_with_password`]; mutate
/// it only through [`DocumentSession::edit`] (incremental append),
/// [`DocumentSession::sign`] (incremental signature revision) or
/// [`DocumentSession::edit_full`] (whole-file rewrite); undo and redo walk the
/// incremental revisions; and save with [`DocumentSession::save`],
/// [`DocumentSession::save_forced`] or [`DocumentSession::save_as`].
///
/// The session keeps the decrypted plaintext for an encrypted source so reading
/// and editing work, but refuses to write it back until re-encryption is wired
/// (see [`SessionError::EncryptedSaveUnsupported`]).
pub struct DocumentSession {
    document: Document,
    bytes: Vec<u8>,
    path: Option<PathBuf>,
    password: Option<Vec<u8>>,
    decryption: Option<DecryptionReport>,
    identity: Option<FileIdentity>,
    current_digest: [u8; 32],
    saved_digest: [u8; 32],
    undo: VecDeque<Revision>,
    redo: Vec<Revision>,
    lineage_reset: bool,
    dropped_oldest: bool,
}

impl fmt::Debug for DocumentSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DocumentSession")
            .field("path", &self.path)
            .field("bytes", &self.bytes.len())
            .field("password", &self.password.as_ref().map(|_| "<redacted>"))
            .field("encrypted", &self.is_encrypted())
            .field("dirty", &self.is_dirty())
            .field("revisions", &self.undo.len())
            .field("redo", &self.redo.len())
            .field("dropped_oldest", &self.dropped_oldest)
            .finish()
    }
}

impl DocumentSession {
    /// Open the PDF at `path` without a password.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PasswordRequired`] when the file is encrypted,
    /// [`SessionError::Core`] when the header is not a PDF, and
    /// [`SessionError::Io`] when the file cannot be read.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, SessionError> {
        Self::open_with_password(path, None)
    }

    /// Open the PDF at `path`, decrypting it with `password` when required.
    ///
    /// A password supplied for an unencrypted file is ignored. On success the
    /// session bytes are the decrypted plaintext and
    /// [`DocumentSession::decryption_report`] describes the security handler.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PasswordRequired`] when the file is encrypted
    /// and no password was given, [`SessionError::WrongPassword`] when the
    /// password does not authenticate, and the core, security, or I/O error for
    /// any other failure.
    pub fn open_with_password(
        path: impl AsRef<Path>,
        password: Option<&[u8]>,
    ) -> Result<Self, SessionError> {
        let path = path.as_ref();
        let bytes = fs::read(path)?;
        let mut session = Self::from_source(bytes, Some(path.to_path_buf()), password)?;
        session.identity = Some(identity::capture(path)?);
        Ok(session)
    }

    /// Open a PDF held entirely in memory.
    ///
    /// The session has no path, so [`DocumentSession::save`] returns
    /// [`SessionError::NoPath`]; use [`DocumentSession::save_as`] to write it.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PasswordRequired`] when the bytes are encrypted,
    /// plus the core or security error for a malformed or undecryptable file.
    pub fn open_bytes(bytes: &[u8]) -> Result<Self, SessionError> {
        Self::open_bytes_with_password(bytes, None)
    }

    /// Open an in-memory PDF, decrypting it with `password` when required.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::PasswordRequired`] when the bytes are encrypted
    /// and no password was given, [`SessionError::WrongPassword`] when the
    /// password does not authenticate, and the core or security error for any
    /// other failure.
    pub fn open_bytes_with_password(
        bytes: &[u8],
        password: Option<&[u8]>,
    ) -> Result<Self, SessionError> {
        Self::from_source(bytes.to_vec(), None, password)
    }

    /// Build a session from raw source bytes.
    fn from_source(
        source: Vec<u8>,
        path: Option<PathBuf>,
        password: Option<&[u8]>,
    ) -> Result<Self, SessionError> {
        let encrypted = has_encrypt_token(&source);
        let (document, decryption) = if encrypted {
            let supplied = password.ok_or(SessionError::PasswordRequired)?;
            match pdfxi_security::open_encrypted(&source, Some(supplied)) {
                Ok((document, report)) => (document, Some(report)),
                Err(pdfxi_security::SecurityError::WrongPassword) => {
                    return Err(SessionError::WrongPassword);
                }
                Err(error) => return Err(SessionError::Security(error)),
            }
        } else {
            (Document::from_bytes(source)?, None)
        };
        let bytes = document.bytes().to_vec();
        let digest = sha256(&bytes);
        Ok(Self {
            document,
            bytes,
            path,
            password: password.map(<[u8]>::to_vec),
            decryption,
            identity: None,
            current_digest: digest,
            saved_digest: digest,
            undo: VecDeque::new(),
            redo: Vec::new(),
            lineage_reset: false,
            dropped_oldest: false,
        })
    }

    /// Return the parsed document for reading.
    pub fn document(&self) -> &Document {
        &self.document
    }

    /// Return the current file bytes.
    ///
    /// For an encrypted source this is the decrypted plaintext, matching what
    /// the parser and renderer see.
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }

    /// Return the destination path, when the session was opened from a file.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Return `true` when the current bytes differ from the last loaded or
    /// saved bytes.
    pub fn is_dirty(&self) -> bool {
        self.current_digest != self.saved_digest
    }

    /// Return the number of revisions currently available for undo.
    pub fn revision_count(&self) -> usize {
        self.undo.len()
    }

    /// Return `true` when an incremental revision can be undone.
    pub fn can_undo(&self) -> bool {
        !self.undo.is_empty()
    }

    /// Return `true` when a previous undo can be reapplied.
    pub fn can_redo(&self) -> bool {
        !self.redo.is_empty()
    }

    /// Return `true` when the revision cap discarded an older revision.
    ///
    /// The flag is sticky for the life of the session.
    pub fn dropped_oldest_revision(&self) -> bool {
        self.dropped_oldest
    }

    /// Return the decryption report for an encrypted source, if any.
    pub fn decryption_report(&self) -> Option<DecryptionReport> {
        self.decryption
    }

    /// Return `true` when the session was opened with a password.
    pub fn is_encrypted(&self) -> bool {
        self.decryption.is_some()
    }

    /// Return a read-only view of the current session state.
    pub fn snapshot(&self) -> DocumentSnapshot<'_> {
        DocumentSnapshot { session: self }
    }

    /// Apply `mutation` and append the result as an incremental revision.
    ///
    /// The mutation runs against the parsed document. Its staged changes are
    /// written by [`pdfxi_core::Document::save_incremental`], so every byte
    /// before this revision is preserved and an earlier signature remains
    /// verifiable. On a mutation or save error the document is reloaded from the
    /// unchanged session bytes and no revision is recorded.
    ///
    /// # Errors
    ///
    /// Returns the mutation's error, [`SessionError::Core`] when the append
    /// fails, or [`SessionError::RevisionMismatch`] if the writer did not
    /// produce the expected prefix.
    pub fn edit<F, T>(&mut self, mutation: F) -> Result<T, SessionError>
    where
        F: FnOnce(&mut Document) -> Result<T, SessionError>,
    {
        let start_len = self.bytes.len();
        let before = self.current_digest;
        let value = match mutation(&mut self.document) {
            Ok(value) => value,
            Err(error) => {
                self.reload_unchanged()?;
                return Err(error);
            }
        };
        let appended = match self.document.save_incremental() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.reload_unchanged()?;
                return Err(SessionError::Core(error));
            }
        };
        let delta = match appended.get(start_len..) {
            Some(delta)
                if !delta.is_empty()
                    && appended.get(..start_len) == Some(self.bytes.as_slice()) =>
            {
                delta.to_vec()
            }
            _ => {
                self.reload_unchanged()?;
                return Err(SessionError::RevisionMismatch);
            }
        };
        let after = sha256(&appended);
        let document = Document::from_bytes(appended.clone())?;
        self.bytes = appended;
        self.document = document;
        self.current_digest = after;
        self.push_revision(Revision {
            start_len,
            delta,
            before,
            after,
        });
        self.redo.clear();
        Ok(value)
    }

    /// Sign the document with one incremental signature revision.
    ///
    /// Calls [`pdfxi_security::sign_pdf`] on the current parsed document and
    /// records the result exactly like [`DocumentSession::edit`]: the bytes
    /// before the signature stay a byte-for-byte prefix, the appended revision
    /// becomes the new session bytes, the parsed document is reloaded from the
    /// signed bytes, the revision is pushed with its before/after digests, and
    /// the redo stack is cleared. The whole-file rewrite path
    /// ([`DocumentSession::edit_full`]) is deliberately not used, because it
    /// would invalidate a signature applied earlier in the history (ADR 0010,
    /// point 3).
    ///
    /// The returned [`SignatureReport`] describes the signature that was
    /// written, including the `/ByteRange` and the signer subject.
    /// [`DocumentSession::bytes`] is the authoritative signed file; persist it
    /// with [`DocumentSession::save`] or [`DocumentSession::save_as`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EncryptedSignUnsupported`] for an encrypted
    /// source, because the signature would cover the decrypted plaintext rather
    /// than the bytes on disk; [`SessionError::Security`] when
    /// [`pdfxi_security::sign_pdf`] rejects the page, the `/AcroForm`, the
    /// identity, or the `/Contents` reservation; [`SessionError::Core`] when
    /// the signed bytes cannot be reparsed; and [`SessionError::RevisionMismatch`]
    /// when the security layer's output does not begin with the unchanged
    /// session bytes. On any error the previous bytes, parsed document and
    /// history are restored.
    pub fn sign(
        &mut self,
        identity: &SigningIdentity,
        options: &SignOptions,
    ) -> Result<SignatureReport, SessionError> {
        if self.is_encrypted() {
            return Err(SessionError::EncryptedSignUnsupported);
        }
        let start_len = self.bytes.len();
        let before = self.current_digest;
        let signed = match pdfxi_security::sign_pdf(&mut self.document, identity, options) {
            Ok(signed) => signed,
            Err(error) => {
                self.reload_unchanged()?;
                return Err(SessionError::Security(error));
            }
        };
        let report = signed.report().clone();
        let appended = signed.into_bytes();
        let delta = match appended.get(start_len..) {
            Some(delta)
                if !delta.is_empty()
                    && appended.get(..start_len) == Some(self.bytes.as_slice()) =>
            {
                delta.to_vec()
            }
            _ => {
                self.reload_unchanged()?;
                return Err(SessionError::RevisionMismatch);
            }
        };
        let after = sha256(&appended);
        let document = match Document::from_bytes(appended.clone()) {
            Ok(document) => document,
            Err(error) => {
                self.reload_unchanged()?;
                return Err(SessionError::Core(error));
            }
        };
        self.bytes = appended;
        self.document = document;
        self.current_digest = after;
        self.push_revision(Revision {
            start_len,
            delta,
            before,
            after,
        });
        self.redo.clear();
        Ok(report)
    }

    /// Apply `mutation` and rewrite the whole file, discarding history.
    ///
    /// Use this only for operations whose earlier bytes must not survive, such
    /// as redaction, sanitization or optimization (ADR 0010, point 5). The
    /// resulting bytes are a single-revision file, every earlier revision is
    /// dropped, and a later [`DocumentSession::undo`] returns
    /// [`SessionError::UndoAcrossRewrite`] at the rewrite boundary.
    ///
    /// # Errors
    ///
    /// Returns the mutation's error or [`SessionError::Core`] when the rewrite
    /// fails; in both cases the previous bytes and history are left intact.
    pub fn edit_full<F>(&mut self, mutation: F) -> Result<(), SessionError>
    where
        F: FnOnce(&mut Document) -> Result<(), SessionError>,
    {
        if let Err(error) = mutation(&mut self.document) {
            self.reload_unchanged()?;
            return Err(error);
        }
        let rewritten = match self.document.save() {
            Ok(bytes) => bytes,
            Err(error) => {
                self.reload_unchanged()?;
                return Err(SessionError::Core(error));
            }
        };
        let digest = sha256(&rewritten);
        let document = Document::from_bytes(rewritten.clone())?;
        self.bytes = rewritten;
        self.document = document;
        self.current_digest = digest;
        self.undo.clear();
        self.redo.clear();
        self.lineage_reset = true;
        Ok(())
    }

    /// Undo the newest incremental revision by truncating to its boundary.
    ///
    /// The stored prefix hash is verified before the truncation, so a corrupted
    /// history is reported rather than applied. The undone revision is kept for
    /// [`DocumentSession::redo`].
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NothingToUndo`] when no revision is left,
    /// [`SessionError::UndoAcrossRewrite`] when the next step back would cross a
    /// whole-file rewrite, or [`SessionError::RevisionMismatch`] when the
    /// current bytes do not match the recorded boundary.
    pub fn undo(&mut self) -> Result<(), SessionError> {
        let Some(revision) = self.undo.pop_back() else {
            return Err(if self.lineage_reset {
                SessionError::UndoAcrossRewrite
            } else {
                SessionError::NothingToUndo
            });
        };
        if self.bytes.len() < revision.start_len
            || sha256(&self.bytes[..revision.start_len]) != revision.before
        {
            self.undo.push_back(revision);
            return Err(SessionError::RevisionMismatch);
        }
        let truncated = self.bytes[..revision.start_len].to_vec();
        match Document::from_bytes(truncated.clone()) {
            Ok(document) => {
                self.bytes = truncated;
                self.document = document;
                self.current_digest = revision.before;
                self.redo.push(revision);
                Ok(())
            }
            Err(error) => {
                self.undo.push_back(revision);
                Err(SessionError::Core(error))
            }
        }
    }

    /// Reapply the most recently undone revision by appending its stored delta.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::NothingToRedo`] when no undone revision exists,
    /// or [`SessionError::RevisionMismatch`] when the current bytes are not the
    /// revision's recorded starting point.
    pub fn redo(&mut self) -> Result<(), SessionError> {
        let Some(revision) = self.redo.pop() else {
            return Err(SessionError::NothingToRedo);
        };
        if self.bytes.len() != revision.start_len || sha256(&self.bytes) != revision.before {
            self.redo.push(revision);
            return Err(SessionError::RevisionMismatch);
        }
        let mut restored = self.bytes.clone();
        restored.extend_from_slice(&revision.delta);
        match Document::from_bytes(restored.clone()) {
            Ok(document) => {
                self.bytes = restored;
                self.document = document;
                self.current_digest = revision.after;
                self.push_revision(revision);
                Ok(())
            }
            Err(error) => {
                self.redo.push(revision);
                Err(SessionError::Core(error))
            }
        }
    }

    /// Save the current bytes to the session path.
    ///
    /// The write is atomic: a sibling temporary file is flushed and renamed over
    /// the destination. Before writing, the on-disk identity is compared with
    /// the identity captured at load or the previous save.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EncryptedSaveUnsupported`] for an encrypted
    /// source, [`SessionError::NoPath`] for an in-memory session,
    /// [`SessionError::ExternallyModified`] when the file changed on disk, and
    /// [`SessionError::Io`] for a failed write or rename.
    pub fn save(&mut self) -> Result<(), SessionError> {
        self.save_impl(false)
    }

    /// Save the current bytes to the session path, ignoring an external change.
    ///
    /// This overwrites the file even when its identity no longer matches. Use it
    /// only after the user has explicitly chosen to overwrite; the normal path
    /// is [`DocumentSession::save`] or [`DocumentSession::save_as`].
    ///
    /// # Errors
    ///
    /// Returns the same errors as [`DocumentSession::save`] except
    /// [`SessionError::ExternallyModified`].
    pub fn save_forced(&mut self) -> Result<(), SessionError> {
        self.save_impl(true)
    }

    /// Save the current bytes to `path`, atomically.
    ///
    /// Save As does not check the source identity because the destination is a
    /// new location; it does not modify the previously opened file. The session
    /// path becomes `path` and the external identity is recaptured.
    ///
    /// # Errors
    ///
    /// Returns [`SessionError::EncryptedSaveUnsupported`] for an encrypted
    /// source and [`SessionError::Io`] for a failed write or rename.
    pub fn save_as(&mut self, path: impl AsRef<Path>) -> Result<(), SessionError> {
        if self.is_encrypted() {
            return Err(SessionError::EncryptedSaveUnsupported);
        }
        let path = path.as_ref();
        atomic::write(path, &self.bytes)?;
        self.identity = Some(identity::capture(path)?);
        self.path = Some(path.to_path_buf());
        self.saved_digest = self.current_digest;
        Ok(())
    }

    /// Save to the session path with an optional external-change override.
    fn save_impl(&mut self, force: bool) -> Result<(), SessionError> {
        if self.is_encrypted() {
            return Err(SessionError::EncryptedSaveUnsupported);
        }
        let path = self.path.clone().ok_or(SessionError::NoPath)?;
        if !force {
            let recorded = self
                .identity
                .ok_or_else(|| SessionError::ExternallyModified { path: path.clone() })?;
            let current = match identity::capture(&path) {
                Ok(current) => current,
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                    return Err(SessionError::ExternallyModified { path });
                }
                Err(error) => return Err(SessionError::Io(error)),
            };
            if current != recorded {
                return Err(SessionError::ExternallyModified { path });
            }
        }
        atomic::write(&path, &self.bytes)?;
        self.identity = Some(identity::capture(&path)?);
        self.saved_digest = self.current_digest;
        Ok(())
    }

    /// Reload the parsed document from the unchanged session bytes.
    fn reload_unchanged(&mut self) -> Result<(), SessionError> {
        self.document = Document::from_bytes(self.bytes.clone())?;
        Ok(())
    }

    /// Record a revision and enforce [`MAX_REVISIONS`].
    fn push_revision(&mut self, revision: Revision) {
        self.undo.push_back(revision);
        while self.undo.len() > MAX_REVISIONS {
            self.undo.pop_front();
            self.dropped_oldest = true;
        }
    }
}

/// A read-only view of a [`DocumentSession`] at one moment.
///
/// The snapshot borrows the session; it never copies the document bytes.
#[derive(Debug, Clone, Copy)]
pub struct DocumentSnapshot<'a> {
    session: &'a DocumentSession,
}

impl<'a> DocumentSnapshot<'a> {
    /// Return the parsed document.
    pub fn document(&self) -> &Document {
        self.session.document()
    }

    /// Return the current decrypted file bytes.
    pub fn bytes(&self) -> &[u8] {
        self.session.bytes()
    }

    /// Return the destination path, when the session was opened from a file.
    pub fn path(&self) -> Option<&Path> {
        self.session.path()
    }

    /// Return `true` when the current bytes differ from the saved bytes.
    pub fn is_dirty(&self) -> bool {
        self.session.is_dirty()
    }

    /// Return the number of revisions available for undo.
    pub fn revision_count(&self) -> usize {
        self.session.revision_count()
    }

    /// Return `true` when an incremental revision can be undone.
    pub fn can_undo(&self) -> bool {
        self.session.can_undo()
    }

    /// Return `true` when a previous undo can be reapplied.
    pub fn can_redo(&self) -> bool {
        self.session.can_redo()
    }

    /// Return `true` when the revision cap discarded an older revision.
    pub fn dropped_oldest_revision(&self) -> bool {
        self.session.dropped_oldest_revision()
    }

    /// Return the decryption report for an encrypted source, if any.
    pub fn decryption_report(&self) -> Option<DecryptionReport> {
        self.session.decryption_report()
    }
}

/// Return `true` when the trailing bytes contain an `/Encrypt` name token.
///
/// Only the last [`ENCRYPTION_SCAN_BYTES`] are inspected because that is where
/// the newest trailer lives. The token must not be followed by an ASCII
/// alphanumeric character: that keeps `/EncryptMetadata` (a key inside the
/// encryption dictionary) from being mistaken for the trailer's `/Encrypt`
/// reference. A literal `/Encrypt` inside a stream payload in the final 64 KiB
/// can still produce a false positive; the security layer then reports a
/// missing encryption dictionary instead of silently treating the file as
/// plaintext.
fn has_encrypt_token(bytes: &[u8]) -> bool {
    let start = bytes.len().saturating_sub(ENCRYPTION_SCAN_BYTES);
    let window = &bytes[start..];
    window
        .windows(ENCRYPT_KEY.len())
        .enumerate()
        .any(|(index, candidate)| {
            candidate == ENCRYPT_KEY
                && !window
                    .get(index + ENCRYPT_KEY.len())
                    .is_some_and(u8::is_ascii_alphanumeric)
        })
}

#[cfg(test)]
mod tests {
    use super::has_encrypt_token;

    #[test]
    fn encrypt_detection_ignores_the_metadata_key() {
        assert!(!has_encrypt_token(
            b"%PDF-1.7\ntrailer\n<< /EncryptMetadata false >>"
        ));
        assert!(!has_encrypt_token(b"%PDF-1.7\nsome stream bytes"));
    }

    #[test]
    fn encrypt_detection_finds_the_trailer_reference() {
        assert!(has_encrypt_token(
            b"%PDF-1.7\ntrailer\n<< /Size 5 /Encrypt 4 0 R >>"
        ));
        assert!(has_encrypt_token(b"/Encrypt"));
        assert!(has_encrypt_token(b"/Encrypt\n4 0 R"));
    }
}
