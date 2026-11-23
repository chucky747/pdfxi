//! Left side panel: page thumbnails, Recent, Home, and future rail content.

use std::path::PathBuf;

use pdfxi_edit::document_annotations::{export_xfdf, import_xfdf};

use crate::design::{Icon, Palette};
use crate::shared_folder::SharedFolderSource;
use crate::ui::{AppCommand, PdfxiApp};
#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, thumbnail::ThumbnailSpec};

use super::capabilities::{Capability, RailItem};
use super::widgets::shell_icon_button;

/// One PDF listed from the shared folder.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct SharedEntry {
    /// File name shown in the panel.
    pub(crate) name: String,
    /// Absolute path opened on click.
    pub(crate) path: PathBuf,
    /// File size in bytes.
    pub(crate) size: u64,
}

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
                if shell_icon_button(ui, palette, Icon::Recent, 22.0, "Refresh")
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

    /// Shared folder contents from the configured folder.
    fn shared_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new("Shared")
                .size(13.0)
                .color(palette.text_muted),
        );
        let Some(root) = self.settings.shared_folder.clone() else {
            ui.label(egui::RichText::new("No shared folder chosen").color(palette.text_muted));
            ui.label(
                egui::RichText::new("Choose one in Settings (gear) > Shared folder.")
                    .size(10.0)
                    .color(palette.text_muted),
            );
            return;
        };
        ui.label(
            egui::RichText::new(format!("Folder: {}", root.display()))
                .size(10.0)
                .color(palette.text_muted),
        );
        let mut open = None;
        match self.shared_entries() {
            Ok(entries) => {
                if entries.is_empty() {
                    ui.label(egui::RichText::new("This folder is empty").color(palette.text_muted));
                } else {
                    egui::ScrollArea::vertical()
                        .max_height(220.0)
                        .show(ui, |ui| {
                            for entry in &entries {
                                let label = format!(
                                    "{}  ({})",
                                    entry.name,
                                    crate::ui::library::format_bytes(entry.size)
                                );
                                if ui.selectable_label(false, label).clicked() {
                                    open = Some(entry.path.clone());
                                }
                            }
                        });
                }
            }
            Err(error) => {
                ui.colored_label(ui.visuals().error_fg_color, error);
            }
        }

        if let Some(sidecar) = self.shared_review_sidecar() {
            ui.separator();
            ui.label(egui::RichText::new("Shared review").strong());
            let review_name = sidecar.file_name().map_or_else(
                || sidecar.display().to_string(),
                |name| name.to_string_lossy().into_owned(),
            );
            let exists = sidecar.is_file();
            ui.label(
                egui::RichText::new(format!(
                    "Review file: {review_name} ({})",
                    if exists { "present" } else { "not written yet" }
                ))
                .size(10.0)
                .color(palette.text_muted),
            );
            let mut share = false;
            let mut import = false;
            ui.horizontal_wrapped(|ui| {
                if ui.button("Share review comments").clicked() {
                    share = true;
                }
                if ui
                    .add_enabled(exists, egui::Button::new("Import review comments"))
                    .on_disabled_hover_text("Share this document's review first")
                    .clicked()
                {
                    import = true;
                }
            });
            if share {
                let _ = self.sync_review_to_shared_folder();
            }
            if import {
                let _ = self.import_review_from_shared_folder();
            }
        }

        if let Some(path) = open {
            let _ = self.open_document(&path);
        }
    }

    /// PDFs in the configured shared folder, sorted by name and capped at 200.
    pub(crate) fn shared_entries(&self) -> Result<Vec<SharedEntry>, String> {
        let Some(root) = &self.settings.shared_folder else {
            return Ok(Vec::new());
        };
        let source = SharedFolderSource::new(root.to_string_lossy().into_owned())
            .map_err(|error| error.to_string())?
            .with_recursive(false)
            .with_max_entries(200)
            .with_extension("pdf");
        let read = std::fs::read_dir(root)
            .map_err(|error| format!("Could not read {}: {error}", root.display()))?;
        let mut entries = Vec::new();
        for item in read {
            let item = item.map_err(|error| error.to_string())?;
            let path = item.path();
            if !path.is_file() {
                continue;
            }
            let name = item.file_name().to_string_lossy().into_owned();
            if !name.to_ascii_lowercase().ends_with(".pdf") {
                continue;
            }
            source
                .validate_child(&name)
                .map_err(|error| error.to_string())?;
            let size = item.metadata().map_or(0, |metadata| metadata.len());
            entries.push(SharedEntry { name, path, size });
        }
        entries.sort_by(|left, right| {
            left.name
                .to_ascii_lowercase()
                .cmp(&right.name.to_ascii_lowercase())
                .then_with(|| left.name.cmp(&right.name))
        });
        entries.truncate(source.max_entries());
        Ok(entries)
    }

    /// The sidecar XFDF path for the active document, when it is shared.
    pub(crate) fn shared_review_sidecar(&self) -> Option<PathBuf> {
        self.shared_review_target()
            .ok()
            .map(|(_, _, sidecar)| sidecar)
    }

    /// Export the active document's annotations beside it as `<stem>.xfdf`.
    pub(crate) fn sync_review_to_shared_folder(&mut self) -> Result<PathBuf, String> {
        let (id, document_path, sidecar) = self.shared_review_target()?;
        let xml = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())
            .and_then(|session| {
                export_xfdf(session.document()).map_err(|error| error.to_string())
            })?;
        let temporary = sidecar.with_extension("xfdf.tmp");
        std::fs::write(&temporary, xml)
            .map_err(|error| format!("Could not write {}: {error}", temporary.display()))?;
        if let Err(error) = std::fs::rename(&temporary, &sidecar) {
            let _ = std::fs::remove_file(&temporary);
            return Err(format!("Could not write {}: {error}", sidecar.display()));
        }
        let name = document_path.file_name().map_or_else(
            || document_path.display().to_string(),
            |name| name.to_string_lossy().into_owned(),
        );
        self.notify_real_event("Review comments shared", &name);
        self.status = format!("Shared review comments to {}", sidecar.display());
        Ok(sidecar)
    }

    /// Import the sidecar XFDF into the active document as one session edit.
    pub(crate) fn import_review_from_shared_folder(&mut self) -> Result<usize, String> {
        let (id, _document_path, sidecar) = self.shared_review_target()?;
        if !sidecar.is_file() {
            return Err(format!("No review file at {}", sidecar.display()));
        }
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        let xml = std::fs::read_to_string(&sidecar)
            .map_err(|error| format!("Could not read {}: {error}", sidecar.display()))?;
        let author = if self.settings.display_name.trim().is_empty() {
            "PDFXI reviewer".to_owned()
        } else {
            self.settings.display_name.trim().to_owned()
        };
        let mut candidate = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .document()
            .clone();
        let imported =
            import_xfdf(&mut candidate, &xml, &author).map_err(|error| error.to_string())?;
        self.sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .map_err(|error| error.to_string())?;
        self.after_session_edit(id);
        self.status = format!("Imported {imported} review comment(s)");
        Ok(imported)
    }

    /// Active tab id, canonical document path and sidecar for shared review.
    fn shared_review_target(&self) -> Result<(crate::TabId, PathBuf, PathBuf), String> {
        let root = self
            .settings
            .shared_folder
            .as_ref()
            .ok_or_else(|| "Choose a shared folder first".to_owned())?;
        let id = self
            .state
            .active_tab_id()
            .ok_or_else(|| "Open a document first".to_owned())?;
        let path = self
            .sessions
            .get(&id)
            .and_then(|session| session.path())
            .ok_or_else(|| "The document has no file path".to_owned())?;
        let document = std::fs::canonicalize(path).unwrap_or_else(|_| path.to_path_buf());
        let root = std::fs::canonicalize(root).unwrap_or_else(|_| root.clone());
        if !document.starts_with(&root) {
            return Err("The active document is not inside the shared folder".to_owned());
        }
        let sidecar = document.with_extension("xfdf");
        Ok((id, document, sidecar))
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
                    if shell_icon_button(ui, palette, Icon::More, 22.0, "Page options")
                        .on_hover_text("Page options")
                        .clicked()
                    {
                        self.dispatch(AppCommand::PageMenu, Some(ui.ctx()));
                    }
                    if shell_icon_button(ui, palette, Icon::Plus, 22.0, "Add page")
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
                if shell_icon_button(ui, palette, glyph, 22.0, "Toggle page layout")
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
        // The bookmark section is its own bottom panel above the storage meter.
        // Drawing it after the thumbnail scroll area clipped it entirely and
        // let "Add bookmark" overlap the storage meter in short windows.
        if self.capabilities.contains(Capability::Bookmarks) && self.state.active_tab_id().is_some()
        {
            egui::TopBottomPanel::bottom("pages_bookmarks")
                .resizable(true)
                .default_height(220.0)
                .height_range(96.0..=360.0)
                .frame(egui::Frame::new())
                .show_inside(ui, |ui| {
                    egui::ScrollArea::vertical()
                        .id_salt("pages_bookmarks_scroll")
                        .show(ui, |ui| self.bookmarks_section_ui(ui, palette));
                });
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
                        let page_label = format!("Page {}", page + 1);
                        response.widget_info(|| {
                            egui::WidgetInfo::labeled(
                                egui::WidgetType::Button,
                                true,
                                page_label.as_str(),
                            )
                        });
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
                            // Keep the page's own aspect ratio inside the tile;
                            // stretching a landscape page into a fixed box
                            // distorted it.
                            let image_rect = fit_inside(texture.size_vec2(), rect.shrink(3.0));
                            ui.painter().image(
                                texture.id(),
                                image_rect,
                                full_uv,
                                egui::Color32::WHITE,
                            );
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
                    let page_label = format!("Page {}", page + 1);
                    response.widget_info(|| {
                        egui::WidgetInfo::labeled(
                            egui::WidgetType::Button,
                            true,
                            page_label.as_str(),
                        )
                    });
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
                        let available = egui::Rect::from_min_max(
                            egui::pos2(rect.left() + 6.0, rect.top() + 6.0),
                            egui::pos2(rect.right() - 6.0, rect.bottom() - 18.0),
                        );
                        let image_rect = fit_inside(texture.size_vec2(), available);
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

/// Fit a texture of `size` inside `available`, preserving its aspect ratio.
///
/// The thumbnail renderer already honours the page's aspect; the panel used to
/// stretch the texture to a fixed tile, which squashed landscape pages.
#[cfg(feature = "pdfium")]
fn fit_inside(size: egui::Vec2, available: egui::Rect) -> egui::Rect {
    if size.x <= 0.0 || size.y <= 0.0 || available.width() <= 0.0 || available.height() <= 0.0 {
        return available;
    }
    let scale = (available.width() / size.x).min(available.height() / size.y);
    egui::Rect::from_center_size(available.center(), size * scale)
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

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_document::DocumentSession;
    use pdfxi_edit::{Rect, add_highlight, list_annotations};
    use std::path::Path;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld")
            .join(name)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        // Build an absolute root without `..`; SharedFolderSource rejects
        // parent traversal and verbatim (`\\?\`) paths.
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        let root = workspace
            .parent()
            .and_then(Path::parent)
            .expect("the workspace root")
            .to_path_buf();
        let dir = root
            .join("target/tmp/shared-panel")
            .join(format!("{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the shared folder");
        dir
    }

    fn copy_fixture(fixture_name: &str, dir: &Path, name: &str) -> PathBuf {
        let target = dir.join(name);
        std::fs::copy(fixture(fixture_name), &target).expect("copy the fixture");
        target
    }

    #[test]
    fn lists_pdfs_sorted_by_name_and_opens_a_row() {
        let dir = temp_dir("list");
        copy_fixture("microsoft-excel-2019-sample.pdf", &dir, "a-excel.pdf");
        copy_fixture("powerpoint-slides.pdf", &dir, "z-slides.pdf");
        std::fs::write(dir.join("notes.txt"), b"not a PDF").expect("write a non-PDF");
        let mut app = PdfxiApp::new();
        app.set_shared_folder(Some(dir.clone()));

        let entries = app.shared_entries().expect("list the shared folder");
        let names: Vec<&str> = entries.iter().map(|entry| entry.name.as_str()).collect();
        assert_eq!(names, vec!["a-excel.pdf", "z-slides.pdf"]);
        assert!(entries.iter().all(|entry| entry.size > 0));

        let outcome = app.open_document(&entries[1].path);
        assert!(matches!(outcome, crate::OpenOutcome::Opened { .. }));
        assert_eq!(
            app.state
                .active_tab()
                .and_then(|tab| tab.path())
                .map(Path::to_path_buf),
            Some(entries[1].path.clone())
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn no_folder_and_empty_folder_states() {
        let app = PdfxiApp::new();
        assert!(app.shared_entries().expect("no folder").is_empty());
        assert_eq!(app.shared_review_sidecar(), None);

        let dir = temp_dir("empty");
        let mut app = PdfxiApp::new();
        app.set_shared_folder(Some(dir.clone()));
        assert!(app.shared_entries().expect("empty folder").is_empty());
        assert!(matches!(
            app.sync_review_to_shared_folder(),
            Err(error) if error.contains("Open a document")
        ));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn share_review_writes_the_sidecar_with_the_highlight() {
        let dir = temp_dir("share");
        let path = copy_fixture("microsoft-excel-2019-sample.pdf", &dir, "review.pdf");
        let mut app = PdfxiApp::new();
        app.set_shared_folder(Some(dir.clone()));
        app.open_document(&path);
        let id = app.state.active_tab_id().expect("the fixture opens");
        let mut candidate = app.sessions.get(&id).expect("session").document().clone();
        let rect = Rect::new(72.0, 700.0, 200.0, 716.0).expect("a valid rect");
        add_highlight(&mut candidate, 0, &[rect], "Review note", "Reviewer")
            .expect("add a highlight");
        app.sessions
            .get_mut(&id)
            .expect("session")
            .edit(|document| {
                *document = candidate;
                Ok(())
            })
            .expect("commit the highlight");

        let sidecar = app
            .sync_review_to_shared_folder()
            .expect("share the review");
        assert_eq!(
            sidecar.file_name().and_then(|name| name.to_str()),
            Some("review.xfdf")
        );
        let xml = std::fs::read_to_string(&sidecar).expect("read the sidecar");
        assert!(xml.contains("<annots"), "XFDF annots root: {xml}");
        assert!(xml.contains("Highlight"), "XFDF subtype: {xml}");
        assert!(app.status().starts_with("Shared review comments"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn import_review_adds_one_annotation_and_persists() {
        let dir = temp_dir("import");
        let path = copy_fixture("microsoft-excel-2019-sample.pdf", &dir, "review.pdf");
        {
            let mut author = PdfxiApp::new();
            author.set_shared_folder(Some(dir.clone()));
            author.open_document(&path);
            let id = author.state.active_tab_id().expect("the fixture opens");
            let mut candidate = author
                .sessions
                .get(&id)
                .expect("session")
                .document()
                .clone();
            let rect = Rect::new(72.0, 700.0, 200.0, 716.0).expect("a valid rect");
            add_highlight(&mut candidate, 0, &[rect], "Review note", "Reviewer")
                .expect("add a highlight");
            author
                .sessions
                .get_mut(&id)
                .expect("session")
                .edit(|document| {
                    *document = candidate;
                    Ok(())
                })
                .expect("commit the highlight");
            author
                .sync_review_to_shared_folder()
                .expect("share the review");
        }
        // Restore the clean copy so the import is the only annotation source.
        std::fs::copy(fixture("microsoft-excel-2019-sample.pdf"), &path)
            .expect("reset to the clean fixture");

        let mut app = PdfxiApp::new();
        app.set_shared_folder(Some(dir.clone()));
        app.open_document(&path);
        assert_eq!(
            app.annotations().len(),
            0,
            "the clean copy has no annotations"
        );
        let imported = app
            .import_review_from_shared_folder()
            .expect("import the review");
        assert_eq!(imported, 1);
        assert_eq!(app.annotations().len(), 1);
        assert!(app.status().starts_with("Imported 1 review comment"));

        let checkpoint = dir.join("review-imported.pdf");
        app.save_active_as(&checkpoint)
            .expect("save the imported review");
        let reopened = DocumentSession::open(&checkpoint).expect("reopen the saved file");
        assert_eq!(
            list_annotations(reopened.document(), 0)
                .expect("read annotations")
                .len(),
            1,
            "the imported annotation persists through save and reopen"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }
}
