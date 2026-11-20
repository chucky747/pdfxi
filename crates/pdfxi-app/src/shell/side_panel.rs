//! The page-thumbnail side panel.

use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};
#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, thumbnail::ThumbnailSpec};

impl PdfxiApp {
    /// Render the left thumbnail navigation side panel.
    ///
    /// With `pdfium`, only visible rows (plus the pane's documented prefetch
    /// margin) are enqueued, and at most [`crate::ui::THUMBS_PER_FRAME`] are
    /// generated per frame so scrolling never blocks on rasterization. Without
    /// it, every tile is drawn as a placeholder. Clicking a tile dispatches
    /// [`AppCommand::SelectThumbnailPage`]; scrolling updates the pane's visible
    /// range so the next frame requests the right thumbnails.
    #[allow(dead_code)]
    pub(crate) fn thumbnails_ui(&mut self, ui: &mut egui::Ui) {
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
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

    /// Draw the panel border and, for a real document, its thumbnails.
    pub(crate) fn side_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let panel = ui.max_rect();
        ui.painter().line_segment(
            [
                egui::pos2(panel.right(), panel.top()),
                egui::pos2(panel.right(), panel.bottom()),
            ],
            egui::Stroke::new(1.0_f32, palette.border),
        );
        // Only real thumbnails are shown, and only for a real open document.
        if self.state.active_tab().is_some() {
            self.thumbnails_ui(ui);
        }
    }
}

impl PdfxiApp {
    /// Upload (or refresh) the cached thumbnail for `page` into an egui texture.
    ///
    /// The texture handle is reused across frames and re-uploaded only when the
    /// cached thumbnail changes, avoiding a new GPU allocation every frame.
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
