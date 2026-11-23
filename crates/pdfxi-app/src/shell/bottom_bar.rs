//! Bottom document bar: page navigation, page box, zoom and full screen.

use crate::design::{Icon, Palette};
use crate::ui::{AppCommand, PdfxiApp};

use super::widgets::{shell_divider, shell_icon_button};

impl PdfxiApp {
    /// Draw the one-line status message reported by the last action.
    pub(crate) fn status_line_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.horizontal_centered(|ui| {
            ui.add(
                egui::Label::new(
                    egui::RichText::new(&self.status)
                        .size(11.5)
                        .color(palette.text),
                )
                .truncate(),
            );
        });
    }

    /// Draw the document bar that replaces the old floating page pill.
    pub(crate) fn bottom_bar_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let Some(tab) = self.state.active_tab() else {
            return;
        };
        let current = tab.current_page();
        let count = tab.page_count();
        let zoom_label = tab.zoom().scale().map_or_else(
            || "Fit".to_owned(),
            |scale| format!("{:.0}%", scale * 100.0),
        );
        let fullscreen = ui
            .ctx()
            .input(|input| input.viewport().fullscreen.unwrap_or(false));

        ui.horizontal_centered(|ui| {
            if shell_icon_button(ui, palette, Icon::ChevronLeft, 24.0, "Previous page")
                .on_hover_text("Previous page")
                .clicked()
            {
                self.dispatch(AppCommand::PreviousPage, Some(ui.ctx()));
            }

            if !self.page_box_focused {
                self.page_box = (current + 1).to_string();
            }
            let response = ui.add_sized(
                egui::vec2(42.0, 24.0),
                egui::TextEdit::singleline(&mut self.page_box)
                    .horizontal_align(egui::Align::Center),
            );
            response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::TextEdit, true, "Page number")
            });
            self.page_box_focused = response.has_focus();
            if response.lost_focus() {
                let text = self.page_box.clone();
                self.dispatch(AppCommand::SubmitPageNumber(text), Some(ui.ctx()));
            }
            ui.add_space(6.0);
            ui.label(
                egui::RichText::new(format!("of {}", count.max(1)))
                    .size(11.0)
                    .color(palette.text_muted),
            );
            ui.add_space(6.0);

            if shell_icon_button(ui, palette, Icon::ChevronRight, 24.0, "Next page")
                .on_hover_text("Next page")
                .clicked()
            {
                self.dispatch(AppCommand::NextPage, Some(ui.ctx()));
            }

            shell_divider(ui, palette);

            let zoom = egui::ComboBox::from_id_salt("shell-zoom")
                .selected_text(egui::RichText::new(zoom_label).size(10.5))
                .width(78.0)
                .show_ui(ui, |ui| {
                    for percent in [50_u32, 75, 100, 125, 150, 200] {
                        if ui.selectable_label(false, format!("{percent}%")).clicked() {
                            self.dispatch(AppCommand::SetZoomPercent(percent), Some(ui.ctx()));
                        }
                    }
                    ui.separator();
                    if ui.button("Fit page").clicked() {
                        self.dispatch(AppCommand::FitPage, Some(ui.ctx()));
                    }
                    if ui.button("Fit width").clicked() {
                        self.dispatch(AppCommand::FitWidth, Some(ui.ctx()));
                    }
                    if ui.button("Actual size").clicked() {
                        self.dispatch(AppCommand::ActualSize, Some(ui.ctx()));
                    }
                });
            zoom.response.widget_info(|| {
                egui::WidgetInfo::labeled(egui::WidgetType::ComboBox, true, "Zoom")
            });

            if shell_icon_button(ui, palette, Icon::Minus, 22.0, "Zoom out")
                .on_hover_text("Zoom out")
                .clicked()
            {
                self.dispatch(AppCommand::ZoomOut, Some(ui.ctx()));
            }
            if shell_icon_button(ui, palette, Icon::Plus, 22.0, "Zoom in")
                .on_hover_text("Zoom in")
                .clicked()
            {
                self.dispatch(AppCommand::ZoomIn, Some(ui.ctx()));
            }
            if shell_icon_button(ui, palette, Icon::Fullscreen, 22.0, "Full screen")
                .on_hover_text(if fullscreen {
                    "Exit full screen"
                } else {
                    "Full screen"
                })
                .clicked()
            {
                self.dispatch(AppCommand::ToggleFullscreen, Some(ui.ctx()));
            }
        });
    }
}
