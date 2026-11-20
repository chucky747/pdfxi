//! The empty-document Home (empty state) surface.

use crate::design::{Icon, Palette, icon};
use crate::ui::{AppCommand, PdfxiApp};

impl PdfxiApp {
    /// Empty-document Home state: recent cards when available, else open CTA.
    pub(crate) fn empty_home_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let recents: Vec<_> = if self.active_rail == crate::ui::RailItem::Home {
            self.library.recent().iter().take(6).cloned().collect()
        } else {
            Vec::new()
        };
        if !recents.is_empty() {
            ui.vertical_centered(|ui| {
                ui.add_space(64.0);
                ui.label(
                    egui::RichText::new("Recent")
                        .size(18.0)
                        .strong()
                        .color(palette.text),
                );
                ui.add_space(12.0);
                ui.horizontal_wrapped(|ui| {
                    for entry in recents {
                        let title = if entry.is_missing() {
                            format!("{} (missing)", entry.title)
                        } else {
                            entry.title.clone()
                        };
                        let button = egui::Button::new(
                            egui::RichText::new(title).size(12.0).color(palette.text),
                        )
                        .fill(palette.surface)
                        .corner_radius(8.0)
                        .min_size(egui::vec2(190.0, 54.0));
                        if ui.add_enabled(!entry.is_missing(), button).clicked() {
                            self.dispatch(
                                AppCommand::OpenPath(entry.path.to_string_lossy().into_owned()),
                                Some(ui.ctx()),
                            );
                        }
                    }
                });
                ui.add_space(14.0);
                if ui.button("Open another PDF").clicked() {
                    self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
                }
            });
            return;
        }

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
