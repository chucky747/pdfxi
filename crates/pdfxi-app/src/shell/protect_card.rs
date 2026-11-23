//! Protect PDF card (A-30 / O-04 save half): password-protect the active
//! document and remove protection from an encrypted copy.
//!
//! Protection goes through `pdfxi_security::protect::protect_document` on the
//! active session document; removal reads the chosen bytes, calls
//! `remove_protection`, writes a decrypted copy and opens it. Passwords are
//! typed into masked fields and never persisted or logged.

use std::path::{Path, PathBuf};

use pdfxi_security::{
    Permission, ProtectOptions, ProtectPermissions, protect_document, remove_protection,
};

use crate::ui::PdfxiApp;

/// State of the Protect PDF window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ProtectUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// User (open) password.
    pub(crate) user_password: String,
    /// Owner (permissions) password.
    pub(crate) owner_password: String,
    /// Allow printing.
    pub(crate) allow_print: bool,
    /// Allow copying or extracting content.
    pub(crate) allow_copy: bool,
    /// Allow annotations.
    pub(crate) allow_annotate: bool,
    /// Allow filling forms.
    pub(crate) allow_forms: bool,
    /// Password typed for the remove operation.
    pub(crate) remove_password: String,
    /// Typed error from the last protect or remove call.
    pub(crate) error: Option<String>,
    /// Success or status line for the card.
    pub(crate) result: Option<String>,
    /// Decrypted copy produced by the last successful removal.
    pub(crate) decrypted_path: Option<PathBuf>,
}

impl Default for ProtectUiState {
    fn default() -> Self {
        Self {
            open: false,
            user_password: String::new(),
            owner_password: String::new(),
            allow_print: true,
            allow_copy: true,
            allow_annotate: true,
            allow_forms: true,
            remove_password: String::new(),
            error: None,
            result: None,
            decrypted_path: None,
        }
    }
}

impl PdfxiApp {
    /// Handle [`AppCommand::Protect`]: explain the missing document or open
    /// the Protect PDF window.
    pub(crate) fn handle_protect_command(&mut self) {
        if self.state.active_tab_id().is_none() {
            self.status = "Open a document to protect".to_owned();
            return;
        }
        self.protect.open = true;
        self.protect.error = None;
        self.protect.result = None;
        self.status = "Protect PDF: choose passwords and permissions".to_owned();
    }

    /// Whether the Protect PDF window is open (harness accessor).
    #[must_use]
    pub fn protect_card_open(&self) -> bool {
        self.protect.open
    }

    /// The permissions selected in the card.
    fn protect_permissions(&self) -> ProtectPermissions {
        ProtectPermissions {
            print: self.protect.allow_print,
            copy: self.protect.allow_copy,
            annotate: self.protect.allow_annotate,
            fill_forms: self.protect.allow_forms,
            // Not exposed in this round; keep the conservative-but-usable
            // defaults the READ_ONLY preset uses for accessibility and
            // high-quality printing.
            accessibility: true,
            print_high_quality: self.protect.allow_print,
            // Annotations are a modification, so the library's consistency
            // check requires this bit whenever annotations are granted.
            modify: self.protect.allow_annotate,
            assemble: false,
        }
    }

    /// Draw the Protect PDF window.
    pub(crate) fn protect_card_ui(&mut self, ctx: &egui::Context) {
        if !self.protect.open {
            return;
        }
        let mut open = true;
        let mut protect_save = false;
        let mut remove_pick = false;
        let mut open_decrypted = false;

        egui::Window::new("Protect PDF")
            .open(&mut open)
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                ui.heading("Password protection");
                ui.horizontal(|ui| {
                    ui.label("User password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.protect.user_password)
                            .password(true)
                            .hint_text("Required to open")
                            .desired_width(200.0),
                    );
                });
                ui.horizontal(|ui| {
                    ui.label("Owner password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.protect.owner_password)
                            .password(true)
                            .hint_text("Optional; reuses the user password")
                            .desired_width(200.0),
                    );
                });
                ui.label("Allow");
                ui.checkbox(&mut self.protect.allow_print, "Printing");
                ui.checkbox(&mut self.protect.allow_copy, "Copying text and images");
                ui.checkbox(&mut self.protect.allow_annotate, "Annotations");
                ui.checkbox(&mut self.protect.allow_forms, "Form filling");
                if ui.button("Protect and save as...").clicked() {
                    protect_save = true;
                }

                ui.separator();
                ui.heading("Remove protection");
                ui.horizontal(|ui| {
                    ui.label("Password");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.protect.remove_password)
                            .password(true)
                            .hint_text("Password of the encrypted PDF")
                            .desired_width(200.0),
                    );
                });
                if ui.button("Remove protection...").clicked() {
                    remove_pick = true;
                }
                if let Some(path) = &self.protect.decrypted_path {
                    ui.label(format!("Decrypted copy: {}", path.display()));
                    if ui.button("Open decrypted copy").clicked() {
                        open_decrypted = true;
                    }
                }

                if let Some(result) = &self.protect.result {
                    ui.label(result);
                }
                if let Some(error) = &self.protect.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });

        if protect_save {
            self.choose_protect_output();
        }
        if remove_pick {
            self.choose_removal_source();
        }
        if open_decrypted && let Some(path) = self.protect.decrypted_path.clone() {
            let _ = self.open_document(&path);
        }
        if !open {
            self.protect.open = false;
        }
    }

    /// Show the save dialog and protect the active document there.
    pub(crate) fn choose_protect_output(&mut self) {
        let Some(stem) = self.protected_stem() else {
            self.status = "Open a document to protect".to_owned();
            return;
        };
        let Some(path) = rfd::FileDialog::new()
            .set_title("Save the protected PDF")
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(format!("{stem}-protected.pdf"))
            .save_file()
        else {
            return;
        };
        let _ = self.protect_to_path(&path);
    }

    /// Protect the active session document and save it to `path`.
    pub(crate) fn protect_to_path(&mut self, path: &Path) -> Result<(), String> {
        let result = self.write_protected(path);
        match result {
            Ok(()) => {
                self.protect.error = None;
                self.protect.result = Some(format!("Protected copy saved to {}", path.display()));
                Ok(())
            }
            Err(error) => {
                self.protect.error = Some(error.clone());
                self.protect.result = None;
                self.status = format!("Protect failed: {error}");
                Err(error)
            }
        }
    }

    /// Build the protected bytes and write them.
    fn write_protected(&mut self, path: &Path) -> Result<(), String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to protect".to_owned());
        };
        let Some(session) = self.sessions.get(&id) else {
            return Err("The document has no open session".to_owned());
        };
        let mut options = ProtectOptions::new(self.protect.user_password.as_bytes())
            .with_permissions(self.protect_permissions());
        if !self.protect.owner_password.is_empty() {
            options = options.with_owner_password(self.protect.owner_password.as_bytes());
        }
        let bytes =
            protect_document(session.document(), options).map_err(|error| error.to_string())?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, bytes)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
        let name = path.file_name().map_or_else(
            || path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.status = format!("Protected {name} and saved it to {}", path.display());
        self.notify_real_event("PDF protected", &name);
        Ok(())
    }

    /// Pick an encrypted PDF with the native dialog and remove its protection.
    pub(crate) fn choose_removal_source(&mut self) {
        let Some(source) = rfd::FileDialog::new()
            .set_title("Choose an encrypted PDF")
            .add_filter("PDF documents", &["pdf"])
            .pick_file()
        else {
            return;
        };
        let stem = source.file_stem().map_or_else(
            || "document".to_owned(),
            |stem| stem.to_string_lossy().into_owned(),
        );
        let output = source.with_file_name(format!("{stem}-unprotected.pdf"));
        let password = self.protect.remove_password.clone();
        let _ = self.remove_protection_from(&source, &password, &output);
    }

    /// Remove protection from `source` and write a decrypted copy, then open it.
    pub(crate) fn remove_protection_from(
        &mut self,
        source: &Path,
        password: &str,
        output: &Path,
    ) -> Result<(), String> {
        let result = self.write_decrypted(source, password, output);
        match result {
            Ok(()) => {
                self.protect.error = None;
                self.protect.result = Some(format!(
                    "Protection removed; the decrypted copy is at {}",
                    output.display()
                ));
                self.protect.decrypted_path = Some(output.to_path_buf());
                self.protect.remove_password.clear();
                self.status = format!("Protection removed; decrypted copy at {}", output.display());
                self.notify_real_event("Protection removed", &output.display().to_string());
                let _ = self.open_document(output);
                Ok(())
            }
            Err(error) => {
                self.protect.error = Some(error.clone());
                self.protect.result = None;
                self.protect.decrypted_path = None;
                self.status = format!("Remove protection failed: {error}");
                Err(error)
            }
        }
    }

    /// Read, decrypt and write the copy.
    fn write_decrypted(
        &mut self,
        source: &Path,
        password: &str,
        output: &Path,
    ) -> Result<(), String> {
        let bytes = std::fs::read(source)
            .map_err(|error| format!("Could not read {}: {error}", source.display()))?;
        let decrypted =
            remove_protection(&bytes, password.as_bytes()).map_err(|error| error.to_string())?;
        if let Some(parent) = output
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(output, decrypted)
            .map_err(|error| format!("Could not write {}: {error}", output.display()))?;
        Ok(())
    }

    /// File stem of the active document for the protected default name.
    fn protected_stem(&self) -> Option<String> {
        let id = self.state.active_tab_id()?;
        Some(
            self.sessions
                .get(&id)
                .and_then(|session| session.path())
                .and_then(Path::file_stem)
                .map_or_else(
                    || "document".to_owned(),
                    |stem| stem.to_string_lossy().into_owned(),
                ),
        )
    }

    /// Permission bits exposed for the tests and capture readout.
    #[must_use]
    pub fn protect_selected_permission(&self, permission: Permission) -> bool {
        let permissions = self.protect_permissions();
        match permission {
            Permission::PRINT => permissions.print,
            Permission::COPY => permissions.copy,
            Permission::ANNOTATE => permissions.annotate,
            Permission::FILL_FORMS => permissions.fill_forms,
            Permission::MODIFY => permissions.modify,
            Permission::ASSEMBLE => permissions.assemble,
            _ => false,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_document::DocumentSession;
    use pdfxi_security::{PasswordRole, open_encrypted};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/protect-card")
            .join(format!("{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the protect directory");
        dir
    }

    fn app_with_excel() -> PdfxiApp {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        assert!(app.state.active_tab_id().is_some(), "the fixture opens");
        app
    }

    #[test]
    fn protects_with_passwords_and_permission_restrictions() {
        let dir = temp_dir("protect");
        let output = dir.join("protected.pdf");
        let mut app = app_with_excel();
        app.protect.user_password = "open-secret".to_owned();
        app.protect.owner_password = "owner-secret".to_owned();
        app.protect.allow_print = true;
        app.protect.allow_copy = false;
        app.protect.allow_annotate = false;
        app.protect.allow_forms = true;

        app.protect_to_path(&output).expect("protect the document");
        assert!(app.protect.error.is_none(), "{:?}", app.protect.error);
        let bytes = std::fs::read(&output).expect("read the protected file");

        let (document, report) =
            open_encrypted(&bytes, Some(b"open-secret")).expect("the user password opens it");
        assert_eq!(document.page_count().expect("page count"), 2);
        assert_eq!(report.role, PasswordRole::User);
        assert!(report.permissions.allows(Permission::PRINT));
        assert!(!report.permissions.allows(Permission::COPY));
        assert!(!report.permissions.allows(Permission::ANNOTATE));
        assert!(report.permissions.allows(Permission::FILL_FORMS));

        assert!(
            open_encrypted(&bytes, Some(b"wrong")).is_err(),
            "a wrong password must be a typed error"
        );
        assert!(
            open_encrypted(&bytes, None).is_err(),
            "a missing password must be a typed error"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn removal_writes_a_copy_that_opens_without_a_password() {
        let dir = temp_dir("remove");
        let protected = dir.join("protected.pdf");
        let decrypted = dir.join("decrypted.pdf");
        let mut app = app_with_excel();
        app.protect.user_password = "open-secret".to_owned();
        app.protect.owner_password = "owner-secret".to_owned();
        app.protect_to_path(&protected).expect("protect");

        // The open password unlocks reading but must not strip the owner's
        // permission restrictions; that requires the owner password.
        let user_only = dir.join("user-only.pdf");
        assert!(
            app.remove_protection_from(&protected, "open-secret", &user_only)
                .is_err(),
            "the user password must not remove owner restrictions"
        );
        assert!(!user_only.exists(), "a refused removal writes no file");

        app.remove_protection_from(&protected, "owner-secret", &decrypted)
            .expect("remove protection with the owner password");
        assert!(
            app.protect
                .result
                .as_deref()
                .is_some_and(|result| { result.contains("Protection removed") })
        );
        assert_eq!(
            app.protect.decrypted_path.as_deref(),
            Some(decrypted.as_path())
        );
        let reopened = DocumentSession::open(&decrypted).expect("opens without a password");
        assert_eq!(reopened.document().page_count().expect("page count"), 2);

        let wrong = dir.join("wrong.pdf");
        assert!(
            app.remove_protection_from(&protected, "wrong", &wrong)
                .is_err(),
            "a wrong removal password is a typed error"
        );
        assert!(!wrong.exists(), "a failed removal writes no file");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_passwords_are_a_typed_error() {
        let dir = temp_dir("empty");
        let mut app = app_with_excel();
        let error = app
            .protect_to_path(&dir.join("empty.pdf"))
            .expect_err("both passwords empty");
        assert!(!error.is_empty());
        assert!(app.status().starts_with("Protect failed:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn selected_permissions_follow_the_checkboxes() {
        let mut app = PdfxiApp::new();
        app.protect.allow_print = true;
        app.protect.allow_copy = false;
        app.protect.allow_annotate = false;
        app.protect.allow_forms = true;
        assert!(app.protect_selected_permission(Permission::PRINT));
        assert!(!app.protect_selected_permission(Permission::COPY));
        assert!(!app.protect_selected_permission(Permission::ANNOTATE));
        assert!(app.protect_selected_permission(Permission::FILL_FORMS));
    }
}
