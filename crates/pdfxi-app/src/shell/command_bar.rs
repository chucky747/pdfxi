//! Document command bar and the legacy tab strip.

use crate::design::{Icon, Palette};
use crate::ui::{AppCommand, PdfxiApp};

use super::capabilities::command_bar_features;
use super::widgets::{shell_icon_button, shell_tool_button};

impl PdfxiApp {
    /// The document command bar: only controls backed by a real command.
    pub(crate) fn document_bar_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let has_document = self.state.active_tab().is_some();
        ui.horizontal_centered(|ui| {
            if shell_tool_button(ui, palette, Icon::File, "Open", false).clicked() {
                self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
            }
            if shell_tool_button(ui, palette, Icon::Print, "Print", false)
                .on_hover_text("Print (Ctrl+P)")
                .clicked()
            {
                self.dispatch(AppCommand::Print, Some(ui.ctx()));
            }
            // Undo and redo exist only when the active session can use them.
            if self.can_undo()
                && shell_tool_button(ui, palette, Icon::ArrowLeft, "Undo", false)
                    .on_hover_text("Undo (Ctrl+Z)")
                    .clicked()
            {
                self.dispatch(AppCommand::Undo, Some(ui.ctx()));
            }
            if self.can_redo()
                && shell_tool_button(ui, palette, Icon::ArrowRight, "Redo", false)
                    .on_hover_text("Redo (Ctrl+Y)")
                    .clicked()
            {
                self.dispatch(AppCommand::Redo, Some(ui.ctx()));
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(has_document, |ui| {
                if shell_icon_button(ui, palette, Icon::ArrowLeft, 30.0)
                    .on_hover_text("Rotate counter-clockwise")
                    .clicked()
                {
                    self.dispatch(AppCommand::RotateCounterclockwise, Some(ui.ctx()));
                }
                if shell_icon_button(ui, palette, Icon::ArrowRight, 30.0)
                    .on_hover_text("Rotate clockwise")
                    .clicked()
                {
                    self.dispatch(AppCommand::RotateClockwise, Some(ui.ctx()));
                }
                if shell_tool_button(ui, palette, Icon::FitPage, "Fit", false)
                    .on_hover_text("Fit the page in the window")
                    .clicked()
                {
                    self.dispatch(AppCommand::FitPage, Some(ui.ctx()));
                }
                if shell_tool_button(ui, palette, Icon::Fullscreen, "Actual", false)
                    .on_hover_text("Actual size")
                    .clicked()
                {
                    self.dispatch(AppCommand::ActualSize, Some(ui.ctx()));
                }
            });

            // Feature buttons exist only when their capability is enabled.
            for feature in command_bar_features(self.capabilities) {
                if shell_tool_button(ui, palette, feature.icon, feature.label, false).clicked() {
                    self.dispatch(feature.command.clone(), Some(ui.ctx()));
                }
            }

            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shell_tool_button(ui, palette, Icon::Organize, "Tools", self.tools_open)
                    .on_hover_text("Show or hide the tools panel")
                    .clicked()
                {
                    self.dispatch(AppCommand::ToggleTools, Some(ui.ctx()));
                }
                if shell_tool_button(ui, palette, Icon::Search, "Find", self.find_open).clicked() {
                    self.dispatch(AppCommand::ToggleFind, Some(ui.ctx()));
                }
            });
        });
    }

    /// Render the legacy document tab strip (kept for headless layout tests).
    #[allow(dead_code)]
    pub(crate) fn tabs_ui(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.tab_strip_snapshot();
        if snapshot.is_empty() {
            ui.label("No documents open");
            return;
        }
        let mut activate = None;
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (id, label, is_active) in snapshot {
                if ui.selectable_label(is_active, label).clicked() {
                    activate = Some(id);
                }
                if ui.small_button("-").clicked() {
                    close = Some(id);
                }
            }
        });
        if let Some(id) = activate {
            self.dispatch(AppCommand::ActivateTab(id), Some(ui.ctx()));
        }
        if let Some(id) = close {
            self.dispatch(AppCommand::CloseTab(id), Some(ui.ctx()));
        }
    }
}
