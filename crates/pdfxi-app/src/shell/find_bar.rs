//! The in-document find bar.

use crate::ui::{AppCommand, PdfxiApp};

impl PdfxiApp {
    /// Render the in-document find bar.
    ///
    /// The query field mirrors into [`crate::FindState`] on change; the two
    /// toggles rebuild the search with new options; Next/Previous navigate with
    /// wrap-around and request a scroll to the current match.
    pub(crate) fn find_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Find:");
            let response = ui.text_edit_singleline(&mut self.find_query);
            if response.changed() {
                let query = self.find_query.clone();
                self.dispatch(AppCommand::SetFindQuery(query), Some(ui.ctx()));
            }
            if ui.checkbox(&mut self.find_case, "Match case").changed() {
                self.dispatch(
                    AppCommand::SetFindOptions {
                        case_sensitive: self.find_case,
                        whole_word: self.find_word,
                    },
                    Some(ui.ctx()),
                );
            }
            if ui.checkbox(&mut self.find_word, "Whole word").changed() {
                self.dispatch(
                    AppCommand::SetFindOptions {
                        case_sensitive: self.find_case,
                        whole_word: self.find_word,
                    },
                    Some(ui.ctx()),
                );
            }
            if ui.button("Previous").clicked() {
                self.dispatch(AppCommand::FindPrevious, Some(ui.ctx()));
            }
            if ui.button("Next").clicked() {
                self.dispatch(AppCommand::FindNext, Some(ui.ctx()));
            }
            ui.label(self.find.match_summary());
            if ui.button("Close").clicked() {
                self.dispatch(AppCommand::CloseFind, Some(ui.ctx()));
            }
        });
    }
}
