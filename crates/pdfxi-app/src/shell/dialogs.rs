//! Open dialog, dismissible error banner, and open-error window.

use super::widgets::shell_panel_frame;
use crate::design::Palette;
use crate::ui::{AppCommand, CloseChoice, PdfxiApp};

impl PdfxiApp {
    /// Draw the in-app "Open PDF" path dialog.
    ///
    /// The shell links no native file picker, so this is intentionally a typed
    /// path plus drag-and-drop support. It mutates only in-memory state.
    pub(crate) fn open_dialog_ui(&mut self, _ctx: &egui::Context) {
        if !self.open_dialog_open {
            return;
        }
        // Consume the request first so a failed or cancelled picker cannot
        // reopen the dialog on the next frame.
        self.open_dialog_open = false;
        // The native picker is modal; it runs between frames, never inside a
        // paint callback, so the window cannot deadlock itself. Multi-select
        // opens each chosen file in its own tab.
        let chosen = rfd::FileDialog::new()
            .set_title("Open PDF")
            .add_filter("PDF documents", &["pdf"])
            .pick_files();
        if let Some(paths) = chosen {
            for path in paths {
                self.open_document(&path);
            }
        }
    }

    /// Draw the dismissible open-error banner above the command bar.
    pub(crate) fn error_banner_ui(&mut self, ctx: &egui::Context, palette: Palette) {
        let Some(message) = self.open_error_banner.clone() else {
            return;
        };
        egui::TopBottomPanel::top("shell_error")
            .frame(shell_panel_frame(palette, 14))
            .show(ctx, |ui| {
                ui.horizontal_centered(|ui| {
                    ui.label(egui::RichText::new(message).color(palette.accent));
                    ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                        if ui.button("Dismiss").clicked() {
                            self.dispatch(AppCommand::DismissOpenError, Some(ui.ctx()));
                        }
                    });
                });
            });
    }

    pub(crate) fn open_errors_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Recent document-open failures:");
        let messages: Vec<String> = self.open_errors.iter().map(ToString::to_string).collect();
        for message in messages {
            ui.label(message);
        }
        if ui.button("Clear").clicked() {
            self.dispatch(AppCommand::ClearOpenErrors, Some(ui.ctx()));
        }
    }

    /// Modal prompt for an encrypted document's open password.
    ///
    /// The field is masked, a wrong password keeps the modal open with the
    /// typed reason, and Cancel opens no tab.
    pub(crate) fn password_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(mut prompt) = self.password_prompt.take() else {
            return;
        };
        let mut submit = None;
        let mut cancel = false;
        egui::Window::new(format!("Password required - {}", prompt.title))
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label("This document is encrypted. Enter its open password.");
                let response = ui.add(
                    egui::TextEdit::singleline(&mut prompt.input)
                        .password(true)
                        .desired_width(240.0),
                );
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    submit = Some(prompt.input.clone());
                }
                if let Some(error) = &prompt.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        submit = Some(prompt.input.clone());
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                response.request_focus();
            });
        if submit.is_some() || cancel {
            self.apply_password_dialog(ctx, prompt, submit, cancel);
            return;
        }
        self.password_prompt = Some(prompt);
    }

    /// Apply the password modal's outcome.
    ///
    /// The prompt is put back before dispatching: `submit_password` and
    /// `cancel_password_prompt` read the pending path from `password_prompt`,
    /// and a wrong password must leave the modal open with its error. Without
    /// this the prompt was dropped first, so every submit closed the dialog
    /// without opening anything.
    pub(crate) fn apply_password_dialog(
        &mut self,
        ctx: &egui::Context,
        prompt: super::session::PasswordPrompt,
        submit: Option<String>,
        cancel: bool,
    ) {
        self.password_prompt = Some(prompt);
        if let Some(password) = submit {
            self.dispatch(AppCommand::SubmitPassword(password), Some(ctx));
            return;
        }
        if cancel {
            self.dispatch(AppCommand::CancelPasswordPrompt, Some(ctx));
        }
    }

    /// Modal save / discard / cancel prompt for a dirty tab.
    pub(crate) fn close_prompt_ui(&mut self, ctx: &egui::Context) {
        let Some(pending) = self.pending_close.take() else {
            return;
        };
        let mut choice = None;
        egui::Window::new("Unsaved changes")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("\"{}\" has unsaved changes.", pending.title));
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        choice = Some(CloseChoice::Save);
                    }
                    if ui.button("Discard").clicked() {
                        choice = Some(CloseChoice::Discard);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(CloseChoice::Cancel);
                    }
                });
            });
        if choice.is_none() {
            // Keyboard access to the modal: D discards, S saves and Escape
            // cancels. The buttons remain the primary pointer targets.
            choice = ctx.input(|input| {
                if input.key_pressed(egui::Key::D) {
                    Some(CloseChoice::Discard)
                } else if input.key_pressed(egui::Key::S) {
                    Some(CloseChoice::Save)
                } else if input.key_pressed(egui::Key::Escape) {
                    Some(CloseChoice::Cancel)
                } else {
                    None
                }
            });
        }
        // `resolve_close` consumes the pending prompt; put it back before
        // dispatching or every choice becomes a silent no-op.
        self.pending_close = Some(pending);
        if let Some(choice) = choice {
            self.dispatch(AppCommand::ResolveClose(choice), Some(ctx));
        }
    }

    /// Dialog offered when a save finds the file changed on disk.
    pub(crate) fn save_conflict_ui(&mut self, ctx: &egui::Context) {
        let Some(conflict) = self.save_conflict.take() else {
            return;
        };
        let mut choice = None;
        egui::Window::new("File changed on disk")
            .collapsible(false)
            .resizable(false)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!(
                    "{} changed after it was opened. Saving would overwrite those changes.",
                    conflict.path.display()
                ));
                ui.horizontal(|ui| {
                    if ui.button("Save As...").clicked() {
                        choice = Some(AppCommand::SaveAs);
                    }
                    if ui.button("Overwrite").clicked() {
                        choice = Some(AppCommand::OverwriteDocument);
                    }
                    if ui.button("Cancel").clicked() {
                        choice = Some(AppCommand::DismissSaveConflict);
                    }
                });
            });
        match choice {
            Some(command) => self.dispatch(command, Some(ctx)),
            None => self.save_conflict = Some(conflict),
        }
    }
}
