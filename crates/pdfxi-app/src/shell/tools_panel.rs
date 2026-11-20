//! Right-hand tools panel with a search filter.

use crate::design::Palette;
use crate::ui::PdfxiApp;

use super::capabilities::tool_rows_for;
use super::widgets::shell_tool_button;

impl PdfxiApp {
    /// Draw the tools panel. Rows appear only when their capability is enabled.
    pub(crate) fn tools_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Tools")
                    .size(13.0)
                    .color(palette.text_muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.add(
                    egui::TextEdit::singleline(&mut self.tools_search)
                        .hint_text("Search tools")
                        .desired_width(112.0),
                );
            });
        });
        ui.add_space(8.0);

        let rows = tool_rows_for(self.capabilities, &self.tools_search);
        let empty = rows.is_empty();
        for row in rows {
            if shell_tool_button(ui, palette, row.icon, row.label, false).clicked() {
                self.dispatch(row.command.clone(), Some(ui.ctx()));
            }
        }
        if empty {
            let message = if self.tools_search.trim().is_empty() {
                "Tools appear as features are enabled"
            } else {
                "No tools match your search"
            };
            ui.add(
                egui::Label::new(
                    egui::RichText::new(message)
                        .size(10.5)
                        .color(palette.text_muted),
                )
                .wrap_mode(egui::TextWrapMode::Extend),
            );
        }
    }
}
