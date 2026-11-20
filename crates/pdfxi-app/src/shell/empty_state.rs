//! The empty-document Home (empty state) surface.

use crate::design::{Icon, Palette, icon};
use crate::ui::{AppCommand, PdfxiApp};

impl PdfxiApp {
    /// Empty-document Home state: app mark, open action, drop hint, shortcut.
    pub(crate) fn empty_home_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space(96.0);
            let (mark, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
            ui.painter().rect_filled(mark, 14.0, palette.accent);
            icon(
                ui.painter(),
                mark.shrink(15.0),
                Icon::File,
                egui::Color32::WHITE,
            );
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Open a PDF to begin")
                    .size(20.0)
                    .strong()
                    .color(palette.text),
            );
            ui.add_space(14.0);
            let open =
                egui::Button::new(egui::RichText::new("Open a PDF").color(egui::Color32::WHITE))
                    .fill(palette.accent)
                    .corner_radius(8.0)
                    .min_size(egui::vec2(150.0, 36.0));
            if ui.add(open).clicked() {
                self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new("or drop a file here").color(palette.text_muted));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Ctrl+O")
                    .monospace()
                    .color(palette.text_muted),
            );
        });
    }
}
