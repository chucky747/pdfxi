//! Document canvas, page pill, and raster/texture helpers.

use crate::design::Palette;
#[cfg(feature = "pdfium")]
use crate::raster::RasterKey;
#[cfg(feature = "pdfium")]
use crate::ui::LiveTexture;
use crate::ui::*;
use crate::{
    CanvasLayout, CanvasPoint, CanvasSize, FindMatch, Rotation, Toolset, uv_quad_for_rotation,
};

/// Paint a cached page texture into `rect`, rotating the texture UVs so the
/// raster appears turned rather than stretched into a swapped page rectangle.
///
/// The four UV corners come from [`uv_quad_for_rotation`]; they are assigned to
/// the displayed rectangle's corners in `(top-left, top-right, bottom-right,
/// bottom-left)` order and drawn as two triangles. This is a pure GPU-side
/// transform - no re-render or blocking I/O is involved.
#[cfg(feature = "pdfium")]
fn paint_rotated_image(
    painter: &egui::Painter,
    texture_id: egui::TextureId,
    rect: egui::Rect,
    rotation: Rotation,
) {
    let uv = uv_quad_for_rotation(rotation);
    let corners = [
        rect.left_top(),
        rect.right_top(),
        rect.right_bottom(),
        rect.left_bottom(),
    ];
    let mut mesh = egui::Mesh::with_texture(texture_id);
    for (corner, (u, v)) in corners.iter().zip(uv.iter()) {
        mesh.vertices.push(egui::epaint::Vertex {
            pos: *corner,
            uv: egui::pos2(*u, *v),
            color: egui::Color32::WHITE,
        });
    }
    mesh.add_triangle(0, 1, 2);
    mesh.add_triangle(0, 2, 3);
    painter.add(mesh);
}

impl PdfxiApp {
    /// Rasterize the pages visible in `layout` into the cache (PDFium build only).
    ///
    /// A fresh [`CancellationToken`] is scoped to this frame; the renderer
    /// checks it between steps so a closed document can stop outstanding work.
    /// Each raster is keyed by that page's effective rotation, matching the
    /// rotation used when painting the cached texture below.
    #[cfg(feature = "pdfium")]
    pub(crate) fn ensure_rasters(&mut self, layout: &CanvasLayout, pixel_ratio: f32) {
        let Some(renderer) = self.pdfium_renderer.as_ref() else {
            return;
        };
        let scroll = CanvasPoint::new(self.scroll_x, self.scroll_y);
        let token = self.raster_cancel.clone();
        match self.raster_cache.ensure_visible_capped_at(
            renderer,
            layout,
            scroll,
            pixel_ratio,
            &token,
            RASTERS_PER_FRAME,
            &mut |_| {},
        ) {
            Ok(_) => {}
            Err(error) => self.status = format!("Render error: {error}"),
        }
    }

    /// Upload the cached raster for `page` into a reusable `egui` texture.
    ///
    /// Returns `None` when the page is not cached or its pixel buffer is not a
    /// valid tightly packed RGBA image. The handle is reused across frames and
    /// only re-uploaded when the scale or rotation key changes.
    #[cfg(feature = "pdfium")]
    pub(crate) fn raster_texture(
        &mut self,
        ctx: &egui::Context,
        page: usize,
        scale: f32,
        rotation: Rotation,
    ) -> Option<egui::TextureHandle> {
        let key = RasterKey::new(page, scale, rotation);
        // Clone out of the cache so the pixel borrow does not overlap the
        // mutable borrow of the texture map below.
        let raster = self.raster_cache.get(key)?.clone();
        let width = raster.width() as usize;
        let height = raster.height() as usize;
        let pixels = raster.pixels();
        let expected = width.checked_mul(height)?.checked_mul(4)?;
        if width == 0 || height == 0 || pixels.len() != expected {
            return None;
        }
        let image = || egui::ColorImage::from_rgba_unmultiplied([width, height], pixels);
        let scale_bits = key.scale_bits();
        let rotation_degrees = key.rotation_degrees();
        match self.textures.get_mut(&page) {
            Some(live) if live.scale_bits == scale_bits && live.rotation == rotation_degrees => {}
            Some(live) => {
                live.handle.set(image(), egui::TextureOptions::LINEAR);
                live.scale_bits = scale_bits;
                live.rotation = rotation_degrees;
            }
            None => {
                let handle = ctx.load_texture(
                    format!("pdfxi-page-{page}"),
                    image(),
                    egui::TextureOptions::LINEAR,
                );
                self.textures.insert(
                    page,
                    LiveTexture {
                        handle,
                        scale_bits,
                        rotation: rotation_degrees,
                    },
                );
            }
        }
        self.textures.get(&page).map(|live| live.handle.clone())
    }

    /// Render the document canvas, or the empty Home state with no document.
    pub(crate) fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::for_mode(self.theme);
        egui::Frame::new()
            .fill(palette.canvas)
            .inner_margin(egui::Margin::same(18))
            .show(ui, |ui| {
                if self.state.active_tab().is_none() {
                    self.empty_home_ui(ui, palette);
                    return;
                }
                let viewport = CanvasSize::new(ui.available_width(), ui.available_height());
                let Some(layout) = layout_for_state(&self.state, viewport, &self.page_sizes) else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(48.0);
                        ui.label(egui::RichText::new(canvas_placeholder(&self.state)).weak());
                    });
                    return;
                };

                // Fill the LRU cache with real rasters before painting. This
                // happens only with PDFium; otherwise the placeholder stands.
                #[cfg(feature = "pdfium")]
                let pixels_per_point = ui.ctx().pixels_per_point();
                #[cfg(feature = "pdfium")]
                self.ensure_rasters(&layout, pixels_per_point);

                let origin = ui.min_rect().left_top();
                // Centre a page narrower than the viewport; wider pages keep
                // normal left-aligned scrolling with no extra offset.
                let content_right = layout
                    .pages()
                    .iter()
                    .map(|page| page.rect.x + page.rect.width)
                    .fold(0.0_f32, f32::max);
                let pad_x = ((viewport.width - content_right) * 0.5).max(0.0);
                let text_tool = self.state.active_tool() == Toolset::TextSelection;
                let (scroll_delta, hover, pressed, dragging) = ui.input(|input| {
                    let pointer = &input.pointer;
                    let pressed = pointer.primary_pressed();
                    (
                        input.smooth_scroll_delta,
                        pointer.hover_pos(),
                        pressed,
                        pointer.primary_down() && !pressed,
                    )
                });

                // Scroll, clamped to the laid-out content (never blocks on I/O).
                let scrolled = layout.clamp_scroll(
                    self.scroll_x + scroll_delta.x,
                    self.scroll_y + scroll_delta.y,
                );
                self.scroll_x = scrolled.x;
                self.scroll_y = scrolled.y;

                // A thumbnail click scrolls the canvas to that page. The scroll
                // is applied here because this is where a layout is available.
                if let Some(page) = self.pending_page_scroll.take() {
                    self.scroll_y = scroll_offset_for_page(&layout, page);
                }

                // A find navigation request scrolls the current match into view
                // once a layout is available, using the pure helper below.
                if self.pending_scroll {
                    if let Some(page) = self.find.current().map(FindMatch::page) {
                        self.scroll_y = scroll_offset_for_page(&layout, page);
                    }
                    self.pending_scroll = false;
                }

                // Keep the thumbnail pane's active tile in sync with canvas
                // scrolling. A change asks the pane to scroll that row into view
                // on its next frame; an unchanged frame leaves manual pane
                // scrolling alone.
                let _ = self.sync_active_page_from_canvas(&layout);

                // Text drag: anchor on press, advance the head while dragging.
                // Real page text is not wired yet, so the slice is empty.

                if text_tool && let Some(pos) = hover {
                    let point = screen_to_canvas(pos, origin, self.scroll_x - pad_x, self.scroll_y);
                    if pressed {
                        self.selection.begin_at(point, &layout, &self.text_pages);
                    } else if dragging {
                        self.selection.update_to(point, &layout, &self.text_pages);
                    }
                }

                // Report the page under the cursor in canvas coordinates.

                #[cfg(feature = "pdfium")]
                let ctx = ui.ctx().clone();
                let painter = ui.painter();
                let stroke = ui.visuals().widgets.inactive.fg_stroke;
                for page in layout.pages() {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(
                            origin.x + page.rect.x - self.scroll_x + pad_x,
                            origin.y + page.rect.y - self.scroll_y,
                        ),
                        egui::vec2(page.rect.width, page.rect.height),
                    );
                    #[cfg(feature = "pdfium")]
                    if let Some(texture) = self.raster_texture(
                        &ctx,
                        page.page,
                        page.scale * pixels_per_point,
                        page.rotation,
                    ) {
                        // Paint the real page raster inside the rotated page
                        // rectangle, rotating its UVs to match the view.
                        paint_rotated_image(painter, texture.id(), rect, page.rotation);
                        continue;
                    }
                    // Without a cached raster (or without PDFium), draw the
                    // outlined page rectangle.
                    painter.rect(
                        rect,
                        0.0,
                        egui::Color32::TRANSPARENT,
                        stroke,
                        egui::StrokeKind::Outside,
                    );
                }

                // Highlight every find match; the current one gets a stronger
                // fill and an outline. Geometry is computed by pure helpers so
                // the coordinate mapping is testable without a window.
                let current_index = self.find.current_index();
                let mut highlights: Vec<(egui::Rect, bool)> = Vec::new();
                for (index, find_hit) in self.find.matches().iter().enumerate() {
                    let Some(local) = match_rect(find_hit, &self.text_pages) else {
                        continue;
                    };
                    // Map the page-local match rectangle forward through the
                    // page's effective rotation so highlights follow the
                    // rotated page instead of stretching with it.
                    let Some(mapped) = layout.page_rect_to_canvas(find_hit.page(), local) else {
                        continue;
                    };
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(
                            origin.x + mapped.x - self.scroll_x + pad_x,
                            origin.y + mapped.y - self.scroll_y,
                        ),
                        egui::vec2(mapped.width, mapped.height),
                    );
                    highlights.push((rect, Some(index) == current_index));
                }
                for (rect, is_current) in highlights {
                    let fill = if is_current {
                        egui::Color32::from_rgba_unmultiplied(255, 196, 0, 130)
                    } else {
                        egui::Color32::from_rgba_unmultiplied(255, 230, 80, 70)
                    };
                    painter.rect_filled(rect, 2.0, fill);
                    if is_current {
                        painter.rect_stroke(
                            rect,
                            2.0,
                            egui::Stroke::new(2.0_f32, egui::Color32::from_rgb(255, 180, 0)),
                            egui::StrokeKind::Outside,
                        );
                    }
                }

                // Persist the active tab's final scroll position so a later tab
                // switch can restore exactly where the user left this document.
                if let Some(tab) = self.state.active_tab_mut() {
                    tab.set_scroll_offset(CanvasPoint::new(self.scroll_x, self.scroll_y));
                }
            });
    }

    #[allow(dead_code)]
    fn cache_usage(&self) -> (usize, usize) {
        #[cfg(feature = "pdfium")]
        {
            (self.raster_cache.bytes(), self.raster_cache.max_bytes())
        }
        #[cfg(not(feature = "pdfium"))]
        {
            (0, 256 * 1024 * 1024)
        }
    }
}
