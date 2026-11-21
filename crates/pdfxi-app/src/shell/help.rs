//! Help window: genuine local guide sections and the bundled docs folder.

use crate::ui::{AppCommand, PdfxiApp};

/// The "Privacy: what leaves this machine" help section.
///
/// Kept as one string so the wording can be tested against the live assistant
/// design (ADR 0013): the assistant connects only on demand and sends the
/// question plus a bounded, fenced document excerpt to the local service.
pub(crate) const PRIVACY_TEXT: &str = "The library, recent files, stars and display name are local files. The AI assistant is \
     live and connects only when you ask it to. Each question is sent with a bounded excerpt of \
     the open document (at most 24,000 characters, with the page range) to the local OpenCode \
     service, and to no other network destination. The assistant session has no shell, file or \
     network tools, and the excerpt is marked as untrusted data. You can disconnect at any time \
     from the assistant panel.";

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
                    ("Privacy: what leaves this machine", PRIVACY_TEXT),
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

#[cfg(test)]
mod tests {
    use super::PRIVACY_TEXT;

    #[test]
    fn privacy_text_describes_the_live_bounded_excerpt_path() {
        let text = PRIVACY_TEXT.to_ascii_lowercase();
        for phrase in [
            "is live",
            "connects only when you ask",
            "24,000 characters",
            "page range",
            "local opencode service",
            "no other network destination",
            "no shell, file or network tools",
            "disconnect at any time",
        ] {
            assert!(text.contains(phrase), "privacy text is missing {phrase:?}");
        }
        assert!(
            !text.contains("not enabled in this build"),
            "the assistant is live; the old placeholder wording must be gone"
        );
        assert!(!text.contains("nothing leaves this machine today"));
    }
}
