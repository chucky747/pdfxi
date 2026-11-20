//! Session behaviour against the real-world fixtures: decryption, incremental
//! edits, undo/redo, atomic save, external-change detection, rewrite
//! boundaries, and the revision cap.
//!
//! Every test uses a real PDF from `fixtures/realworld/` and the public session
//! API the application uses. Mutations change the document Info dictionary
//! through `Document::set_info`, which is a small, deterministic change that
//! both the incremental and full save paths support.

use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use pdfxi_core::Document;
use pdfxi_document::{DocumentSession, MAX_REVISIONS, SessionError};

/// Return the absolute path of a real-world fixture.
fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("workspace root")
        .join("fixtures")
        .join("realworld")
        .join(name)
}

/// A per-test temporary directory that removes itself on drop.
struct TempDir {
    path: PathBuf,
}

impl TempDir {
    fn new(label: &str) -> Self {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let sequence = COUNTER.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir()
            .join("pdfxi-document-tests")
            .join(format!("{label}-{}-{sequence}", std::process::id()));
        fs::create_dir_all(&path).expect("create temporary directory");
        Self { path }
    }

    fn join(&self, name: &str) -> PathBuf {
        self.path.join(name)
    }
}

impl Drop for TempDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// Set the document title through the classic Info dictionary.
fn set_title(document: &mut Document, title: &str) -> Result<(), SessionError> {
    let mut info = document.info()?.unwrap_or_default();
    info.title = Some(title.to_owned());
    document.set_info(&info)?;
    Ok(())
}

/// Return the current Info title.
fn title(session: &DocumentSession) -> Option<String> {
    session
        .document()
        .info()
        .expect("read Info dictionary")
        .and_then(|info| info.title)
}

/// Return the concatenated text of every page.
fn all_text(session: &DocumentSession) -> String {
    let count = session.document().page_count().expect("page count");
    let mut text = String::new();
    for index in 0..count {
        text.push_str(&session.document().page_text(index).expect("page text"));
    }
    text
}

/// Copy a fixture into a fresh temporary directory.
fn copy_fixture(label: &str, name: &str) -> (TempDir, PathBuf, Vec<u8>) {
    let dir = TempDir::new(label);
    let path = dir.join(name);
    let bytes = fs::read(fixture(name)).expect("read fixture");
    fs::write(&path, &bytes).expect("write fixture copy");
    (dir, path, bytes)
}

#[test]
fn encrypted_fixture_opens_with_password_and_extracts_text() {
    let path = fixture("pdfxi-encrypted-selfmade.pdf");
    let mut session =
        DocumentSession::open_with_password(&path, Some(b"pdfxi-test")).expect("open encrypted");
    assert!(session.is_encrypted());
    assert!(!session.is_dirty());
    let report = session.decryption_report().expect("decryption report");
    assert_eq!(report.method, pdfxi_security::EncryptionMethod::Aes128);
    let text = session.document().page_text(0).expect("page text");
    assert!(
        text.contains("Encrypted selfmade page marker"),
        "decrypted page text was {text:?}"
    );
    assert_eq!(session.path(), Some(path.as_path()));
    assert!(matches!(
        session.save(),
        Err(SessionError::EncryptedSaveUnsupported)
    ));

    let bytes = fs::read(&path).expect("read fixture");
    assert!(matches!(
        DocumentSession::open_bytes(&bytes),
        Err(SessionError::PasswordRequired)
    ));
    assert!(matches!(
        DocumentSession::open(&path),
        Err(SessionError::PasswordRequired)
    ));
    assert!(matches!(
        DocumentSession::open_bytes_with_password(&bytes, Some(b"not-the-password")),
        Err(SessionError::WrongPassword)
    ));
}

#[test]
fn incremental_edits_undo_and_redo_keep_values_and_page_text() {
    let (_dir, path, _bytes) = copy_fixture("edits", "microsoft-excel-2019-sample.pdf");
    let mut session = DocumentSession::open(&path).expect("open fixture");
    let original_title = title(&session);
    let original_text = all_text(&session);
    assert!(original_text.contains("PDFXI Real-World Sample"));
    assert!(original_text.contains("amber compass lantern"));
    assert!(original_text.contains("meadow harmonic"));

    session
        .edit(|document| set_title(document, "First revision"))
        .expect("first edit");
    assert_eq!(title(&session).as_deref(), Some("First revision"));
    assert_eq!(all_text(&session), original_text);
    assert_eq!(session.revision_count(), 1);
    assert!(session.is_dirty());

    session
        .edit(|document| set_title(document, "Second revision"))
        .expect("second edit");
    assert_eq!(title(&session).as_deref(), Some("Second revision"));
    assert_eq!(session.revision_count(), 2);

    session.undo().expect("undo second edit");
    assert_eq!(title(&session).as_deref(), Some("First revision"));
    assert_eq!(all_text(&session), original_text);
    assert!(session.can_redo());

    session.undo().expect("undo first edit");
    assert_eq!(title(&session), original_title);
    assert_eq!(all_text(&session), original_text);
    assert!(!session.can_undo());
    assert!(!session.is_dirty());

    session.redo().expect("redo first edit");
    assert_eq!(title(&session).as_deref(), Some("First revision"));
    assert_eq!(all_text(&session), original_text);
    assert_eq!(session.revision_count(), 1);
    assert!(session.can_redo());
}

#[test]
fn failed_atomic_save_leaves_the_original_untouched() {
    let (dir, path, original) = copy_fixture("atomic", "microsoft-excel-2019-sample.pdf");
    let mut session = DocumentSession::open(&path).expect("open fixture");
    session
        .edit(|document| set_title(document, "Unwritten revision"))
        .expect("edit");
    assert_ne!(session.bytes(), original.as_slice());

    // Renaming a temporary file over an existing directory fails on Windows
    // and on Unix, so this exercises the replace step deterministically.
    let destination = dir.join("destination-directory");
    fs::create_dir(&destination).expect("create destination directory");
    let error = session
        .save_as(&destination)
        .expect_err("saving over a directory must fail");
    assert!(matches!(error, SessionError::Io(_)), "unexpected {error:?}");

    assert_eq!(fs::read(&path).expect("read original"), original);
    assert_eq!(session.path(), Some(path.as_path()));
    let leftovers: Vec<String> = fs::read_dir(&dir.path)
        .expect("read temporary directory")
        .filter_map(Result::ok)
        .map(|entry| entry.file_name().to_string_lossy().into_owned())
        .filter(|name| name.contains(".pdfxi-tmp"))
        .collect();
    assert!(leftovers.is_empty(), "temporary files left: {leftovers:?}");
}

#[test]
fn external_change_is_reported_unless_forced() {
    let (_dir, path, _bytes) = copy_fixture("external", "microsoft-excel-2019-sample.pdf");
    let mut session = DocumentSession::open(&path).expect("open fixture");
    session
        .edit(|document| set_title(document, "Session revision"))
        .expect("edit");

    let mut file = fs::OpenOptions::new()
        .append(true)
        .open(&path)
        .expect("open for append");
    file.write_all(b"\n% appended by another program\n")
        .expect("append");
    drop(file);

    let error = session.save().expect_err("save must detect the change");
    match error {
        SessionError::ExternallyModified { path: reported } => assert_eq!(reported, path),
        other => panic!("expected ExternallyModified, got {other:?}"),
    }

    session.save_forced().expect("forced save");
    assert_eq!(
        fs::read(&path).expect("read saved file"),
        session.bytes(),
        "forced save must write the session bytes"
    );
    assert!(!session.is_dirty());
}

#[test]
fn full_rewrite_edit_makes_earlier_revisions_non_undoable() {
    let (_dir, path, _bytes) = copy_fixture("rewrite", "microsoft-excel-2019-sample.pdf");
    let mut session = DocumentSession::open(&path).expect("open fixture");
    session
        .edit(|document| set_title(document, "Incremental revision"))
        .expect("incremental edit");
    assert!(session.can_undo());

    session
        .edit_full(|document| set_title(document, "Rewritten revision"))
        .expect("full rewrite");
    assert_eq!(title(&session).as_deref(), Some("Rewritten revision"));
    assert!(!session.can_undo());
    assert!(matches!(
        session.undo(),
        Err(SessionError::UndoAcrossRewrite)
    ));
}

#[test]
fn revision_cap_drops_the_oldest_and_reports_it() {
    let (_dir, path, _bytes) = copy_fixture("cap", "microsoft-excel-2019-sample.pdf");
    let mut session = DocumentSession::open(&path).expect("open fixture");
    let total = MAX_REVISIONS + 5;
    for index in 0..total {
        session
            .edit(|document| set_title(document, &format!("revision {index}")))
            .expect("edit");
    }
    assert_eq!(session.revision_count(), MAX_REVISIONS);
    assert!(session.dropped_oldest_revision());

    for _ in 0..MAX_REVISIONS {
        session.undo().expect("undo within the retained window");
    }
    assert!(!session.can_undo());
    assert!(matches!(session.undo(), Err(SessionError::NothingToUndo)));
    assert_eq!(title(&session).as_deref(), Some("revision 4"));
}
