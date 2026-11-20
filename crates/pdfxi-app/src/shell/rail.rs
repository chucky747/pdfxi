//! Left icon rail. Items appear only for enabled capabilities.

use crate::design::{Icon, Palette, icon};
use crate::ui::{AppCommand, PdfxiApp};

use super::capabilities::rail_items;

impl PdfxiApp {
    /// Draw the rail: logo, enabled navigation items, AI Assistant last.
    pub(crate) fn rail_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.vertical_centered(|ui| {
            let (logo, _) = ui.allocate_exact_size(egui::vec2(30.0, 30.0), egui::Sense::hover());
            ui.painter().rect_filled(logo, 8.0, palette.accent);
            icon(
                ui.painter(),
                logo.shrink(6.5),
                Icon::File,
                egui::Color32::WHITE,
            );
            ui.add_space(12.0);

            for item in rail_items(self.capabilities) {
                let selected = self.active_rail == item;
                let badge = if item == super::capabilities::RailItem::Comments {
                    self.comments_badge()
                } else {
                    None
                };
                if rail_item(ui, palette, item.icon(), item.label(), selected, badge).clicked() {
                    self.dispatch(AppCommand::ShowRail(item), Some(ui.ctx()));
                }
                ui.add_space(4.0);
            }
        });
    }
}

/// One icon-and-label rail entry.
fn rail_item(
    ui: &mut egui::Ui,
    palette: Palette,
    glyph: Icon,
    label: &str,
    selected: bool,
    badge: Option<usize>,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(56.0, 50.0), egui::Sense::click());
    let background = if selected {
        palette.accent_soft
    } else if response.hovered() {
        palette.surface
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, background);
    let color = if selected {
        palette.accent
    } else {
        palette.text_muted
    };
    icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.center().x, rect.top() + 17.0),
            egui::Vec2::splat(16.0),
        ),
        glyph,
        color,
    );
    ui.painter().text(
        egui::pos2(rect.center().x, rect.bottom() - 7.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        egui::FontId::proportional(9.0),
        color,
    );
    if let Some(count) = badge {
        let center = egui::pos2(rect.right() - 8.0, rect.top() + 8.0);
        ui.painter().circle_filled(center, 7.0, palette.accent);
        ui.painter().text(
            center,
            egui::Align2::CENTER_CENTER,
            count.to_string(),
            egui::FontId::proportional(9.0),
            egui::Color32::WHITE,
        );
    }
    response
}
