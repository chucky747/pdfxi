//! Left side panel: page thumbnails, Recent, Home, and future rail content.

use crate::design::{Icon, Palette};
use crate::ui::{AppCommand, PdfxiApp};
#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, thumbnail::ThumbnailSpec};

use super::capabilities::{Capability, RailItem};
use super::widgets::shell_icon_button;

impl PdfxiApp {
    /// Draw the side panel selected by the rail.
    pub(crate) fn side_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        match self.active_rail {
            RailItem::Pages => self.pages_panel_ui(ui, palette),
            RailItem::Recent => self.recent_panel_ui(ui, palette),
            RailItem::Home => self.home_panel_ui(ui, palette),
            RailItem::Files => self.future_panel(
                ui,
                palette,
                "Files",
                "The local library appears when Files is enabled",
            ),
            RailItem::Starred => self.future_panel(
                ui,
                palette,
                "Starred",
                "Starred documents appear when Starred is enabled",
            ),
            RailItem::Shared => self.future_panel(
                ui,
                palette,
                "Shared",
                "The shared folder appears when Shared is enabled",
            ),
            RailItem::Comments => self.future_panel(
                ui,
                palette,
                "Comments",
                "Comments appear when commenting is enabled",
            ),
            RailItem::AiAssistant => self.future_panel(
                ui,
                palette,
                "AI Assistant",
                "The assistant appears when it is connected",
            ),
        }
    }

    /// Pages panel: header, grid/list toggle, optional organise actions.
    fn pages_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new(format!("Pages ({page_count})"))
                    .size(13.0)
                    .color(palette.text_muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if self.capabilities.contains(Capability::Export) {
                    if shell_icon_button(ui, palette, Icon::More, 22.0)
                        .on_hover_text("Page options")
                        .clicked()
                    {
                        self.dispatch(AppCommand::PageMenu, Some(ui.ctx()));
                    }
                    if shell_icon_button(ui, palette, Icon::Plus, 22.0)
                        .on_hover_text("Add page")
                        .clicked()
                    {
                        self.dispatch(AppCommand::AddPage, Some(ui.ctx()));
                    }
                }
                let glyph = if self.grid_view {
                    Icon::List
                } else {
                    Icon::Grid
                };
                if shell_icon_button(ui, palette, glyph, 22.0)
                    .on_hover_text("Grid or list view")
                    .clicked()
                {
                    self.dispatch(AppCommand::ToggleGrid, Some(ui.ctx()));
                }
            });
        });
        ui.add_space(6.0);
        if page_count == 0 {
            ui.label(egui::RichText::new("Open a document to see pages").color(palette.text_muted));
            return;
        }
        self.thumbnails_ui(ui);
    }

    /// Recent panel; the persisted list arrives with T-0221.
    fn recent_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(egui::RichText::new("Recent").strong().size(13.0));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Recent files appear after you open documents")
                .color(palette.text_muted),
        );
    }

    /// Home panel: a short recent hint, no second open action.
    fn home_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(egui::RichText::new("Home").strong().size(13.0));
        ui.add_space(6.0);
        ui.label(
            egui::RichText::new("Recent files appear after you open documents")
                .color(palette.text_muted),
        );
    }

    /// Placeholder for a rail entry whose feature has not landed yet.
    fn future_panel(&mut self, ui: &mut egui::Ui, palette: Palette, title: &str, message: &str) {
        ui.label(egui::RichText::new(title).strong().size(13.0));
        ui.add_space(6.0);
        ui.label(egui::RichText::new(message).color(palette.text_muted));
    }

    /// Generate the visible thumbnails, then draw them as a list or a grid.
    fn thumbnails_ui(&mut self, ui: &mut egui::Ui) {
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        self.ensure_thumbnails(ui, page_count);
        if self.grid_view {
            self.thumbnail_grid_ui(ui, page_count);
        } else {
            self.thumbnail_list_ui(ui, page_count);
        }
    }

    /// Enqueue and generate a bounded number of visible thumbnails.
    fn ensure_thumbnails(&mut self, ui: &mut egui::Ui, page_count: usize) {
        if self.thumbs.page_count() != page_count {
            self.thumbs.reset(page_count);
            #[cfg(feature = "pdfium")]
            self.thumb_textures.clear();
        }
        self.thumbs.set_row_height(crate::ui::THUMB_ROW_HEIGHT);
        self.thumbs
            .set_viewport_height(ui.available_height().max(1.0));

        #[cfg(feature = "pdfium")]
        {
            if let Some(renderer) = self.pdfium_renderer.as_ref() {
                let spec = ThumbnailSpec::new(crate::ui::THUMB_MAX_SIDE, crate::ui::THUMB_MAX_SIDE);
                let _ = self.thumbs.enqueue_visible(&spec);
                let token = CancellationToken::new();
                for _ in 0..crate::ui::THUMBS_PER_FRAME {
                    match self
                        .thumbs
                        .generate_pending(renderer, &spec, 1, &token, &mut |_| {})
                    {
                        Ok(0) => break,
                        Ok(_) => {}
                        Err(error) => {
                            self.status = format!("Thumbnail error: {error}");
                            break;
                        }
                    }
                }
            }
        }
    }

    /// Draw the vertical thumbnail list.
    fn thumbnail_list_ui(&mut self, ui: &mut egui::Ui, page_count: usize) {
        let active = self.thumbs.active_page();
        let follow = self.thumb_follow_active;
        let mut clicked = None;
        let tile_size = egui::vec2(crate::ui::THUMB_TILE_WIDTH, crate::ui::THUMB_TILE_HEIGHT);
        #[cfg(feature = "pdfium")]
        let spec = ThumbnailSpec::new(crate::ui::THUMB_MAX_SIDE, crate::ui::THUMB_MAX_SIDE);

        let output = egui::ScrollArea::vertical().show(ui, |ui| {
            for page in 0..page_count {
                let is_active = page == active;
                let (_, _response) = ui
                    .vertical_centered(|ui| {
                        let (rect, response) =
                            ui.allocate_exact_size(tile_size, egui::Sense::click());
                        let fill = if is_active {
                            egui::Color32::from_rgb(214, 231, 252)
                        } else {
                            ui.visuals().extreme_bg_color
                        };
                        let stroke = if is_active {
                            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(40, 110, 200))
                        } else {
                            ui.visuals().widgets.inactive.fg_stroke
                        };
                        ui.painter().rect_filled(rect, 3.0, fill);
                        ui.painter()
                            .rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Outside);

                        #[cfg(feature = "pdfium")]
                        if let Some(texture) = self.thumbnail_texture(ui.ctx(), page, &spec) {
                            let full_uv = egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            );
                            ui.painter()
                                .image(texture.id(), rect, full_uv, egui::Color32::WHITE);
                        }

                        if response.clicked() {
                            clicked = Some(page);
                        }
                        if follow && is_active {
                            response.scroll_to_me(Some(egui::Align::Center));
                        }
                        if is_active {
                            ui.label(egui::RichText::new(format!("Page {}", page + 1)).strong());
                        } else {
                            ui.label(format!("Page {}", page + 1));
                        }
                        (rect, response)
                    })
                    .inner;
                ui.add_space(8.0);
            }
        });

        self.thumbs.set_scroll_offset(output.state.offset.y);
        self.thumb_follow_active = false;
        if let Some(page) = clicked {
            self.dispatch(AppCommand::SelectThumbnailPage(page), Some(ui.ctx()));
        }
    }

    /// Draw a compact thumbnail grid (two columns in the pages panel).
    fn thumbnail_grid_ui(&mut self, ui: &mut egui::Ui, page_count: usize) {
        let active = self.thumbs.active_page();
        let follow = self.thumb_follow_active;
        let mut clicked = None;
        let tile = egui::vec2(84.0, 104.0);
        #[cfg(feature = "pdfium")]
        let spec = ThumbnailSpec::new(crate::ui::THUMB_MAX_SIDE, crate::ui::THUMB_MAX_SIDE);

        let output = egui::ScrollArea::vertical().show(ui, |ui| {
            ui.horizontal_wrapped(|ui| {
                for page in 0..page_count {
                    let is_active = page == active;
                    let (rect, response) = ui.allocate_exact_size(tile, egui::Sense::click());
                    let fill = if is_active {
                        egui::Color32::from_rgb(214, 231, 252)
                    } else {
                        ui.visuals().extreme_bg_color
                    };
                    let stroke = if is_active {
                        egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(40, 110, 200))
                    } else {
                        ui.visuals().widgets.inactive.fg_stroke
                    };
                    ui.painter().rect_filled(rect, 3.0, fill);
                    ui.painter()
                        .rect_stroke(rect, 3.0, stroke, egui::StrokeKind::Outside);

                    #[cfg(feature = "pdfium")]
                    if let Some(texture) = self.thumbnail_texture(ui.ctx(), page, &spec) {
                        let image_rect = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + 6.0, rect.top() + 6.0),
                            egui::pos2(rect.right() - 6.0, rect.bottom() - 18.0),
                        );
                        if image_rect.width() > 0.0 && image_rect.height() > 0.0 {
                            let full_uv = egui::Rect::from_min_max(
                                egui::pos2(0.0, 0.0),
                                egui::pos2(1.0, 1.0),
                            );
                            ui.painter().image(
                                texture.id(),
                                image_rect,
                                full_uv,
                                egui::Color32::WHITE,
                            );
                        }
                    }

                    ui.painter().text(
                        egui::pos2(rect.center().x, rect.bottom() - 6.0),
                        egui::Align2::CENTER_BOTTOM,
                        format!("{}", page + 1),
                        egui::FontId::proportional(9.5),
                        ui.visuals().text_color(),
                    );
                    if response.clicked() {
                        clicked = Some(page);
                    }
                    if follow && is_active {
                        response.scroll_to_me(Some(egui::Align::Center));
                    }
                    ui.add_space(2.0);
                }
            });
        });

        self.thumbs.set_scroll_offset(output.state.offset.y);
        self.thumb_follow_active = false;
        if let Some(page) = clicked {
            self.dispatch(AppCommand::SelectThumbnailPage(page), Some(ui.ctx()));
        }
    }

    /// Upload (or refresh) the cached thumbnail for `page` into an egui texture.
    #[cfg(feature = "pdfium")]
    #[allow(dead_code)]
    fn thumbnail_texture(
        &mut self,
        ctx: &egui::Context,
        page: usize,
        spec: &ThumbnailSpec,
    ) -> Option<egui::TextureHandle> {
        let thumbnail = self.thumbs.cached_thumbnail(page, spec)?.clone();
        let width = thumbnail.width() as usize;
        let height = thumbnail.height() as usize;
        let pixels = thumbnail.pixels();
        let expected = width.checked_mul(height)?.checked_mul(4)?;
        if width == 0 || height == 0 || pixels.len() != expected {
            return None;
        }
        let image = || egui::ColorImage::from_rgba_unmultiplied([width, height], pixels);
        match self.thumb_textures.get_mut(&page) {
            Some(handle) => handle.set(image(), egui::TextureOptions::LINEAR),
            None => {
                let handle = ctx.load_texture(
                    format!("pdfxi-thumb-{page}"),
                    image(),
                    egui::TextureOptions::LINEAR,
                );
                self.thumb_textures.insert(page, handle);
            }
        }
        self.thumb_textures.get(&page).cloned()
    }
}
