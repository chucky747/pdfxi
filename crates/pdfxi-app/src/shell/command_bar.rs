//! The document command bar.

use super::widgets::{shell_icon_button, shell_tool_button};
use crate::design::{Icon, Palette};
use crate::ui::{AppCommand, PdfxiApp};

impl PdfxiApp {
    /// The document command bar: only controls wired to real document state.
    pub(crate) fn document_bar_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let has_document = self.state.active_tab().is_some();
        ui.horizontal_centered(|ui| {
            if shell_tool_button(ui, palette, Icon::File, "Open", false).clicked() {
                self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
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
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shell_tool_button(ui, palette, Icon::Search, "Find", self.find_open).clicked() {
                    self.dispatch(AppCommand::ToggleFind, Some(ui.ctx()));
                }
            });
        });
    }
}

impl PdfxiApp {
    /// Render the document tab strip.
    ///
    /// Labels, order, and the active highlight come from the window-free
    /// [`Self::tab_strip_snapshot`]. Each tab also gets a small close button;
    /// close requests are deferred until after the `egui` closure so the
    /// workspace is never mutated while the UI borrows it. With no open tab the
    /// strip shows a fixed placeholder.
    #[allow(dead_code)]
    fn tabs_ui(&mut self, ui: &mut egui::Ui) {
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
