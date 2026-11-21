//! Right-hand tools panel with a search filter.

use crate::design::Palette;
use crate::ui::PdfxiApp;

use super::capabilities::{ToolRow, tool_rows_for};
use super::widgets::shell_tool_button;

impl PdfxiApp {
    /// The tools-panel rows for the open document.
    ///
    /// A capability alone is not enough: Fill & Sign is listed only when the
    /// active document actually carries an AcroForm, so the panel never offers
    /// a tool that could only report "no form fields". Place signature needs an
    /// open document for the same reason: placement edits the active page.
    /// Every other row is filtered by capability and by the search query.
    #[must_use]
    pub fn tools_panel_rows(&self) -> Vec<ToolRow> {
        let forms_available = self.forms_available();
        let document_open = self.state.active_tab_id().is_some();
        tool_rows_for(self.capabilities, &self.tools_search)
            .into_iter()
            .filter(|row| forms_available || row.command.label() != "fill-and-sign")
            .filter(|row| document_open || row.command.label() != "place-signature")
            .filter(|row| document_open || row.command.label() != "request-signature")
            .filter(|row| document_open || row.command.label() != "document-properties")
            .collect()
    }

    /// Draw the tools panel. Rows appear only when their capability is enabled
    /// and, for document-dependent tools, when the document supports them.
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

        let rows = self.tools_panel_rows();
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
