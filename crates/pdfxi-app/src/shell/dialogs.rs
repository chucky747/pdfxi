//! Open dialog, dismissible error banner, and open-error window.

use super::widgets::shell_panel_frame;
use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};

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
}
