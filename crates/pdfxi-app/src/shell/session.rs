//! Session-backed document actions: open, save, save-as, undo/redo, passwords.
//!
//! The app keeps one [`DocumentSession`] per tab. View-only state (zoom,
//! scroll, page) stays on [`crate::DocumentTab`]; byte-changing edits go through
//! the session so undo, redo and atomic save are available. This module owns the
//! pure close-plan decision and the small prompt-state structs the dialogs use.

use std::path::{Path, PathBuf};

use pdfxi_document::{DocumentSession, SessionError};
use pdfxi_security::Permission;

use crate::OpenOutcome;
use crate::tabs::{display_name_from_path, normalize_path_key};
use crate::ui::PdfxiApp;

/// Hard upper bound for a document opened by the app.
const MAX_DOCUMENT_BYTES: u64 = 2 * 1024 * 1024 * 1024;

/// What the user chose in the unsaved-changes prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseChoice {
    /// Save the document, then close it.
    Save,
    /// Close without saving.
    Discard,
    /// Leave the tab open.
    Cancel,
}

/// The action a close request needs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClosePlan {
    /// Close immediately; the document is clean.
    CloseNow,
    /// Ask the user first; the document is dirty.
    Prompt,
    /// Save, then close.
    SaveThenClose,
    /// Keep the tab open.
    KeepOpen,
}

/// Decide how a close request proceeds.
///
/// A clean tab never prompts. A dirty tab prompts when no choice was supplied;
/// Save and Discard both close after their step, Cancel keeps it open.
#[must_use]
pub fn plan_close(dirty: bool, choice: Option<CloseChoice>) -> ClosePlan {
    match (dirty, choice) {
        (false, _) => ClosePlan::CloseNow,
        (true, None) => ClosePlan::Prompt,
        (true, Some(CloseChoice::Save)) => ClosePlan::SaveThenClose,
        (true, Some(CloseChoice::Discard)) => ClosePlan::CloseNow,
        (true, Some(CloseChoice::Cancel)) => ClosePlan::KeepOpen,
    }
}

/// Whether a save error is the external-change conflict that offers Save As.
#[must_use]
pub fn save_conflict_offers_save_as(error: &SessionError) -> bool {
    matches!(error, SessionError::ExternallyModified { .. })
}

/// State for the password modal.
#[derive(Debug, Clone)]
pub(crate) struct PasswordPrompt {
    /// Document being opened.
    pub(crate) path: PathBuf,
    /// File name shown in the modal.
    pub(crate) title: String,
    /// Masked field contents.
    pub(crate) input: String,
    /// Typed error from the previous attempt, if any.
    pub(crate) error: Option<String>,
}

impl PasswordPrompt {
    fn new(path: &Path) -> Self {
        Self {
            path: path.to_path_buf(),
            title: display_name_from_path(path),
            input: String::new(),
            error: None,
        }
    }
}

/// State for the unsaved-changes prompt.
#[derive(Debug, Clone)]
pub(crate) struct PendingClose {
    /// Tab waiting to close.
    pub(crate) id: crate::TabId,
    /// Title shown in the prompt.
    pub(crate) title: String,
}

/// State for the external-modification dialog.
#[derive(Debug, Clone)]
pub(crate) struct SaveConflict {
    /// Tab whose save conflicted.
    pub(crate) id: crate::TabId,
    /// Path that changed on disk.
    pub(crate) path: PathBuf,
}

impl PdfxiApp {
    /// Open a path through a session, optionally with a password.
    pub(crate) fn open_document_session(
        &mut self,
        path: &Path,
        password: Option<&[u8]>,
    ) -> OpenOutcome {
        if let Some(id) = self.existing_tab_for(path) {
            let _ = self.state.activate_tab(id);
            self.on_active_tab_changed();
            return OpenOutcome::Focused { id };
        }
        if let Ok(metadata) = std::fs::metadata(path)
            && metadata.len() > MAX_DOCUMENT_BYTES
        {
            let message = "This PDF is too large (limit 2 GiB).".to_owned();
            self.open_error_banner = Some(message.clone());
            self.status = message.clone();
            return OpenOutcome::Failed { message };
        }
        match DocumentSession::open_with_password(path, password) {
            Ok(session) => {
                let read_only = session
                    .decryption_report()
                    .is_some_and(|report| !report.permissions.allows(Permission::MODIFY));
                let id = self.state.open_path(path);
                if read_only {
                    self.read_only_tabs.insert(id);
                }
                self.sessions.insert(id, session);
                self.finish_new_tab(id);
                // A signed PDF shows its chip and verification panel
                // immediately, once per opened document; an unsigned one
                // clears the previous readout without an error.
                self.verify_signature_on_open(id);
                if let Some(signer) = self
                    .signing
                    .verification
                    .as_ref()
                    .map(|verification| verification.signer_subject().to_owned())
                {
                    self.mark_returned_signature(path, Some(&signer));
                }
                self.sync_tab_dirty(id);
                self.refresh_annotations();
                self.refresh_forms();
                self.refresh_bookmarks();
                self.refresh_renderer_for_active_tab();
                let title = self.state.tab_title(id).unwrap_or_default();
                self.library.record_open(path, &title, 0);
                let opened_name = path.file_name().map_or_else(
                    || path.display().to_string(),
                    |name| name.to_string_lossy().into_owned(),
                );
                self.notify_real_event("Document opened", &opened_name);
                self.status = format!("Opened {}", path.display());
                OpenOutcome::Opened { id }
            }
            Err(SessionError::PasswordRequired) => {
                self.password_prompt = Some(PasswordPrompt::new(path));
                self.status = "Password required".to_owned();
                OpenOutcome::PasswordRequired {
                    path: path.to_path_buf(),
                }
            }
            Err(SessionError::WrongPassword) if password.is_some() => {
                let prompt = self
                    .password_prompt
                    .get_or_insert_with(|| PasswordPrompt::new(path));
                prompt.error = Some(SessionError::WrongPassword.to_string());
                self.status = SessionError::WrongPassword.to_string();
                OpenOutcome::PasswordRequired {
                    path: path.to_path_buf(),
                }
            }
            Err(error) => {
                let message = error.to_string();
                self.open_error_banner = Some(message.clone());
                self.status = message.clone();
                OpenOutcome::Failed { message }
            }
        }
    }

    /// Retry opening the prompted document with `password`.
    ///
    /// Returns `true` when a tab opened.
    pub fn open_with_password(&mut self, path: &Path, password: &str) -> bool {
        match self.open_document_session(path, Some(password.as_bytes())) {
            OpenOutcome::Opened { .. } | OpenOutcome::Focused { .. } => {
                self.password_prompt = None;
                true
            }
            _ => false,
        }
    }

    /// Submit the currently prompted password.
    pub(crate) fn submit_password(&mut self, password: String) {
        let Some(path) = self
            .password_prompt
            .as_ref()
            .map(|prompt| prompt.path.clone())
        else {
            return;
        };
        if self.open_with_password(&path, &password) {
            self.status = "Document unlocked".to_owned();
        }
    }

    /// Cancel the password prompt; no tab opens and the banner explains why.
    pub fn cancel_password_prompt(&mut self) {
        if let Some(prompt) = self.password_prompt.take() {
            let message = format!("{}: {}", prompt.title, SessionError::PasswordRequired);
            self.open_error_banner = Some(message.clone());
            self.status = message;
        }
    }

    /// Whether the password modal is open.
    #[must_use]
    pub fn password_prompt_open(&self) -> bool {
        self.password_prompt.is_some()
    }

    /// The typed error shown in the password modal, if any.
    #[must_use]
    pub fn password_prompt_error(&self) -> Option<&str> {
        self.password_prompt
            .as_ref()
            .and_then(|prompt| prompt.error.as_deref())
    }

    /// Whether the active tab's permissions deny modification.
    #[must_use]
    pub fn active_is_read_only(&self) -> bool {
        self.state
            .active_tab_id()
            .is_some_and(|id| self.read_only_tabs.contains(&id))
    }

    /// Whether the active session can undo.
    #[must_use]
    pub fn can_undo(&self) -> bool {
        self.state
            .active_tab_id()
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(DocumentSession::can_undo)
            && !self.active_is_read_only()
    }

    /// Whether the active session can redo.
    #[must_use]
    pub fn can_redo(&self) -> bool {
        self.state
            .active_tab_id()
            .and_then(|id| self.sessions.get(&id))
            .is_some_and(DocumentSession::can_redo)
            && !self.active_is_read_only()
    }

    /// Save the active document through its session.
    pub fn save_active_document(&mut self) -> Result<(), SessionError> {
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NoPath);
        };
        self.save_tab_by_id(id)
    }

    /// Save the active document to a new path and update the tab.
    pub fn save_active_as(&mut self, path: &Path) -> Result<(), SessionError> {
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NoPath);
        };
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NoPath);
            };
            session.save_as(path)
        };
        result?;
        if let Some(tab) = self.state.tab_mut(id) {
            tab.replace_path(path);
        }
        self.sync_tab_dirty(id);
        self.save_conflict = None;
        self.refresh_renderer_for_active_tab();
        self.status = format!("Saved {}", path.display());
        let saved_name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.notify_real_event("Document saved", &saved_name);
        Ok(())
    }

    /// Save over an external change after the user confirms.
    pub fn save_forced_active(&mut self) -> Result<(), SessionError> {
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NoPath);
        };
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NoPath);
            };
            session.save_forced()
        };
        result?;
        self.sync_tab_dirty(id);
        self.save_conflict = None;
        self.status = "Saved over the external change".to_owned();
        let saved_name = self
            .state
            .tab(id)
            .and_then(|tab| tab.path())
            .and_then(|path| path.file_name())
            .map_or_else(
                || "document".to_owned(),
                |name| name.to_string_lossy().into_owned(),
            );
        self.notify_real_event("Document saved", &saved_name);
        Ok(())
    }

    /// Show the native Save As picker and save through it.
    pub(crate) fn save_as_dialog(&mut self) {
        let Some(tab) = self.state.active_tab() else {
            return;
        };
        let suggested = tab
            .path()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("document.pdf")
            .to_owned();
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save PDF as")
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(&suggested)
            .save_file()
            && let Err(error) = self.save_active_as(&path)
        {
            self.open_error_banner = Some(error.to_string());
        }
    }

    /// Undo the newest session revision.
    pub fn undo_document(&mut self) -> Result<(), SessionError> {
        if self.active_is_read_only() {
            self.status = "The document permissions do not allow modification".to_owned();
            return Ok(());
        }
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NothingToUndo);
        };
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NothingToUndo);
            };
            session.undo()
        };
        result?;
        self.after_session_edit(id);
        self.status = "Undo".to_owned();
        Ok(())
    }

    /// Redo the most recently undone revision.
    pub fn redo_document(&mut self) -> Result<(), SessionError> {
        if self.active_is_read_only() {
            self.status = "The document permissions do not allow modification".to_owned();
            return Ok(());
        }
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NothingToRedo);
        };
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NothingToRedo);
            };
            session.redo()
        };
        result?;
        self.after_session_edit(id);
        self.status = "Redo".to_owned();
        Ok(())
    }

    /// Set the active document's Info title through a session edit.
    pub fn set_active_document_title(&mut self, title: &str) -> Result<(), SessionError> {
        if self.active_is_read_only() {
            self.status = "The document permissions do not allow modification".to_owned();
            return Ok(());
        }
        let Some(id) = self.state.active_tab_id() else {
            return Err(SessionError::NoPath);
        };
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NoPath);
            };
            session.edit(|document| {
                let mut info = document.info()?.unwrap_or_default();
                info.title = Some(title.to_owned());
                document.set_info(&info)?;
                Ok(())
            })
        };
        result?;
        self.sync_tab_dirty(id);
        self.refresh_renderer_for_active_tab();
        self.status = "Document title changed".to_owned();
        Ok(())
    }

    /// Ask to close a tab, prompting when it has unsaved changes.
    pub(crate) fn request_close_tab(&mut self, id: crate::TabId) {
        let dirty = self.sessions.get(&id).map_or_else(
            || self.state.tab(id).is_some_and(|tab| tab.is_dirty()),
            DocumentSession::is_dirty,
        );
        match plan_close(dirty, None) {
            ClosePlan::CloseNow => {
                let _ = self.force_close_tab(id);
            }
            ClosePlan::Prompt => {
                let title = self.state.tab_title(id).unwrap_or_default();
                self.pending_close = Some(PendingClose { id, title });
            }
            ClosePlan::SaveThenClose | ClosePlan::KeepOpen => {}
        }
    }

    /// Ask to close the active tab.
    pub(crate) fn request_close_active_tab(&mut self) {
        if let Some(id) = self.state.active_tab_id() {
            self.request_close_tab(id);
        } else {
            self.status = "No tab to close".to_owned();
        }
    }

    /// Resolve the unsaved-changes prompt.
    pub(crate) fn resolve_close(&mut self, choice: CloseChoice) {
        let Some(pending) = self.pending_close.take() else {
            return;
        };
        let mut closed = false;
        match plan_close(true, Some(choice)) {
            ClosePlan::CloseNow => {
                closed = self.force_close_tab(pending.id);
            }
            ClosePlan::SaveThenClose => {
                if self.save_tab_by_id(pending.id).is_ok() {
                    closed = self.force_close_tab(pending.id);
                } else {
                    self.pending_close = Some(pending);
                }
            }
            ClosePlan::KeepOpen | ClosePlan::Prompt => {}
        }
        if !self.close_window_after_resolve {
            return;
        }
        if choice == CloseChoice::Cancel || !closed {
            self.close_window_after_resolve = false;
            return;
        }
        // The window was closing: prompt for the next dirty tab, or finish.
        let mut dirty = self
            .sessions
            .iter()
            .filter(|(_, session)| session.is_dirty())
            .map(|(id, _)| *id);
        if let Some(id) = dirty.next() {
            let title = self.state.tab_title(id).unwrap_or_default();
            self.pending_close = Some(PendingClose { id, title });
        } else {
            self.close_window_after_resolve = false;
            self.request_window_close = true;
        }
    }

    /// Whether the external-modification dialog is open.
    #[must_use]
    pub fn save_conflict_open(&self) -> bool {
        self.save_conflict.is_some()
    }

    /// Dismiss the external-modification dialog.
    pub(crate) fn cancel_save_conflict(&mut self) {
        self.save_conflict = None;
    }

    /// The path shown in the external-modification dialog.
    #[must_use]
    pub fn save_conflict_path(&self) -> Option<&Path> {
        self.save_conflict
            .as_ref()
            .map(|conflict| conflict.path.as_path())
    }

    /// Save a specific tab and sync its dirty marker.
    fn save_tab_by_id(&mut self, id: crate::TabId) -> Result<(), SessionError> {
        if self.read_only_tabs.contains(&id) {
            self.status = "The document permissions do not allow modification".to_owned();
            return Ok(());
        }
        let result = {
            let Some(session) = self.sessions.get_mut(&id) else {
                return Err(SessionError::NoPath);
            };
            session.save()
        };
        match result {
            Ok(()) => {
                self.sync_tab_dirty(id);
                self.refresh_renderer_for_active_tab();
                self.status = "Saved".to_owned();
                let saved_name = self
                    .state
                    .tab(id)
                    .and_then(|tab| tab.path())
                    .and_then(|path| path.file_name())
                    .map_or_else(
                        || "document".to_owned(),
                        |name| name.to_string_lossy().into_owned(),
                    );
                self.notify_real_event("Document saved", &saved_name);
                Ok(())
            }
            Err(SessionError::ExternallyModified { path }) => {
                self.save_conflict = Some(SaveConflict {
                    id,
                    path: path.clone(),
                });
                Err(SessionError::ExternallyModified { path })
            }
            Err(error) => {
                self.open_error_banner = Some(error.to_string());
                Err(error)
            }
        }
    }

    /// Sync a tab after any session edit and rebuild everything derived from it.
    ///
    /// This is the single post-edit hook: dirty marker, canvas renderer and
    /// raster textures, thumbnails and the annotation list. A caller must not
    /// patch only one of them.
    pub(crate) fn after_session_edit(&mut self, id: crate::TabId) {
        self.sync_tab_dirty(id);
        self.force_canvas_reload();
        self.refresh_annotations();
        self.refresh_forms();
        self.refresh_bookmarks();
        // A verification readout describes one exact byte state, so any later
        // edit, undo or redo makes the E-signed badge stale until it is
        // checked again.
        self.signing.verification = None;
    }

    /// Sync a tab's dirty marker with its session.
    pub(crate) fn sync_tab_dirty(&mut self, id: crate::TabId) {
        let dirty = self
            .sessions
            .get(&id)
            .is_some_and(DocumentSession::is_dirty);
        if let Some(tab) = self.state.tab_mut(id) {
            tab.set_dirty(dirty);
        }
    }

    /// Find an existing tab for the same normalized path.
    fn existing_tab_for(&self, path: &Path) -> Option<crate::TabId> {
        let key = normalize_path_key(path);
        if key.is_empty() {
            return None;
        }
        self.state
            .tabs()
            .find(|tab| tab.path_key().as_deref() == Some(key.as_str()))
            .map(|tab| tab.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::{AppCommand, PdfxiApp};
    use pdfxi_text::TextExtractor;

    fn fixture(relative: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(relative)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfxi-t0222-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("create temp dir");
        dir
    }

    /// A clean tab never prompts; a dirty tab prompts until answered.
    #[test]
    fn close_plans_prompt_only_for_dirty_tabs() {
        assert_eq!(plan_close(false, None), ClosePlan::CloseNow);
        assert_eq!(
            plan_close(false, Some(CloseChoice::Save)),
            ClosePlan::CloseNow
        );
        assert_eq!(plan_close(true, None), ClosePlan::Prompt);
        assert_eq!(
            plan_close(true, Some(CloseChoice::Save)),
            ClosePlan::SaveThenClose
        );
        assert_eq!(
            plan_close(true, Some(CloseChoice::Discard)),
            ClosePlan::CloseNow
        );
        assert_eq!(
            plan_close(true, Some(CloseChoice::Cancel)),
            ClosePlan::KeepOpen
        );
    }

    /// Closing the window after a dirty tab asks about every dirty tab before
    /// the window is allowed to close.
    #[cfg(feature = "pdfium")]
    #[test]
    fn window_close_prompts_for_each_dirty_tab() {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture("../../fixtures/minimal.pdf"));
        app.set_active_document_title("first dirty")
            .expect("dirty first tab");
        app.open_document(&fixture("../../fixtures/text.pdf"));
        app.set_active_document_title("second dirty")
            .expect("dirty second tab");
        assert_eq!(app.sessions.len(), 2);
        assert_eq!(app.dirty_tab_ids().len(), 2);

        app.close_window_after_resolve = true;
        let ids: Vec<_> = app.sessions.keys().copied().collect();
        app.pending_close = Some(PendingClose {
            id: ids[0],
            title: "first".to_owned(),
        });
        app.resolve_close(CloseChoice::Discard);
        assert!(
            app.pending_close.is_some(),
            "the second dirty tab must still prompt"
        );
        assert!(!app.request_window_close);

        app.resolve_close(CloseChoice::Discard);
        assert!(app.pending_close.is_none());
        assert!(
            app.request_window_close,
            "the window may close after every dirty tab is resolved"
        );
        assert!(!app.close_window_after_resolve);
    }

    /// The password modal must put the prompt back before it dispatches, or
    /// the command cannot find the document and every submit closes the modal
    /// without opening anything.
    #[cfg(feature = "pdfium")]
    #[test]
    fn dialog_submission_restores_the_prompt_before_opening() {
        let path = fixture("../../fixtures/realworld/pdfxi-encrypted-selfmade.pdf");
        let mut app = PdfxiApp::new();
        let _ = app.open_document(&path);
        assert!(app.password_prompt_open());

        let prompt = app.password_prompt.take().expect("pending prompt");
        let ctx = egui::Context::default();
        app.apply_password_dialog(&ctx, prompt, Some("not-it".to_owned()), false);
        assert!(app.password_prompt_open(), "wrong password keeps the modal");
        assert_eq!(
            app.password_prompt_error(),
            Some(SessionError::WrongPassword.to_string().as_str())
        );
        assert_eq!(app.state().tab_count(), 0);

        let prompt = app
            .password_prompt
            .take()
            .expect("prompt after a wrong try");
        app.apply_password_dialog(&ctx, prompt, Some("pdfxi-test".to_owned()), false);
        assert!(!app.password_prompt_open());
        assert_eq!(app.state().tab_count(), 1);
    }

    /// Only an external modification offers "Save As"; other errors do not.
    #[test]
    fn only_external_changes_offer_save_as() {
        assert!(save_conflict_offers_save_as(
            &SessionError::ExternallyModified {
                path: PathBuf::from("report.pdf"),
            }
        ));
        assert!(!save_conflict_offers_save_as(&SessionError::NothingToUndo));
        assert!(!save_conflict_offers_save_as(&SessionError::NoPath));
    }

    /// Opening an encrypted document asks for the password, rejects a wrong
    /// one without opening a tab, and opens the real document on success.
    #[cfg(feature = "pdfium")]
    #[test]
    fn encrypted_document_asks_for_a_password_and_cancel_opens_no_tab() {
        let path = fixture("../../fixtures/realworld/pdfxi-encrypted-selfmade.pdf");
        let mut app = PdfxiApp::new();

        let outcome = app.open_document(&path);
        assert!(matches!(outcome, OpenOutcome::PasswordRequired { .. }));
        assert!(app.password_prompt_open());
        assert_eq!(app.state().tab_count(), 0);

        // Wrong password keeps the modal open with the typed message.
        app.dispatch(AppCommand::SubmitPassword("not-it".to_owned()), None);
        assert!(app.password_prompt_open());
        assert_eq!(
            app.password_prompt_error(),
            Some(SessionError::WrongPassword.to_string().as_str())
        );
        assert_eq!(app.state().tab_count(), 0);

        // The real password opens the decrypted document with visible text.
        app.dispatch(AppCommand::SubmitPassword("pdfxi-test".to_owned()), None);
        assert!(!app.password_prompt_open());
        assert_eq!(app.state().tab_count(), 1);
        let text = app
            .state()
            .active_tab()
            .and_then(|tab| app.sessions.get(&tab.id()))
            .and_then(|session| session.document().page_text(0).ok())
            .expect("decrypted page text");
        assert!(
            text.contains("Encrypted selfmade page marker"),
            "text was {text:?}"
        );

        // Permissions are wired from the session to the tab.
        let read_only = app
            .state()
            .active_tab()
            .and_then(|tab| app.sessions.get(&tab.id()))
            .and_then(|session| session.decryption_report())
            .is_some_and(|report| !report.permissions.allows(Permission::MODIFY));
        assert_eq!(app.active_is_read_only(), read_only);

        // Cancel on a fresh open leaves no tab and explains why.
        let mut cancelling = PdfxiApp::new();
        let _ = cancelling.open_document(&path);
        cancelling.dispatch(AppCommand::CancelPasswordPrompt, None);
        assert!(!cancelling.password_prompt_open());
        assert_eq!(cancelling.state().tab_count(), 0);
        assert!(cancelling.open_error_banner.is_some());
    }

    /// A session edit marks the tab dirty, undo clears the marker, redo
    /// restores it, and Save As writes a file that reopens with the new title
    /// and the same page text.
    #[cfg(feature = "pdfium")]
    #[test]
    fn edit_title_undo_redo_and_save_round_trip() {
        let source = fixture("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let dir = temp_dir("round-trip");
        let target = dir.join("saved.pdf");
        let mut app = PdfxiApp::new();
        assert!(matches!(
            app.open_document(&source),
            OpenOutcome::Opened { .. }
        ));
        let id = app.state().active_tab_id().expect("active tab");
        let original_text = app
            .sessions
            .get(&id)
            .and_then(|session| session.document().page_text(0).ok())
            .expect("original page text");
        let pages = app
            .state()
            .active_tab()
            .map(|tab| tab.page_count())
            .unwrap_or(0);
        assert!(pages > 0, "fixture must have pages");

        assert!(app.set_active_document_title("Round trip").is_ok());
        assert!(app.state().active_tab().is_some_and(|tab| tab.is_dirty()));
        assert!(
            app.state()
                .active_tab()
                .is_some_and(|tab| tab.title().contains('*'))
        );
        assert!(app.can_undo());

        assert!(app.undo_document().is_ok());
        assert!(!app.state().active_tab().is_some_and(|tab| tab.is_dirty()));
        assert!(app.can_redo());
        let title = app
            .sessions
            .get(&id)
            .and_then(|session| session.document().info().ok().flatten())
            .and_then(|info| info.title);
        assert_ne!(title.as_deref(), Some("Round trip"));

        assert!(app.redo_document().is_ok());
        assert!(app.state().active_tab().is_some_and(|tab| tab.is_dirty()));
        assert!(app.can_undo());

        app.save_active_as(&target).expect("save as");
        assert!(!app.state().active_tab().is_some_and(|tab| tab.is_dirty()));
        assert_eq!(
            app.state().active_tab().and_then(|tab| tab.path()),
            Some(target.as_path())
        );

        let reopened = DocumentSession::open(&target).expect("reopen saved file");
        let info = reopened
            .document()
            .info()
            .expect("info")
            .expect("info dictionary");
        assert_eq!(info.title.as_deref(), Some("Round trip"));
        assert_eq!(reopened.document().page_count().ok(), Some(pages));
        assert_eq!(
            reopened.document().page_text(0).expect("saved page text"),
            original_text
        );

        // `PdfiumRenderer::from_bytes` loads the saved artifact and extracts
        // the same first-page text, which is the renderer path the canvas uses.
        let bytes = std::fs::read(&target).expect("read saved bytes");
        let renderer = pdfxi_render::PdfiumRenderer::from_bytes(bytes).expect("render saved bytes");
        let cancellation = pdfxi_text::CancellationToken::new();
        let extracted = renderer
            .extract_page(0, &cancellation, &mut |_| {})
            .expect("extract saved text");
        let text: String = extracted
            .spans()
            .iter()
            .map(|span| span.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        let first_word = original_text.split_whitespace().next().unwrap_or("");
        assert!(
            !first_word.is_empty() && text.contains(first_word),
            "renderer text {text:?} should contain {first_word:?}"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// Closing a dirty tab prompts; Cancel keeps it, Save saves then closes.
    #[cfg(feature = "pdfium")]
    #[test]
    fn dirty_close_prompt_cancel_then_save() {
        let source = fixture("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let dir = temp_dir("close");
        let target = dir.join("copy.pdf");
        std::fs::copy(&source, &target).expect("copy fixture");
        let mut app = PdfxiApp::new();
        assert!(matches!(
            app.open_document(&target),
            OpenOutcome::Opened { .. }
        ));
        assert!(app.set_active_document_title("Dirty copy").is_ok());

        app.close_active_tab();
        assert!(app.pending_close.is_some(), "dirty tab must prompt");
        app.resolve_close(CloseChoice::Cancel);
        assert_eq!(app.state().tab_count(), 1);
        assert!(app.pending_close.is_none());

        app.close_active_tab();
        app.resolve_close(CloseChoice::Save);
        assert_eq!(app.state().tab_count(), 0);
        assert!(app.pending_close.is_none());
        let saved = DocumentSession::open(&target).expect("saved copy");
        assert_eq!(
            saved
                .document()
                .info()
                .expect("info")
                .expect("dictionary")
                .title
                .as_deref(),
            Some("Dirty copy")
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// A save after the file changed on disk returns the typed conflict and
    /// opens the Save As / overwrite dialog state.
    #[cfg(feature = "pdfium")]
    #[test]
    fn external_modification_is_reported_and_offers_save_as() {
        let source = fixture("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let dir = temp_dir("conflict");
        let target = dir.join("changed.pdf");
        std::fs::copy(&source, &target).expect("copy fixture");
        let mut app = PdfxiApp::new();
        assert!(matches!(
            app.open_document(&target),
            OpenOutcome::Opened { .. }
        ));
        assert!(app.set_active_document_title("Edited").is_ok());

        // Simulate another program appending to the file after it was opened.
        let mut bytes = std::fs::read(&target).expect("read fixture");
        bytes.extend_from_slice(b"%changed\n");
        std::fs::write(&target, bytes).expect("touch file");

        let error = app
            .save_active_document()
            .expect_err("stale file must conflict");
        assert!(save_conflict_offers_save_as(&error));
        assert!(app.save_conflict_open());
        assert_eq!(app.save_conflict_path(), Some(target.as_path()));
        assert_eq!(
            app.state().tab_count(),
            1,
            "a failed save keeps the tab open"
        );
        assert!(app.state().active_tab().is_some_and(|tab| tab.is_dirty()));

        app.dispatch(AppCommand::OverwriteDocument, None);
        assert!(!app.save_conflict_open());

        let _ = std::fs::remove_dir_all(&dir);
    }
}
