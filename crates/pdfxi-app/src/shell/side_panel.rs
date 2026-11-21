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
        egui::TopBottomPanel::bottom("shell_storage")
            .exact_height(56.0)
            .frame(egui::Frame::new().inner_margin(egui::Margin {
                left: 0,
                right: 0,
                top: 6,
                bottom: 4,
            }))
            .show_inside(ui, |ui| self.storage_meter_ui(ui, palette));

        match self.active_rail {
            RailItem::Pages => self.pages_panel_ui(ui, palette),
            RailItem::Recent => self.recent_panel_ui(ui, palette),
            RailItem::Home => self.home_panel_ui(ui, palette),
            RailItem::Files => self.files_panel_ui(ui, palette),
            RailItem::Starred => self.starred_panel_ui(ui, palette),
            RailItem::Shared => self.shared_panel_ui(ui, palette),
            RailItem::Comments => self.comments_panel_ui(ui, palette),
            RailItem::AiAssistant => self.assistant_panel_ui(ui, palette),
        }
    }

    /// Storage total and a bar scaled to the larger of used and 1 GB.
    fn storage_meter_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let root = self.settings.library_root_or_default();
        let folder = root
            .as_ref()
            .and_then(|path| path.file_name())
            .and_then(|name| name.to_str())
            .unwrap_or("library");
        let text = format!(
            "{} used in {folder}",
            crate::ui::library::format_bytes(self.storage_bytes)
        );
        let label = ui.add(
            egui::Label::new(
                egui::RichText::new(text)
                    .size(10.0)
                    .color(palette.text_muted),
            )
            .wrap_mode(egui::TextWrapMode::Extend)
            .sense(egui::Sense::click()),
        );
        let (rect, bar) =
            ui.allocate_exact_size(egui::vec2(ui.available_width(), 6.0), egui::Sense::click());
        let used = self.storage_bytes as f32;
        let scale = used.max(1024.0 * 1024.0 * 1024.0);
        let fraction = (used / scale).clamp(0.0, 1.0);
        ui.painter().rect_filled(rect, 3.0, palette.surface);
        let fill =
            egui::Rect::from_min_size(rect.min, egui::vec2(rect.width() * fraction, rect.height()));
        ui.painter().rect_filled(fill, 3.0, palette.accent);
        if label.clicked() || bar.clicked() {
            self.dispatch(AppCommand::OpenStorageFolder, Some(ui.ctx()));
        }
    }

    /// Library folder listing with open and reveal actions.
    fn files_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.horizontal(|ui| {
            ui.label(
                egui::RichText::new("Files")
                    .size(13.0)
                    .color(palette.text_muted),
            );
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shell_icon_button(ui, palette, Icon::Recent, 22.0)
                    .on_hover_text("Refresh the library")
                    .clicked()
                {
                    self.dispatch(AppCommand::RefreshLibrary, Some(ui.ctx()));
                }
            });
        });
        if !self.library_loaded {
            self.refresh_library();
        }
        if self.library_entries.is_empty() {
            ui.label(egui::RichText::new("No PDFs in this folder yet").color(palette.text_muted));
            if ui.button("Open a PDF").clicked() {
                self.dispatch(AppCommand::OpenDialog, Some(ui.ctx()));
            }
            return;
        }
        for entry in self.library_entries.clone() {
            let title = short_title(&entry.title, 18);
            ui.horizontal(|ui| {
                if ui.selectable_label(false, title).clicked() {
                    self.dispatch(
                        AppCommand::OpenPath(entry.path.to_string_lossy().into_owned()),
                        Some(ui.ctx()),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Reveal").clicked() {
                        self.dispatch(
                            AppCommand::RevealInExplorer(entry.path.clone()),
                            Some(ui.ctx()),
                        );
                    }
                });
            });
        }
    }

    /// Recent documents, with missing entries marked and removable.
    fn recent_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new("Recent")
                .size(13.0)
                .color(palette.text_muted),
        );
        if self.library.recent().is_empty() {
            ui.label(
                egui::RichText::new("Recent files appear after you open documents")
                    .color(palette.text_muted),
            );
            return;
        }
        for entry in self.library.recent().to_vec() {
            ui.horizontal(|ui| {
                let title = if entry.is_missing() {
                    format!("{} (missing)", entry.title)
                } else {
                    entry.title.clone()
                };
                let title = short_title(&title, 20);
                if ui.selectable_label(false, title).clicked() && !entry.is_missing() {
                    self.dispatch(
                        AppCommand::OpenPath(entry.path.to_string_lossy().into_owned()),
                        Some(ui.ctx()),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if entry.is_missing() {
                        if ui.small_button("Remove").clicked() {
                            self.dispatch(
                                AppCommand::RemoveRecent(entry.path.clone()),
                                Some(ui.ctx()),
                            );
                        }
                    } else if ui.small_button("Star").clicked() {
                        self.dispatch(AppCommand::ToggleStar(entry.path.clone()), Some(ui.ctx()));
                    }
                });
            });
        }
    }

    /// Starred documents.
    fn starred_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new("Starred")
                .size(13.0)
                .color(palette.text_muted),
        );
        let starred: Vec<std::path::PathBuf> = self.library.starred().iter().cloned().collect();
        if starred.is_empty() {
            ui.label(
                egui::RichText::new("Star a document to keep it here").color(palette.text_muted),
            );
            return;
        }
        for path in starred {
            let title = path
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("Untitled");
            let title = short_title(title, 18);
            ui.horizontal(|ui| {
                if ui.selectable_label(false, title).clicked() {
                    self.dispatch(
                        AppCommand::OpenPath(path.to_string_lossy().into_owned()),
                        Some(ui.ctx()),
                    );
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    if ui.small_button("Unstar").clicked() {
                        self.dispatch(AppCommand::ToggleStar(path.clone()), Some(ui.ctx()));
                    }
                });
            });
        }
    }

    /// Shared folder placeholder until round B implements browsing.
    fn shared_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new("Shared")
                .size(13.0)
                .color(palette.text_muted),
        );
        if let Some(folder) = &self.settings.shared_folder {
            let name = folder
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("shared folder");
            ui.label(egui::RichText::new(format!("Folder: {name}")).color(palette.text_muted));
        }
        ui.label(
            egui::RichText::new("Shared review browsing arrives in round B")
                .color(palette.text_muted),
        );
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

    /// Home panel: a short recent hint, no second open action.
    fn home_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(egui::RichText::new("Home").strong().size(13.0));
        ui.add_space(6.0);
        let count = self.library.recent().len();
        let message = match count {
            0 => "Recent files appear after you open documents".to_owned(),
            1 => "1 recent document".to_owned(),
            _ => format!("{count} recent documents"),
        };
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

/// Shorten a title for a narrow panel row, adding an ellipsis when clipped.
fn short_title(title: &str, max_chars: usize) -> String {
    let mut chars = title.chars();
    let mut shortened: String = chars.by_ref().take(max_chars).collect();
    if chars.next().is_some() {
        shortened.push('…');
    }
    shortened
}
