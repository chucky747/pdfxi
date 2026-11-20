//! The product title bar: window controls, tabs, theme and settings.

use super::widgets::{
    CAPTION_BUTTON_WIDTH, CaptionAction, CaptionSide, caption_glyph, caption_layout,
    title_icon_button, traffic_light,
};
use crate::TabId;
use crate::design::{Icon, ThemeMode, TitlePalette, icon};
use crate::settings::{ThemePreference, WindowControlStyle};
use crate::ui::{AppCommand, PdfxiApp};

/// What a click on a title-bar tab means.
enum TabAction {
    /// Nothing was clicked.
    None,
    /// Activate this tab.
    Activate,
    /// Close this tab.
    Close,
}

impl PdfxiApp {
    pub(crate) fn title_bar_ui(&mut self, ui: &mut egui::Ui, palette: TitlePalette) {
        let panel = ui.max_rect();
        // Used only to draw the correct maximize/restore glyph; the caption
        // command itself re-reads this from the window through `dispatch`.
        let maximized = ui
            .ctx()
            .input(|input| input.viewport().maximized.unwrap_or(false));

        // The title bar is the window drag handle. Tabs and buttons are added
        // afterwards, so they win the pointer over this background interaction.
        let drag = ui.interact(
            panel,
            egui::Id::new("shell-title-drag"),
            egui::Sense::click_and_drag(),
        );
        let pointer_y = ui
            .ctx()
            .input(|input| input.pointer.hover_pos())
            .map_or(f32::MAX, |position| position.y);
        if drag.drag_started() && pointer_y > panel.top() + 6.0 {
            self.dispatch(AppCommand::StartWindowDrag, Some(ui.ctx()));
        }
        if drag.double_clicked() {
            self.dispatch(
                AppCommand::Caption(CaptionAction::ToggleMaximize),
                Some(ui.ctx()),
            );
        }

        let (side, actions) = caption_layout(self.window_controls);
        let mut caption_width = 0.0;
        if side == CaptionSide::Right {
            caption_width = actions.len() as f32 * CAPTION_BUTTON_WIDTH;
            for (index, action) in actions.iter().enumerate() {
                let left = panel.right() - (actions.len() - index) as f32 * CAPTION_BUTTON_WIDTH;
                let rect = egui::Rect::from_min_max(
                    egui::pos2(left, panel.top()),
                    egui::pos2(left + CAPTION_BUTTON_WIDTH, panel.bottom()),
                );
                let response = ui.interact(
                    rect,
                    egui::Id::new(("caption-button", action)),
                    egui::Sense::click(),
                );
                let close = *action == CaptionAction::Close;
                let hovered = response.hovered();
                if hovered {
                    ui.painter().rect_filled(
                        rect,
                        0.0,
                        if close {
                            egui::Color32::from_rgb(0xe8, 0x11, 0x23)
                        } else {
                            palette.hover
                        },
                    );
                }
                caption_glyph(
                    ui.painter(),
                    rect.center(),
                    *action,
                    maximized,
                    if close && hovered {
                        egui::Color32::WHITE
                    } else {
                        palette.text
                    },
                );
                if response.clicked() {
                    self.dispatch(AppCommand::Caption(*action), Some(ui.ctx()));
                }
            }
        }

        // Keep the ordinary title content clear of the caption strip so the
        // right-side buttons sit to the left of the window controls.
        let right_inset = if caption_width > 0.0 {
            caption_width + 6.0
        } else {
            12.0
        };
        let content = egui::Rect::from_min_max(
            egui::pos2(panel.left() + 12.0, panel.top()),
            egui::pos2(
                (panel.right() - right_inset).max(panel.left() + 12.0),
                panel.bottom(),
            ),
        );

        ui.scope_builder(egui::UiBuilder::new().max_rect(content), |ui| {
            ui.horizontal_centered(|ui| {
                if side == CaptionSide::Left {
                    // macOS traffic lights, driven through the same ordered
                    // caption model as the Windows caption buttons.
                    for action in actions {
                        let (fill, glyph, hover_text) = match action {
                            CaptionAction::Close => (
                                egui::Color32::from_rgb(0xff, 0x5f, 0x57),
                                Icon::Close,
                                "Close",
                            ),
                            CaptionAction::Minimize => (
                                egui::Color32::from_rgb(0xfe, 0xbc, 0x2e),
                                Icon::Minimize,
                                "Minimize",
                            ),
                            CaptionAction::ToggleMaximize => (
                                egui::Color32::from_rgb(0x28, 0xc8, 0x40),
                                Icon::Maximize,
                                "Maximize",
                            ),
                        };
                        if traffic_light(ui, fill, glyph, hover_text).clicked() {
                            self.dispatch(AppCommand::Caption(action), Some(ui.ctx()));
                        }
                    }
                    ui.add_space(10.0);
                }

                let (logo, _) =
                    ui.allocate_exact_size(egui::vec2(24.0, 24.0), egui::Sense::hover());
                ui.painter().rect_filled(logo, 6.0, palette.accent);
                icon(
                    ui.painter(),
                    logo.shrink(5.5),
                    Icon::File,
                    egui::Color32::WHITE,
                );
                ui.add_space(7.0);
                ui.label(
                    egui::RichText::new("PDFXI")
                        .size(13.5)
                        .strong()
                        .color(palette.text),
                );
                ui.add_space(14.0);

                let tabs = self.tab_strip_snapshot();
                let mut activate = None;
                let mut close_tab = None;
                for (index, (id, title, active)) in tabs.iter().enumerate() {
                    match self.title_tab(ui, palette, index, *id, title, *active) {
                        TabAction::None => {}
                        TabAction::Activate => activate = Some(*id),
                        TabAction::Close => close_tab = Some(*id),
                    }
                }
                if title_icon_button(ui, palette, Icon::Plus, 26.0)
                    .on_hover_text("Open a PDF")
                    .clicked()
                {
                    self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
                }
                if let Some(id) = activate {
                    self.dispatch(AppCommand::ActivateTab(id), Some(ui.ctx()));
                }
                if let Some(id) = close_tab {
                    self.dispatch(AppCommand::CloseTab(id), Some(ui.ctx()));
                }

                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let theme_glyph = match self.theme {
                        ThemeMode::Light => Icon::Moon,
                        ThemeMode::Dark => Icon::Sun,
                    };
                    if title_icon_button(ui, palette, theme_glyph, 26.0)
                        .on_hover_text("Switch light/dark")
                        .clicked()
                    {
                        self.dispatch(AppCommand::ToggleTheme, Some(ui.ctx()));
                    }
                    let gear =
                        title_icon_button(ui, palette, Icon::Gear, 26.0).on_hover_text("Settings");
                    let mut theme_choice = None;
                    let mut controls_choice = None;
                    egui::Popup::from_toggle_button_response(&gear)
                        .close_behavior(egui::PopupCloseBehavior::CloseOnClickOutside)
                        .show(|ui| {
                            ui.set_min_width(200.0);
                            ui.label(egui::RichText::new("Theme").strong());
                            for preference in [
                                ThemePreference::Light,
                                ThemePreference::Dark,
                                ThemePreference::System,
                            ] {
                                if ui
                                    .radio(self.theme_preference == preference, preference.label())
                                    .clicked()
                                {
                                    theme_choice = Some(preference);
                                }
                            }
                            ui.separator();
                            ui.label(egui::RichText::new("Window controls").strong());
                            for style in [WindowControlStyle::Windows, WindowControlStyle::MacOs] {
                                if ui
                                    .radio(self.window_controls == style, style.label())
                                    .clicked()
                                {
                                    controls_choice = Some(style);
                                }
                            }
                        });
                    if let Some(preference) = theme_choice {
                        self.dispatch(AppCommand::SetThemePreference(preference), Some(ui.ctx()));
                    }
                    if let Some(style) = controls_choice {
                        self.dispatch(AppCommand::SetWindowControls(style), Some(ui.ctx()));
                    }
                });
            });
        });

        // Draw the hairline last so caption hover fills cannot cover it.
        ui.painter().line_segment(
            [
                egui::pos2(panel.left(), panel.bottom()),
                egui::pos2(panel.right(), panel.bottom()),
            ],
            egui::Stroke::new(1.0_f32, palette.border),
        );
    }

    fn title_tab(
        &mut self,
        ui: &mut egui::Ui,
        palette: TitlePalette,
        index: usize,
        _id: TabId,
        title: &str,
        active: bool,
    ) -> TabAction {
        let mut label: String = title.chars().take(28).collect();
        if title.chars().count() > 28 {
            label.push('\u{2026}');
        }
        let font = egui::FontId::proportional(11.5);
        let galley = ui
            .painter()
            .layout_no_wrap(label.clone(), font.clone(), palette.text);
        let width = (galley.size().x + 54.0).clamp(132.0, 232.0);
        let (rect, response) =
            ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
        if active {
            ui.painter().rect_filled(rect, 8.0, palette.surface);
            ui.painter().rect_stroke(
                rect,
                8.0,
                egui::Stroke::new(1.0_f32, palette.border),
                egui::StrokeKind::Inside,
            );
        } else if response.hovered() {
            ui.painter().rect_filled(rect, 8.0, palette.hover);
        }
        icon(
            ui.painter(),
            egui::Rect::from_center_size(
                egui::pos2(rect.left() + 15.0, rect.center().y),
                egui::Vec2::splat(13.0),
            ),
            Icon::File,
            if active {
                palette.accent
            } else {
                palette.text_muted
            },
        );
        ui.painter().text(
            egui::pos2(rect.left() + 26.0, rect.center().y),
            egui::Align2::LEFT_CENTER,
            label,
            font,
            if active {
                palette.text
            } else {
                palette.text_muted
            },
        );
        if active || response.hovered() {
            let close_rect = egui::Rect::from_center_size(
                egui::pos2(rect.right() - 13.0, rect.center().y),
                egui::Vec2::splat(16.0),
            );
            let close_response = ui.interact(
                close_rect,
                egui::Id::new(("tab-close", index)),
                egui::Sense::click(),
            );
            if close_response.hovered() {
                ui.painter()
                    .circle_filled(close_rect.center(), 7.0, palette.hover);
            }
            icon(
                ui.painter(),
                close_rect.shrink(4.5),
                Icon::Close,
                palette.text_muted,
            );
            if close_response.clicked() {
                return TabAction::Close;
            }
        }
        if response.clicked() {
            return TabAction::Activate;
        }
        TabAction::None
    }
}

impl PdfxiApp {
    /// Resize the undecorated window when the pointer is near an edge.
    ///
    /// The window has no native frame, so the shell owns the edge hit zones.
    /// The zones are deliberately thin (6 pt) and only react to the primary
    /// button, matching the behaviour of a normal Windows frame. A maximized
    /// window has no draggable frame edges, so no resize is started there.
    pub(crate) fn window_resize_handles(&self, ctx: &egui::Context) {
        if ctx.input(|input| input.viewport().maximized.unwrap_or(false)) {
            return;
        }
        let Some(pointer) = ctx.input(|input| input.pointer.hover_pos()) else {
            return;
        };
        let rect = ctx.content_rect();
        let edge = 6.0;
        let left = pointer.x <= rect.left() + edge;
        let right = pointer.x >= rect.right() - edge;
        let top = pointer.y <= rect.top() + edge;
        let bottom = pointer.y >= rect.bottom() - edge;
        use egui::viewport::ResizeDirection;
        let direction = if left && top {
            Some(ResizeDirection::NorthWest)
        } else if right && top {
            Some(ResizeDirection::NorthEast)
        } else if left && bottom {
            Some(ResizeDirection::SouthWest)
        } else if right && bottom {
            Some(ResizeDirection::SouthEast)
        } else if left {
            Some(ResizeDirection::West)
        } else if right {
            Some(ResizeDirection::East)
        } else if top {
            Some(ResizeDirection::North)
        } else if bottom {
            Some(ResizeDirection::South)
        } else {
            None
        };
        let Some(direction) = direction else {
            return;
        };
        ctx.set_cursor_icon(match direction {
            ResizeDirection::NorthWest | ResizeDirection::SouthEast => egui::CursorIcon::ResizeNwSe,
            ResizeDirection::NorthEast | ResizeDirection::SouthWest => egui::CursorIcon::ResizeNeSw,
            ResizeDirection::North | ResizeDirection::South => egui::CursorIcon::ResizeVertical,
            ResizeDirection::East | ResizeDirection::West => egui::CursorIcon::ResizeHorizontal,
        });
        if ctx.input(|input| input.pointer.primary_pressed()) {
            ctx.send_viewport_cmd(egui::ViewportCommand::BeginResize(direction));
        }
    }
}
