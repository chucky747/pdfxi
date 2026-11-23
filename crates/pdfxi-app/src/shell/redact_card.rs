//! Redact and sanitize card (O-06 / A-30 redact half).
//!
//! Redaction applies the current text selection as real content removal on a
//! cloned session document, verifies the saved bytes against the selected
//! secret, writes a full-rewrite `<stem>-redacted.pdf` beside the source (never
//! over it), and opens the result. Sanitization uses the security crate's
//! verified full rewrite and reports the removed categories.

use std::path::{Path, PathBuf};

use pdfxi_edit::Rect;
use pdfxi_edit::redact_document::{
    RedactionMark, RedactionReport, RedactionRequest, apply_redaction, verify_redaction,
};
use pdfxi_security::sanitize_verified::{
    SanitizeCategory, SanitizeRequest, sanitize_document_full,
};

use crate::ui::PdfxiApp;

/// State of the Redact and sanitize window.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RedactUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// Keep images instead of redacting intersecting pixels.
    pub(crate) keep_images: bool,
    /// Remove annotations that intersect a mark.
    pub(crate) remove_annotations: bool,
    /// Remove links that intersect a mark.
    pub(crate) remove_links: bool,
    /// Remove form fields that intersect a mark.
    pub(crate) remove_form_fields: bool,
    /// Remove `/Info` metadata during redaction.
    pub(crate) remove_metadata: bool,
    /// Typed error from the last operation.
    pub(crate) error: Option<String>,
    /// Status line for the card.
    pub(crate) result: Option<String>,
    /// Verification summary of the last redaction.
    pub(crate) verified: Option<String>,
}

impl Default for RedactUiState {
    fn default() -> Self {
        let defaults = RedactionRequest::default();
        Self {
            open: false,
            keep_images: defaults.keep_images,
            remove_annotations: defaults.remove_annotations,
            remove_links: defaults.remove_links,
            remove_form_fields: defaults.remove_form_fields,
            remove_metadata: defaults.remove_metadata,
            error: None,
            result: None,
            verified: None,
        }
    }
}

impl PdfxiApp {
    /// Handle [`AppCommand::Redact`]: explain the missing document or open the
    /// Redact and sanitize window.
    pub(crate) fn handle_redact_command(&mut self) {
        if self.state.active_tab_id().is_none() {
            self.status = "Open a document to redact".to_owned();
            return;
        }
        self.redact.open = true;
        self.redact.error = None;
        self.redact.result = None;
        self.status = "Redact: select text, then redact the selection".to_owned();
    }

    /// Whether the Redact window is open (harness accessor).
    #[must_use]
    pub fn redact_card_open(&self) -> bool {
        self.redact.open
    }

    /// Draw the Redact and sanitize window.
    pub(crate) fn redact_card_ui(&mut self, ctx: &egui::Context) {
        if !self.redact.open {
            return;
        }
        let has_selection = !self.selection_quads().is_empty();
        let mut open = true;
        let mut redact = false;
        let mut sanitize = false;

        egui::Window::new("Redact and sanitize")
            .open(&mut open)
            .resizable(true)
            .default_width(400.0)
            .show(ctx, |ui| {
                ui.heading("Redact selection");
                ui.label(
                    egui::RichText::new(
                        "Select the text to remove on the page first. The redacted copy is \
                         written beside the source; the original is never overwritten.",
                    )
                    .size(10.0)
                    .weak(),
                );
                if ui
                    .add_enabled(has_selection, egui::Button::new("Redact current selection"))
                    .on_disabled_hover_text("Select text on the page first")
                    .clicked()
                {
                    redact = true;
                }
                ui.label("Cleanup");
                ui.checkbox(&mut self.redact.keep_images, "Keep images");
                ui.checkbox(
                    &mut self.redact.remove_annotations,
                    "Remove intersecting annotations",
                );
                ui.checkbox(&mut self.redact.remove_links, "Remove intersecting links");
                ui.checkbox(
                    &mut self.redact.remove_form_fields,
                    "Remove intersecting form fields",
                );
                ui.checkbox(&mut self.redact.remove_metadata, "Remove /Info metadata");

                ui.separator();
                ui.heading("Sanitize");
                ui.label(
                    egui::RichText::new(
                        "Removes metadata, XMP, attachments, JavaScript, optional content \
                         and thumbnails in a verified rewrite.",
                    )
                    .size(10.0)
                    .weak(),
                );
                if ui.button("Sanitize and save as...").clicked() {
                    sanitize = true;
                }

                if let Some(verified) = &self.redact.verified {
                    ui.label(verified);
                }
                if let Some(result) = &self.redact.result {
                    ui.label(result);
                }
                if let Some(error) = &self.redact.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });

        if redact {
            let quads = self.selection_quads();
            let secret = self.selected_text();
            let secrets: Vec<&str> = if secret.is_empty() {
                Vec::new()
            } else {
                vec![secret.as_str()]
            };
            if let Some(path) = self.redacted_output_path() {
                let _ = self.redact_quads_to_path(&quads, &secrets, &path);
            }
        }
        if sanitize && let Some(path) = self.sanitized_output_path() {
            let _ = self.sanitize_to_path(&path);
        }
        if !self.redact.open {
            return;
        }
        if !open {
            self.redact.open = false;
        }
    }

    /// `<stem>-redacted.pdf` next to the active document.
    pub(crate) fn redacted_output_path(&self) -> Option<PathBuf> {
        self.active_document_path().map(|path| {
            path.with_file_name(format!(
                "{}-redacted.pdf",
                path.file_stem().map_or_else(
                    || "document".to_owned(),
                    |stem| stem.to_string_lossy().into_owned()
                )
            ))
        })
    }

    /// `<stem>-sanitized.pdf` next to the active document.
    pub(crate) fn sanitized_output_path(&self) -> Option<PathBuf> {
        self.active_document_path().map(|path| {
            path.with_file_name(format!(
                "{}-sanitized.pdf",
                path.file_stem().map_or_else(
                    || "document".to_owned(),
                    |stem| stem.to_string_lossy().into_owned()
                )
            ))
        })
    }

    /// Redact `quads` on a clone, verify with `secrets`, and write `path`.
    ///
    /// An empty mark list is a typed error and writes nothing. The source tab is
    /// untouched; the redacted copy is a full rewrite opened as a new tab.
    pub(crate) fn redact_quads_to_path(
        &mut self,
        quads: &[(usize, Vec<Rect>)],
        secrets: &[&str],
        path: &Path,
    ) -> Result<RedactionReport, String> {
        let result = self.write_redacted(quads, secrets, path);
        match result {
            Ok(report) => {
                self.redact.error = None;
                self.redact.result = Some(format!("Redacted copy saved to {}", path.display()));
                self.status = self
                    .redact
                    .verified
                    .clone()
                    .unwrap_or_else(|| "Redaction complete".to_owned());
                self.notify_real_event(
                    "Selection redacted",
                    &path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                );
                let _ = self.open_document(path);
                Ok(report)
            }
            Err(error) => {
                self.redact.error = Some(error.clone());
                self.redact.result = None;
                self.redact.verified = None;
                self.status = format!("Redaction failed: {error}");
                Err(error)
            }
        }
    }

    /// Apply, verify and write the redacted copy.
    fn write_redacted(
        &mut self,
        quads: &[(usize, Vec<Rect>)],
        secrets: &[&str],
        path: &Path,
    ) -> Result<RedactionReport, String> {
        let marks: Vec<RedactionMark> = quads
            .iter()
            .flat_map(|(page, rects)| {
                rects
                    .iter()
                    .map(move |rect| RedactionMark::new(*page, *rect))
            })
            .collect();
        if marks.is_empty() {
            return Err("Select text to redact".to_owned());
        }
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to redact".to_owned());
        };
        let session = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?;
        if session.decryption_report().is_some() {
            return Err(
                "Redacting a password-protected document is not supported: the redacted copy \
                 would be saved without encryption. Remove protection first and redact the copy."
                    .to_owned(),
            );
        }
        let mut candidate = session.document().clone();
        let request = RedactionRequest {
            marks,
            keep_images: self.redact.keep_images,
            remove_annotations: self.redact.remove_annotations,
            remove_links: self.redact.remove_links,
            remove_form_fields: self.redact.remove_form_fields,
            remove_metadata: self.redact.remove_metadata,
        };
        let report =
            apply_redaction(&mut candidate, &request).map_err(|error| error.to_string())?;
        let verification =
            verify_redaction(&candidate, secrets, &report).map_err(|error| error.to_string())?;
        if !verification.clean {
            // Fail closed: a file that still contains the removed content must
            // never be written and presented as a finished redaction.
            let details = verification
                .survivors
                .iter()
                .map(|survivor| survivor.detail.clone())
                .collect::<Vec<_>>()
                .join("; ");
            return Err(format!(
                "Redaction verification found {} survivor(s): {details}. No file was written.",
                verification.survivors.len()
            ));
        }
        let bytes = candidate.save().map_err(|error| error.to_string())?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, bytes)
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;

        let mut pages: Vec<usize> = report.marks.iter().map(|mark| mark.page).collect();
        pages.sort_unstable();
        pages.dedup();
        self.redact.verified = Some(format!(
            "Redacted {} area(s) on {} page(s); verification: clean",
            report.marks.len(),
            pages.len()
        ));
        Ok(report)
    }

    /// Sanitize the active document with all categories and write `path`.
    ///
    /// Returns the number of removed report items.
    pub(crate) fn sanitize_to_path(&mut self, path: &Path) -> Result<usize, String> {
        let result = self.write_sanitized(path);
        match result {
            Ok(removed) => {
                self.redact.error = None;
                self.redact.result = Some(format!("Sanitized copy saved to {}", path.display()));
                self.status = self
                    .redact
                    .result
                    .clone()
                    .unwrap_or_else(|| "Sanitized".to_owned());
                self.notify_real_event(
                    "PDF sanitized",
                    &path.file_name().map_or_else(
                        || path.display().to_string(),
                        |name| name.to_string_lossy().into_owned(),
                    ),
                );
                let _ = self.open_document(path);
                Ok(removed)
            }
            Err(error) => {
                self.redact.error = Some(error.clone());
                self.redact.result = None;
                self.status = format!("Sanitize failed: {error}");
                Err(error)
            }
        }
    }

    /// Run the sanitizer and write its bytes.
    fn write_sanitized(&mut self, path: &Path) -> Result<usize, String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to sanitize".to_owned());
        };
        let session = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?;
        if session.decryption_report().is_some() {
            return Err(
                "Sanitizing a password-protected document is not supported: the sanitized copy \
                 would be saved without encryption. Remove protection first and sanitize the copy."
                    .to_owned(),
            );
        }
        let sanitized = sanitize_document_full(session.document(), &SanitizeRequest::all())
            .map_err(|error| error.to_string())?;
        if let Some(parent) = path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
        {
            std::fs::create_dir_all(parent)
                .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
        }
        std::fs::write(path, sanitized.bytes())
            .map_err(|error| format!("Could not write {}: {error}", path.display()))?;
        let report = sanitized.report();
        let mut categories: Vec<SanitizeCategory> =
            report.removed.iter().map(|item| item.category()).collect();
        categories.sort_unstable();
        categories.dedup();
        let removed = report.removed.len();
        self.redact.verified = Some(if categories.is_empty() {
            "Sanitized; no selected item was present".to_owned()
        } else {
            format!(
                "Sanitized; removed {} item(s) [{}]",
                removed,
                categories
                    .iter()
                    .map(|category| category.as_id())
                    .collect::<Vec<_>>()
                    .join(", ")
            )
        });
        Ok(removed)
    }

    /// Path of the active session document.
    fn active_document_path(&self) -> Option<PathBuf> {
        let id = self.state.active_tab_id()?;
        self.sessions
            .get(&id)
            .and_then(|session| session.path())
            .map(Path::to_path_buf)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_core::{Document, DocumentInfo};
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/redact-card")
            .join(format!("{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the redact directory");
        dir
    }

    #[test]
    fn redacting_the_whole_page_removes_the_secret_text() {
        let dir = temp_dir("redact");
        let output = dir.join("redacted.pdf");
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        let id = app.state.active_tab_id().expect("the fixture opens");
        let document = app.sessions.get(&id).expect("session").document();
        let text = document.page_text(0).expect("page text");
        // Pick the longest page word; short header words such as "PDFXI" also
        // occur in metadata/XMP, where the verifier would honestly report a
        // survivor even though the page text is gone.
        let secret = text
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|word| word.len() >= 6)
            .max_by_key(|word| word.len())
            .expect("a distinctive page word")
            .to_owned();
        let page = document.page(0).expect("page 0");
        let rect = Rect::new(0.0, 0.0, page.width() as f32, page.height() as f32)
            .expect("a full-page rect");

        let report = app
            .redact_quads_to_path(&[(0, vec![rect])], &[secret.as_str()], &output)
            .expect("redact the page");
        assert_eq!(report.marks.len(), 1);
        assert!(
            app.redact
                .verified
                .as_deref()
                .is_some_and(|summary| summary.contains("verification: clean")),
            "unexpected verification: {:?}",
            app.redact.verified
        );
        assert!(output.is_file());

        let bytes = std::fs::read(&output).expect("read the redacted file");
        let reopened = Document::from_bytes(bytes).expect("reopen the redacted file");
        assert_eq!(reopened.page_count().expect("page count"), 2);
        let redacted_text = reopened.page_text(0).expect("redacted page text");
        assert!(
            !redacted_text.contains(&secret),
            "the secret survived redaction: {secret:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn empty_selection_is_typed_and_writes_nothing() {
        let dir = temp_dir("empty");
        let output = dir.join("empty-redacted.pdf");
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        let error = app
            .redact_quads_to_path(&[], &[], &output)
            .expect_err("an empty mark list must fail");
        assert_eq!(error, "Select text to redact");
        assert!(!output.exists());
        assert!(app.status().starts_with("Redaction failed:"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn a_failed_verification_writes_no_file() {
        let dir = temp_dir("survivor");
        let output = dir.join("must-not-exist.pdf");
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        let id = app.state.active_tab_id().expect("the fixture opens");
        let document = app.sessions.get(&id).expect("session").document();
        let secret = document
            .page_text(0)
            .expect("page text")
            .split_whitespace()
            .map(|word| word.trim_matches(|c: char| !c.is_ascii_alphanumeric()))
            .filter(|word| word.len() >= 6)
            .max_by_key(|word| word.len())
            .expect("a distinctive page word")
            .to_owned();
        // A tiny mark in an empty corner removes nothing, so the verifier must
        // still find the secret and the card must refuse to write a file.
        let corner = Rect::new(0.0, 0.0, 2.0, 2.0).expect("a tiny rect");
        let error = app
            .redact_quads_to_path(&[(0, vec![corner])], &[secret.as_str()], &output)
            .expect_err("a surviving secret must fail closed");
        assert!(
            error.contains("No file was written"),
            "unexpected error: {error}"
        );
        assert!(!output.exists(), "a failed redaction must not write output");
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn sanitizing_removes_info_metadata_and_keeps_the_pages() {
        let dir = temp_dir("sanitize");
        let input = dir.join("with-title.pdf");
        let output = dir.join("sanitized.pdf");
        let source = std::fs::read(fixture()).expect("fixture bytes");
        let mut document = Document::from_bytes(source).expect("fixture parses");
        document
            .set_info(&DocumentInfo {
                title: Some("Confidential Title".to_owned()),
                author: Some("Confidential Author".to_owned()),
                ..DocumentInfo::default()
            })
            .expect("write Info");
        std::fs::write(&input, document.save().expect("save")).expect("write the input");

        let mut app = PdfxiApp::new();
        app.open_document(&input);
        let removed = app.sanitize_to_path(&output).expect("sanitize");
        assert!(
            removed >= 2,
            "two Info fields should be removed, got {removed}"
        );
        assert!(
            app.redact
                .verified
                .as_deref()
                .is_some_and(|summary| summary.contains("document_info")),
            "unexpected sanitize summary: {:?}",
            app.redact.verified
        );

        let bytes = std::fs::read(&output).expect("read the sanitized file");
        let reopened = Document::from_bytes(bytes).expect("reopen sanitized");
        assert_eq!(reopened.page_count().expect("page count"), 2);
        let info = reopened.info().expect("Info read").unwrap_or_default();
        assert_eq!(info.title, None, "the Title survived sanitization");
        assert_eq!(info.author, None, "the Author survived sanitization");
        let _ = std::fs::remove_dir_all(&dir);
    }
}
