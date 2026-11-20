//! Help window: genuine local guide sections and the bundled docs folder.

use crate::ui::{AppCommand, PdfxiApp};

impl PdfxiApp {
    /// Draw the help window while it is open.
    pub(crate) fn help_window_ui(&mut self, ctx: &egui::Context) {
        if !self.help_open {
            return;
        }
        let mut open = self.help_open;
        egui::Window::new("PDFXI help")
            .open(&mut open)
            .resizable(false)
            .collapsible(false)
            .show(ctx, |ui| {
                ui.set_min_width(380.0);
                for (title, body) in [
                    (
                        "Getting started",
                        "Open a PDF from Home, Files, Recent, Starred or the Open button; \
                         you can also drop a file on the window. Each document gets its own tab.",
                    ),
                    (
                        "Keyboard shortcuts",
                        "Ctrl+O open, Ctrl+F find, Ctrl+W close the tab, Ctrl+Tab switch tabs, \
                         Ctrl+0 fit page, Ctrl+1 actual size, Ctrl+2 fit width, Ctrl+= / Ctrl+- zoom, \
                         Ctrl+R rotate, Ctrl+Shift+R rotate back.",
                    ),
                    (
                        "Themes and window controls",
                        "The gear menu switches between light, dark and the system theme, and \
                         between Windows and macOS caption buttons. The choice is remembered.",
                    ),
                    (
                        "Privacy: what leaves this machine",
                        "Nothing leaves this machine today. The library, recent files, stars and \
                         display name are local files. The optional AI assistant is not enabled in \
                         this build; when it is, it will connect only after you ask it to.",
                    ),
                ] {
                    ui.label(egui::RichText::new(title).strong());
                    ui.label(body);
                    ui.add_space(6.0);
                }
                if crate::ui::bundled_docs_dir().is_some()
                    && ui.button("Open user guide").clicked()
                {
                    self.dispatch(AppCommand::OpenDocsFolder, Some(ui.ctx()));
                }
            });
        self.help_open = open;
    }
}
