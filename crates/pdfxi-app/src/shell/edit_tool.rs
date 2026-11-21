//! Edit PDF tool (A-23 object editing and A-17 in-place text).
//!
//! While the tool is active a canvas click hit-tests the page's selectable
//! objects through [`pdfxi_edit::content_edit::list_objects`] and selects the
//! topmost one. Dragging the body moves the object, dragging a corner handle
//! resizes it, Delete removes it, and **Add text** places a new text box at the
//! next click.
//!
//! Every mutation clones the active session document, runs the matching
//! `pdfxi_edit::content_edit` function on the clone, and commits the result as
//! one undoable `DocumentSession::edit`. The source file is never written until
//! the user saves, and the shell never re-implements text geometry: all
//! placement and object identification come from the library.

use std::path::Path;

use pdfxi_core::Document;
use pdfxi_edit::content_edit::{
    PageObject, TextAlign, add_image, add_text_box, delete_object, list_objects, move_object,
    replace_object, resize_object, text_blocks,
};
use pdfxi_edit::{ImageFormat, ObjectKind, ObjectReplacement, Point, Rect};
use pdfxi_security::Permission;

use crate::ui::{PdfxiApp, active_tab_status, screen_to_canvas};
use crate::{CanvasLayout, CanvasRect, TabId};

/// One selected page object.
///
/// The object is addressed by its position in the current
/// [`list_objects`] result rather than by a stored identifier, so every
/// mutation resolves the identifier from the live document first.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditSelection {
    /// Zero-based page index.
    pub page: usize,
    /// Zero-based index into the page's [`list_objects`] result.
    pub object: usize,
    /// Placed rectangle in default user space as `[x0, y0, x1, y1]`, y up.
    pub rect: [f32; 4],
    /// Short human label shown in the toolbar.
    pub label: String,
}

impl EditSelection {
    /// The selection rectangle as a geometry [`Rect`].
    pub(crate) fn as_rect(&self) -> Option<Rect> {
        Rect::new(self.rect[0], self.rect[1], self.rect[2], self.rect[3]).ok()
    }
}

/// One of the four corner resize handles drawn around a selection.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Handle {
    /// Top-left corner.
    NorthWest,
    /// Top-right corner.
    NorthEast,
    /// Bottom-right corner.
    SouthEast,
    /// Bottom-left corner.
    SouthWest,
}

impl Handle {
    /// Every handle in a stable order.
    pub(crate) const ALL: [Self; 4] = [
        Self::NorthWest,
        Self::NorthEast,
        Self::SouthEast,
        Self::SouthWest,
    ];
}

/// What an in-progress canvas drag will do on release.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum DragMode {
    /// Translate the selected object.
    Move,
    /// Scale the selected object from one corner.
    Resize(Handle),
}

/// Pointer state for one canvas frame, bundled so the edit tool keeps a small
/// argument list.
#[derive(Clone, Copy, Debug, Default)]
pub(crate) struct CanvasPointer {
    /// Pointer position over the canvas, when it is over the window.
    pub hover: Option<egui::Pos2>,
    /// Primary button pressed this frame.
    pub pressed: bool,
    /// Primary button released this frame.
    pub released: bool,
    /// Primary button double-clicked this frame.
    pub double_clicked: bool,
}

/// State of the Edit PDF tool and its toolbar window.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct EditToolState {
    /// Whether the tool is active on the canvas.
    pub active: bool,
    /// The selected object, if any.
    pub selection: Option<EditSelection>,
    /// In-progress move or resize drag.
    pub drag: Option<DragMode>,
    /// Typed error from the last operation.
    pub error: Option<String>,
    /// Text placed by **Add text**.
    draft_text: String,
    /// Text shown by the inline editor for the selected text object.
    edit_text: String,
    /// Whether the inline text editor is open for the selected text object.
    text_editing: bool,
    /// `true` while the next canvas click places a new text box.
    placing_text: bool,
    /// Image bytes and format armed for the next canvas click.
    placing_image: Option<(Vec<u8>, ImageFormat)>,
    /// Pointer position where the current drag began, in PDF points.
    drag_start: Point,
    /// Latest pointer position of the current drag, in PDF points.
    drag_current: Point,
    /// Last toolbar window rectangle, used to ignore clicks on the card itself.
    window_rect: Option<egui::Rect>,
}

impl Default for EditToolState {
    fn default() -> Self {
        Self {
            active: false,
            selection: None,
            drag: None,
            error: None,
            draft_text: "New text".to_owned(),
            edit_text: String::new(),
            text_editing: false,
            placing_text: false,
            placing_image: None,
            drag_start: Point::new(0.0, 0.0),
            drag_current: Point::new(0.0, 0.0),
            window_rect: None,
        }
    }
}

impl PdfxiApp {
    /// Handle [`crate::shell::commands::AppCommand::EditDocument`]: toggle the
    /// tool, explaining when no document is open.
    pub(crate) fn handle_edit_document_command(&mut self) {
        if self.edit.active {
            self.deactivate_edit_tool();
            return;
        }
        if self.state.active_tab_id().is_none() {
            self.status = "Open a document to edit".to_owned();
            return;
        }
        self.edit.active = true;
        self.edit.selection = None;
        self.edit.drag = None;
        self.edit.placing_text = false;
        self.edit.placing_image = None;
        self.edit.text_editing = false;
        self.edit.edit_text.clear();
        self.edit.error = None;
        self.status = "Edit PDF: click an object".to_owned();
    }

    /// Leave the tool and return to the plain document view.
    pub(crate) fn deactivate_edit_tool(&mut self) {
        self.edit.active = false;
        self.edit.selection = None;
        self.edit.drag = None;
        self.edit.placing_text = false;
        self.edit.placing_image = None;
        self.edit.text_editing = false;
        self.edit.edit_text.clear();
        self.status = active_tab_status(&self.state);
    }

    /// Whether the Edit PDF tool is active (harness accessor).
    #[must_use]
    pub fn edit_tool_active(&self) -> bool {
        self.edit.active
    }

    /// The selected object's page and list index (harness accessor).
    #[must_use]
    pub fn edit_tool_selected(&self) -> Option<(usize, usize)> {
        self.edit
            .selection
            .as_ref()
            .map(|selection| (selection.page, selection.object))
    }

    /// Select the topmost object on `page` containing `(x, y)` in PDF points.
    ///
    /// Returns `false` and clears the selection when nothing is hit. A listing
    /// failure is recorded as a typed error.
    pub(crate) fn edit_hit_test(&mut self, page: usize, x: f32, y: f32) -> bool {
        let Some(document) = self.edit_document() else {
            self.edit.selection = None;
            return false;
        };
        match list_objects(&document, page) {
            Ok(objects) => {
                let point = Point::new(x, y);
                if let Some((index, object)) = objects
                    .iter()
                    .enumerate()
                    .rev()
                    .find(|(_, object)| object.rect.contains_point(point))
                {
                    self.edit.selection = Some(selection_from(object, index));
                    self.edit.text_editing = false;
                    self.edit.error = None;
                    self.status = format!(
                        "Edit PDF: selected {} on page {}",
                        object_kind_label(object.kind),
                        page + 1
                    );
                    true
                } else {
                    self.edit.selection = None;
                    self.edit.text_editing = false;
                    self.status = "Edit PDF: no object at that point".to_owned();
                    false
                }
            }
            Err(error) => {
                self.edit.selection = None;
                self.edit.error = Some(error.to_string());
                false
            }
        }
    }

    /// Move the selected object so its lower-left corner is `(x, y)`.
    ///
    /// Always applies to `page`; a selection on another page is rejected.
    pub(crate) fn move_selection_to(&mut self, page: usize, x: f32, y: f32) -> Result<(), String> {
        let selection = self
            .edit
            .selection
            .clone()
            .ok_or_else(|| "Select an object first".to_owned())?;
        if selection.page != page {
            return Err("The selected object is on another page".to_owned());
        }
        let dx = x - selection.rect[0];
        let dy = y - selection.rect[1];
        if dx.abs() < 0.01 && dy.abs() < 0.01 {
            return Ok(());
        }
        let (id, mut candidate) = self.active_edit_target()?;
        let object_id = object_id_at(&candidate, page, selection.object)?;
        move_object(
            &mut candidate,
            page,
            &object_id,
            f64::from(dx),
            f64::from(dy),
        )
        .map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.refresh_selection();
        self.edit.error = None;
        self.status = "Edit PDF: moved the object".to_owned();
        Ok(())
    }

    /// Resize the selected object by dragging `handle` to `(x, y)`.
    ///
    /// The opposite corner stays fixed and the object scales about its
    /// lower-left origin, matching [`resize_object`]'s contract.
    pub(crate) fn resize_selection(
        &mut self,
        handle: Handle,
        x: f32,
        y: f32,
    ) -> Result<(), String> {
        let selection = self
            .edit
            .selection
            .clone()
            .ok_or_else(|| "Select an object first".to_owned())?;
        let rect = selection
            .as_rect()
            .ok_or_else(|| "The selection rectangle is invalid".to_owned())?;
        let (sx, sy) = resize_scales(handle, &rect, Point::new(x, y))
            .ok_or_else(|| "The selection rectangle is degenerate".to_owned())?;
        if (sx - 1.0).abs() < 0.01 && (sy - 1.0).abs() < 0.01 {
            return Ok(());
        }
        let (id, mut candidate) = self.active_edit_target()?;
        let object_id = object_id_at(&candidate, selection.page, selection.object)?;
        resize_object(
            &mut candidate,
            selection.page,
            &object_id,
            f64::from(sx),
            f64::from(sy),
        )
        .map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.refresh_selection();
        self.edit.error = None;
        self.status = "Edit PDF: resized the object".to_owned();
        Ok(())
    }

    /// Delete the selected object.
    pub(crate) fn delete_selection(&mut self) -> Result<(), String> {
        let selection = self
            .edit
            .selection
            .clone()
            .ok_or_else(|| "Select an object first".to_owned())?;
        let (id, mut candidate) = self.active_edit_target()?;
        let object_id = object_id_at(&candidate, selection.page, selection.object)?;
        delete_object(&mut candidate, selection.page, &object_id)
            .map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.edit.selection = None;
        self.edit.drag = None;
        self.edit.error = None;
        self.status = "Edit PDF: deleted the object".to_owned();
        Ok(())
    }

    /// Place a new text box whose lower-left corner is `(x, y)`.
    ///
    /// The text comes from the toolbar's **Add text** field. The box is clamped
    /// to the page so a click near an edge still places usable content.
    pub(crate) fn add_text_at(&mut self, page: usize, x: f32, y: f32) -> Result<(), String> {
        let text = self.edit.draft_text.clone();
        if text.trim().is_empty() {
            return Err("Type the text to add first".to_owned());
        }
        let (width, height) = self.page_extent(page)?;
        let box_height = 28.0_f32.min(height).max(1.0);
        let box_width = 220.0_f32.min(width).max(1.0);
        let x0 = x.clamp(0.0, (width - box_width).max(0.0));
        let y0 = y.clamp(0.0, (height - box_height).max(0.0));
        let rect = Rect::new(x0, y0, x0 + box_width, y0 + box_height)
            .map_err(|error| error.to_string())?;
        let (id, mut candidate) = self.active_edit_target()?;
        add_text_box(&mut candidate, page, rect, &text, 12.0, TextAlign::Left)
            .map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.edit.error = None;
        self.status = "Edit PDF: added text".to_owned();
        Ok(())
    }

    /// Begin in-place editing of the text block under `(x, y)`.
    ///
    /// Selects the object, then prefills the inline editor from
    /// [`text_blocks`]. Returns `false` when the click misses or hits a
    /// non-text object.
    pub(crate) fn begin_text_edit_at(&mut self, page: usize, x: f32, y: f32) -> bool {
        if !self.edit_hit_test(page, x, y) {
            self.edit.text_editing = false;
            return false;
        }
        let Some(selection) = self.edit.selection.clone() else {
            return false;
        };
        let Some(document) = self.edit_document() else {
            return false;
        };
        let Ok(objects) = list_objects(&document, page) else {
            return false;
        };
        let Some(object) = objects.get(selection.object) else {
            return false;
        };
        if object.kind != ObjectKind::TextBlock {
            self.edit.text_editing = false;
            self.status = "Edit PDF: only text objects can be edited in place".to_owned();
            return false;
        }
        let Ok(model) = text_blocks(&document, page) else {
            return false;
        };
        let Some(block) = model
            .blocks
            .iter()
            .find(|block| block.index == selection.object)
        else {
            return false;
        };
        let joined = block
            .runs
            .iter()
            .map(|run| run.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        self.edit.edit_text = joined.split_whitespace().collect::<Vec<_>>().join(" ");
        self.edit.text_editing = true;
        self.edit.error = None;
        self.status = "Edit PDF: editing text; choose Apply text".to_owned();
        true
    }

    /// Replace the selected text block with the inline editor's text.
    ///
    /// The block keeps its width and font rules; [`replace_object`] reflows the
    /// replacement through the library's paragraph path.
    pub(crate) fn apply_edit_text(&mut self) -> Result<(), String> {
        let selection = self
            .edit
            .selection
            .clone()
            .ok_or_else(|| "Select a text object first".to_owned())?;
        let text = self.edit.edit_text.clone();
        if text.trim().is_empty() {
            return Err("Type the replacement text first".to_owned());
        }
        let (id, mut candidate) = self.active_edit_target()?;
        let object_id = object_id_at(&candidate, selection.page, selection.object)?;
        replace_object(
            &mut candidate,
            selection.page,
            &object_id,
            ObjectReplacement::Text(&text),
        )
        .map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.edit.selection = None;
        self.edit.drag = None;
        self.edit.text_editing = false;
        self.edit.error = None;
        self.status = "Edit PDF: text replaced".to_owned();
        Ok(())
    }

    /// Place `bytes` as an image centred on `(x, y)`.
    pub(crate) fn add_image_at(
        &mut self,
        page: usize,
        x: f32,
        y: f32,
        bytes: Vec<u8>,
        format: ImageFormat,
    ) -> Result<(), String> {
        let (width, height) = self.page_extent(page)?;
        let size = 120.0_f32.min(width).min(height).max(2.0);
        let x0 = (x - size * 0.5).clamp(0.0, (width - size).max(0.0));
        let y0 = (y - size * 0.5).clamp(0.0, (height - size).max(0.0));
        let rect = Rect::new(x0, y0, x0 + size, y0 + size).map_err(|error| error.to_string())?;
        let (id, mut candidate) = self.active_edit_target()?;
        add_image(&mut candidate, page, rect, &bytes, format).map_err(|error| error.to_string())?;
        self.commit_edit(id, candidate)?;
        self.edit.selection = None;
        self.edit.text_editing = false;
        self.edit.error = None;
        self.status = "Edit PDF: added image".to_owned();
        Ok(())
    }

    /// Pick a PNG or JPEG file and arm it for the next canvas click.
    pub(crate) fn choose_image_file(&mut self) {
        let Some(path) = rfd::FileDialog::new()
            .set_title("Choose an image")
            .add_filter("Images", &["png", "jpg", "jpeg"])
            .pick_file()
        else {
            return;
        };
        match load_image_file(&path) {
            Ok((bytes, format)) => {
                self.edit.placing_image = Some((bytes, format));
                self.edit.placing_text = false;
                self.edit.error = None;
                self.status = "Edit PDF: click where the image should go".to_owned();
            }
            Err(error) => self.edit.error = Some(error),
        }
    }

    /// Canvas press/drag/release handling for the active edit tool.
    pub(crate) fn edit_tool_pointer(
        &mut self,
        layout: &CanvasLayout,
        origin: egui::Pos2,
        pad_x: f32,
        pointer: CanvasPointer,
    ) {
        let CanvasPointer {
            hover,
            pressed,
            released,
            double_clicked,
        } = pointer;
        if !self.edit.active {
            return;
        }
        // Clicks on the floating card must not select or clear page objects.
        if let (Some(rect), Some(pos)) = (self.edit.window_rect, hover)
            && rect.contains(pos)
        {
            return;
        }
        let pointer = hover.and_then(|pos| {
            let canvas = screen_to_canvas(pos, origin, self.scroll_x - pad_x, self.scroll_y);
            let (page, x, y_down) = layout.page_point(canvas)?;
            Some((page, self.page_point_to_pdf(page, x, y_down)))
        });

        if double_clicked {
            let Some((page, point)) = pointer else {
                return;
            };
            self.begin_text_edit_at(page, point.x, point.y);
            return;
        }

        if pressed {
            let Some((page, point)) = pointer else {
                self.edit.selection = None;
                self.edit.drag = None;
                return;
            };
            if let Some((bytes, format)) = self.edit.placing_image.take() {
                if let Err(error) = self.add_image_at(page, point.x, point.y, bytes, format) {
                    self.edit.error = Some(error);
                }
                return;
            }
            if self.edit.placing_text {
                self.edit.placing_text = false;
                if let Err(error) = self.add_text_at(page, point.x, point.y) {
                    self.edit.error = Some(error);
                }
                return;
            }
            if let Some(selection) = self.edit.selection.clone()
                && selection.page == page
                && let Some(rect) = selection.as_rect()
                && let Some(handle) = edit_handle_at(&rect, point)
            {
                self.edit.drag = Some(DragMode::Resize(handle));
                self.edit.drag_start = point;
                self.edit.drag_current = point;
                return;
            }
            if self.edit_hit_test(page, point.x, point.y) {
                self.edit.drag = Some(DragMode::Move);
                self.edit.drag_start = point;
                self.edit.drag_current = point;
            } else {
                self.edit.drag = None;
            }
            return;
        }

        if let Some((page, point)) = pointer
            && self.edit.drag.is_some()
            && self.edit.selection.as_ref().is_some_and(|s| s.page == page)
        {
            self.edit.drag_current = point;
        }
        if released && let Some(mode) = self.edit.drag.take() {
            self.finish_edit_drag(mode);
        }
    }

    /// Draw the selection rectangle and its four corner handles.
    pub(crate) fn edit_tool_overlay(
        &self,
        painter: &egui::Painter,
        layout: &CanvasLayout,
        origin: egui::Pos2,
        pad_x: f32,
    ) {
        if !self.edit.active {
            return;
        }
        let Some(selection) = &self.edit.selection else {
            return;
        };
        let Some(rect) = self.dragged_rect(selection) else {
            return;
        };
        let Some(screen) = self.page_rect_to_screen(layout, selection.page, &rect, origin, pad_x)
        else {
            return;
        };
        let accent = egui::Color32::from_rgb(0, 122, 204);
        painter.rect_stroke(
            screen,
            0.0,
            egui::Stroke::new(1.5_f32, accent),
            egui::StrokeKind::Outside,
        );
        for center in screen_handle_positions(screen) {
            let handle = egui::Rect::from_center_size(center, egui::vec2(7.0, 7.0));
            painter.rect_filled(handle, 0.0, accent);
            painter.rect_stroke(
                handle,
                0.0,
                egui::Stroke::new(1.0_f32, egui::Color32::WHITE),
                egui::StrokeKind::Inside,
            );
        }
    }

    /// Draw the Edit PDF toolbar window.
    ///
    /// Button state is applied directly to the tool; the action is a session
    /// edit already covered by the `EditDocument` command proof and the unit
    /// tests, so this function is allowlisted by the no-dead-controls scanner.
    pub(crate) fn edit_tool_ui(&mut self, ctx: &egui::Context) {
        if !self.edit.active {
            return;
        }
        let mut open = true;
        let mut done = false;
        let mut delete = false;
        let mut add_text = false;
        let mut add_image_file = false;
        let mut apply_text = false;

        let response = egui::Window::new("Edit PDF")
            .open(&mut open)
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.label(
                    egui::RichText::new(
                        "Click an object to select it. Drag to move, drag a corner to resize, \
                         press Delete to remove it.",
                    )
                    .size(10.0)
                    .weak(),
                );
                if let Some(selection) = &self.edit.selection {
                    let width = selection.rect[2] - selection.rect[0];
                    let height = selection.rect[3] - selection.rect[1];
                    ui.label(format!(
                        "{} on page {}: x {:.1}, y {:.1}, {:.1} x {:.1}",
                        selection.label,
                        selection.page + 1,
                        selection.rect[0],
                        selection.rect[1],
                        width,
                        height
                    ));
                } else {
                    ui.label("No object selected");
                }
                ui.horizontal(|ui| {
                    if ui.button("Done").clicked() {
                        done = true;
                    }
                    if ui
                        .add_enabled(self.edit.selection.is_some(), egui::Button::new("Delete"))
                        .clicked()
                    {
                        delete = true;
                    }
                    if ui.button("Add text").clicked() {
                        add_text = true;
                    }
                    if ui.button("Add image").clicked() {
                        add_image_file = true;
                    }
                });
                ui.horizontal(|ui| {
                    ui.label("Text");
                    ui.add(
                        egui::TextEdit::singleline(&mut self.edit.draft_text)
                            .hint_text("Text to place")
                            .desired_width(220.0),
                    );
                });
                if self.edit.placing_text {
                    ui.label("Click on the page to place the text.");
                }
                if self.edit.placing_image.is_some() {
                    ui.label("Click on the page to place the image.");
                }
                if self.edit.text_editing {
                    ui.separator();
                    ui.label("Replace text");
                    ui.add(
                        egui::TextEdit::multiline(&mut self.edit.edit_text)
                            .desired_rows(3)
                            .desired_width(f32::INFINITY),
                    );
                    if ui.button("Apply text").clicked() {
                        apply_text = true;
                    }
                }
                if let Some(error) = &self.edit.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });
        if let Some(response) = response {
            self.edit.window_rect = Some(response.response.rect);
        }

        if add_text {
            self.edit.placing_text = true;
            self.edit.placing_image = None;
            self.edit.error = None;
            self.status = "Edit PDF: click where the text should go".to_owned();
        }
        if add_image_file {
            self.choose_image_file();
        }
        if apply_text && let Err(error) = self.apply_edit_text() {
            self.edit.error = Some(error);
        }
        if delete && let Err(error) = self.delete_selection() {
            self.edit.error = Some(error);
        }
        if !open || done {
            self.deactivate_edit_tool();
        }
    }

    /// The active document cloned for a library mutation.
    fn active_edit_target(&self) -> Result<(TabId, Document), String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to edit".to_owned());
        };
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        let Some(session) = self.sessions.get(&id) else {
            return Err("The document has no open session".to_owned());
        };
        if let Some(report) = session.decryption_report()
            && !report.permissions.allows(Permission::MODIFY)
        {
            return Err("The document permissions do not allow changes".to_owned());
        }
        Ok((id, session.document().clone()))
    }

    /// The active document, read-only, for hit testing.
    fn edit_document(&self) -> Option<Document> {
        let id = self.state.active_tab_id()?;
        self.sessions
            .get(&id)
            .map(|session| session.document().clone())
    }

    /// Commit a mutated clone as one undoable session edit.
    fn commit_edit(&mut self, id: TabId, candidate: Document) -> Result<(), String> {
        let result = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit(|document| {
                *document = candidate;
                Ok(())
            });
        match result {
            Ok(()) => {
                self.after_session_edit(id);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Refresh the cached selection rectangle after a successful edit.
    fn refresh_selection(&mut self) {
        let Some(selection) = self.edit.selection.clone() else {
            return;
        };
        let Some(document) = self.edit_document() else {
            return;
        };
        let Ok(objects) = list_objects(&document, selection.page) else {
            return;
        };
        if let Some(object) = objects.get(selection.object)
            && let Some(current) = &mut self.edit.selection
        {
            *current = selection_from(object, selection.object);
        }
    }

    /// Apply an in-progress drag once the pointer is released.
    fn finish_edit_drag(&mut self, mode: DragMode) {
        let Some(selection) = self.edit.selection.clone() else {
            return;
        };
        let outcome = match mode {
            DragMode::Move => {
                let dx = self.edit.drag_current.x - self.edit.drag_start.x;
                let dy = self.edit.drag_current.y - self.edit.drag_start.y;
                if dx.abs() < 0.01 && dy.abs() < 0.01 {
                    return;
                }
                self.move_selection_to(
                    selection.page,
                    selection.rect[0] + dx,
                    selection.rect[1] + dy,
                )
            }
            DragMode::Resize(handle) => {
                self.resize_selection(handle, self.edit.drag_current.x, self.edit.drag_current.y)
            }
        };
        if let Err(error) = outcome {
            self.edit.error = Some(error);
        }
    }

    /// Selection rectangle adjusted by the in-progress drag, in PDF points.
    fn dragged_rect(&self, selection: &EditSelection) -> Option<Rect> {
        let rect = selection.as_rect()?;
        let Some(mode) = self.edit.drag else {
            return Some(rect);
        };
        match mode {
            DragMode::Move => {
                let dx = self.edit.drag_current.x - self.edit.drag_start.x;
                let dy = self.edit.drag_current.y - self.edit.drag_start.y;
                Rect::new(
                    rect.x0() + dx,
                    rect.y0() + dy,
                    rect.x1() + dx,
                    rect.y1() + dy,
                )
                .ok()
            }
            DragMode::Resize(handle) => {
                let (sx, sy) = resize_scales(handle, &rect, self.edit.drag_current)?;
                Rect::new(
                    rect.x0(),
                    rect.y0(),
                    rect.x0() + rect.width() * sx,
                    rect.y0() + rect.height() * sy,
                )
                .ok()
            }
        }
    }

    /// Map a PDF-space rectangle on `page` to an on-screen rectangle.
    fn page_rect_to_screen(
        &self,
        layout: &CanvasLayout,
        page: usize,
        rect: &Rect,
        origin: egui::Pos2,
        pad_x: f32,
    ) -> Option<egui::Rect> {
        let page_height = self
            .page_sizes
            .get(page)
            .map_or(792.0, |size| size.height_pt);
        let local = CanvasRect::new(
            rect.x0(),
            page_height - rect.y1(),
            rect.width(),
            rect.height(),
        );
        let mapped = layout.page_rect_to_canvas(page, local)?;
        Some(egui::Rect::from_min_size(
            egui::pos2(
                origin.x + mapped.x - self.scroll_x + pad_x,
                origin.y + mapped.y - self.scroll_y,
            ),
            egui::vec2(mapped.width, mapped.height),
        ))
    }

    /// Convert a top-left-origin page point into PDF coordinates (y up).
    fn page_point_to_pdf(&self, page: usize, x: f32, y_down: f32) -> Point {
        let height = self
            .page_sizes
            .get(page)
            .map_or(792.0, |size| size.height_pt);
        Point::new(x, height - y_down)
    }

    /// Intrinsic page width and height in points.
    fn page_extent(&self, page: usize) -> Result<(f32, f32), String> {
        self.page_sizes
            .get(page)
            .map(|size| (size.width_pt, size.height_pt))
            .ok_or_else(|| format!("Page {} is not part of the document", page + 1))
    }
}

/// Build a selection entry from a listed object.
fn selection_from(object: &PageObject, index: usize) -> EditSelection {
    EditSelection {
        page: object.page,
        object: index,
        rect: [
            object.rect.x0(),
            object.rect.y0(),
            object.rect.x1(),
            object.rect.y1(),
        ],
        label: match object.kind {
            ObjectKind::TextBlock if !object.text_preview.is_empty() => {
                format!("Text \"{}\"", object.text_preview)
            }
            ObjectKind::TextBlock => "Text block".to_owned(),
            ObjectKind::Image => "Image".to_owned(),
        },
    }
}

/// The identifier of the object at `index` in the current page listing.
fn object_id_at(document: &Document, page: usize, index: usize) -> Result<String, String> {
    let objects = list_objects(document, page).map_err(|error| error.to_string())?;
    objects
        .get(index)
        .map(|object| object.id.clone())
        .ok_or_else(|| "The selected object no longer exists".to_owned())
}

/// Handle under `point`, if any.
fn edit_handle_at(rect: &Rect, point: Point) -> Option<Handle> {
    let tolerance = (rect.width().min(rect.height()) * 0.2).clamp(2.0, 8.0);
    Handle::ALL.into_iter().find(|handle| {
        let center = handle_center(*handle, rect);
        (point.x - center.x).abs() <= tolerance && (point.y - center.y).abs() <= tolerance
    })
}

/// Centre of one corner handle in PDF points.
fn handle_center(handle: Handle, rect: &Rect) -> Point {
    match handle {
        Handle::NorthWest => Point::new(rect.x0(), rect.y1()),
        Handle::NorthEast => Point::new(rect.x1(), rect.y1()),
        Handle::SouthEast => Point::new(rect.x1(), rect.y0()),
        Handle::SouthWest => Point::new(rect.x0(), rect.y0()),
    }
}

/// Scale factors implied by dragging `handle` to `current`, anchored at the
/// object's lower-left origin (the anchor [`resize_object`] documents).
fn resize_scales(handle: Handle, rect: &Rect, current: Point) -> Option<(f32, f32)> {
    let width = rect.width();
    let height = rect.height();
    if width <= 0.0 || height <= 0.0 {
        return None;
    }
    let sx = match handle {
        Handle::NorthWest | Handle::SouthWest => (rect.x1() - current.x) / width,
        Handle::NorthEast | Handle::SouthEast => (current.x - rect.x0()) / width,
    };
    let sy = match handle {
        Handle::NorthWest | Handle::NorthEast => (current.y - rect.y0()) / height,
        Handle::SouthWest | Handle::SouthEast => (rect.y1() - current.y) / height,
    };
    Some((sx.clamp(0.05, 10.0), sy.clamp(0.05, 10.0)))
}

/// Four corner positions on an on-screen selection rectangle.
fn screen_handle_positions(rect: egui::Rect) -> [egui::Pos2; 4] {
    [
        egui::pos2(rect.left(), rect.top()),
        egui::pos2(rect.right(), rect.top()),
        egui::pos2(rect.right(), rect.bottom()),
        egui::pos2(rect.left(), rect.bottom()),
    ]
}

/// Short human label for a page object kind.
fn object_kind_label(kind: ObjectKind) -> &'static str {
    match kind {
        ObjectKind::TextBlock => "text",
        ObjectKind::Image => "image",
    }
}

/// Read a PNG or JPEG image and identify its encoding from the file bytes.
///
/// Kept free of `rfd` so tests exercise the same validation the file picker
/// uses without opening a native dialog.
pub(crate) fn load_image_file(path: &Path) -> Result<(Vec<u8>, ImageFormat), String> {
    let bytes = std::fs::read(path)
        .map_err(|error| format!("Could not read {}: {error}", path.display()))?;
    let format = if bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]) {
        ImageFormat::Png
    } else if bytes.starts_with(&[0xFF, 0xD8]) {
        ImageFormat::Jpeg
    } else {
        return Err(format!("{} is not a PNG or JPEG image", path.display()));
    };
    Ok((bytes, format))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn app_with_fixture() -> PdfxiApp {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture());
        assert!(
            app.state.active_tab_id().is_some(),
            "the Excel fixture opens"
        );
        app.edit.active = true;
        app
    }

    fn active_document(app: &PdfxiApp) -> Document {
        let id = app.state.active_tab_id().expect("an active tab");
        app.sessions
            .get(&id)
            .expect("an open session")
            .document()
            .clone()
    }

    /// Save the current session through `save_as` and parse the file again, so
    /// assertions cover the bytes a later reader would see.
    fn save_and_reopen(app: &mut PdfxiApp, label: &str) -> Document {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/edit-tool");
        std::fs::create_dir_all(&directory).expect("create the edit-tool temp directory");
        let path = directory.join(format!("{label}-{}.pdf", std::process::id()));
        let id = app.state.active_tab_id().expect("an active tab");
        app.sessions
            .get_mut(&id)
            .expect("an open session")
            .save_as(&path)
            .expect("save the edited document");
        let bytes = std::fs::read(&path).expect("read the saved document");
        let _ = std::fs::remove_file(&path);
        Document::from_bytes(bytes).expect("reopen the saved document")
    }

    /// Select the first text block by clicking its centre.
    fn select_first_text(app: &mut PdfxiApp) -> (usize, EditSelection) {
        let document = active_document(app);
        let objects = list_objects(&document, 0).expect("list page objects");
        let index = objects
            .iter()
            .position(|object| object.kind == ObjectKind::TextBlock)
            .expect("the fixture has a text block");
        let object = &objects[index];
        let cx = (object.rect.x0() + object.rect.x1()) * 0.5;
        let cy = (object.rect.y0() + object.rect.y1()) * 0.5;
        assert!(
            app.edit_hit_test(0, cx, cy),
            "the click hits the first text block"
        );
        let selection = app.edit.selection.clone().expect("a selection");
        assert_eq!(selection.object, index);
        (index, selection)
    }

    #[test]
    fn hit_test_selects_the_topmost_listed_object() {
        let mut app = app_with_fixture();
        let document = active_document(&app);
        let objects = list_objects(&document, 0).expect("list page objects");
        let index = objects
            .iter()
            .position(|object| object.kind == ObjectKind::TextBlock)
            .expect("a text block");
        let object = &objects[index];
        let cx = (object.rect.x0() + object.rect.x1()) * 0.5;
        let cy = (object.rect.y0() + object.rect.y1()) * 0.5;

        assert!(app.edit_hit_test(0, cx, cy));
        assert_eq!(app.edit_tool_selected(), Some((0, index)));
        let selected = app.edit.selection.as_ref().expect("a selection");
        assert!((selected.rect[0] - object.rect.x0()).abs() < 0.01);
        assert!(selected.label.contains("Text"));

        assert!(
            !app.edit_hit_test(0, -25.0, -25.0),
            "empty space selects nothing"
        );
        assert!(app.edit_tool_selected().is_none());
    }

    #[test]
    fn moving_a_text_block_updates_the_reopened_rect() {
        let mut app = app_with_fixture();
        let (index, selection) = select_first_text(&mut app);
        let before = selection.rect;

        app.move_selection_to(0, before[0] + 20.0, before[1])
            .expect("move the text block 20 points right");

        let document = save_and_reopen(&mut app, "move");
        let objects = list_objects(&document, 0).expect("list after the move");
        let moved = objects.get(index).expect("the block survives the move");
        assert!(
            (moved.rect.x0() - (before[0] + 20.0)).abs() < 0.6,
            "x0 {} did not move by 20 from {}",
            moved.rect.x0(),
            before[0]
        );
        let refreshed = app.edit.selection.as_ref().expect("still selected");
        assert!((refreshed.rect[0] - (before[0] + 20.0)).abs() < 0.6);
    }

    #[test]
    fn deleting_a_text_block_drops_one_object() {
        let mut app = app_with_fixture();
        let (_, selection) = select_first_text(&mut app);
        let before_count = list_objects(&active_document(&app), 0)
            .expect("list before delete")
            .len();

        app.delete_selection().expect("delete the text block");

        let document = save_and_reopen(&mut app, "delete");
        let objects = list_objects(&document, 0).expect("list after the delete");
        assert_eq!(
            objects.len() + 1,
            before_count,
            "exactly one object should disappear"
        );
        assert!(
            !objects
                .iter()
                .any(|object| object.rect.contains_point(Point::new(
                    (selection.rect[0] + selection.rect[2]) * 0.5,
                    (selection.rect[1] + selection.rect[3]) * 0.5,
                )) && object.kind == ObjectKind::TextBlock),
            "the deleted block still covers its centre"
        );
        assert!(app.edit.selection.is_none(), "the selection is cleared");
    }

    #[test]
    fn adding_text_places_extractable_text() {
        let mut app = app_with_fixture();
        app.edit.draft_text = "ZebraQuarantine".to_owned();
        app.add_text_at(0, 72.0, 72.0).expect("add a text box");

        let document = save_and_reopen(&mut app, "add-text");
        let text = document.page_text(0).expect("page text");
        assert!(
            text.contains("ZebraQuarantine"),
            "the placed text is missing from {text:?}"
        );
    }

    #[test]
    fn resizing_a_text_block_scales_the_reopened_rect() {
        let mut app = app_with_fixture();
        let (index, selection) = select_first_text(&mut app);
        let before = selection.rect;
        let width = before[2] - before[0];
        let height = before[3] - before[1];

        app.resize_selection(
            Handle::NorthEast,
            before[0] + width * 1.2,
            before[1] + height * 1.2,
        )
        .expect("resize the text block");

        let document = save_and_reopen(&mut app, "resize");
        let objects = list_objects(&document, 0).expect("list after the resize");
        let resized = objects.get(index).expect("the block survives the resize");
        assert!(
            (resized.rect.width() - width * 1.2).abs() <= width * 0.03 + 0.5,
            "width {} did not scale to {}",
            resized.rect.width(),
            width * 1.2
        );
        assert!(
            (resized.rect.height() - height * 1.2).abs() <= height * 0.03 + 0.5,
            "height {} did not scale to {}",
            resized.rect.height(),
            height * 1.2
        );
    }

    #[test]
    fn corner_handles_map_to_expected_scales() {
        let rect = Rect::new(10.0, 20.0, 110.0, 80.0).expect("a valid rect");
        assert_eq!(
            edit_handle_at(&rect, Point::new(110.0, 80.0)),
            Some(Handle::NorthEast)
        );
        assert_eq!(
            edit_handle_at(&rect, Point::new(10.0, 20.0)),
            Some(Handle::SouthWest)
        );
        let (sx, sy) =
            resize_scales(Handle::NorthEast, &rect, Point::new(210.0, 140.0)).expect("scales");
        assert!((sx - 2.0).abs() < 0.01, "sx {sx}");
        assert!((sy - 2.0).abs() < 0.01, "sy {sy}");
        assert!(edit_handle_at(&rect, Point::new(60.0, 50.0)).is_none());
    }

    #[test]
    fn in_place_text_editing_replaces_the_reopened_text() {
        let mut app = app_with_fixture();
        let document = active_document(&app);
        let model = text_blocks(&document, 0).expect("page text model");
        let block = model
            .blocks
            .iter()
            .find(|block| block.runs.iter().any(|run| run.text.contains("orchestra")))
            .expect("the fixture has the orchestra sentence");
        let cx = (block.rect.x0() + block.rect.x1()) * 0.5;
        let cy = (block.rect.y0() + block.rect.y1()) * 0.5;

        assert!(
            app.begin_text_edit_at(0, cx, cy),
            "the double-click opens the inline editor"
        );
        assert!(app.edit.text_editing);
        assert!(
            app.edit.edit_text.contains("orchestra"),
            "the editor should prefill the block text, got {:?}",
            app.edit.edit_text
        );

        app.edit.edit_text = "MarmaladeReflowMarker".to_owned();
        app.apply_edit_text().expect("apply the replacement text");
        assert!(app.edit.selection.is_none(), "the selection is cleared");
        assert!(!app.edit.text_editing, "the editor closes");
        assert!(app.status().contains("text replaced"));

        let document = save_and_reopen(&mut app, "replace-text");
        let text = document.page_text(0).expect("page text");
        assert!(
            text.contains("MarmaladeReflowMarker"),
            "the replacement is missing from {text:?}"
        );
        assert!(
            !text.contains("orchestra"),
            "the old sentence survived in {text:?}"
        );
    }

    #[test]
    fn adding_an_image_creates_a_listed_image_object_at_the_click() {
        let mut app = app_with_fixture();
        let (width, height) = app.page_extent(0).expect("page extent");
        let (cx, cy) = (width * 0.5, height * 0.5);

        app.add_image_at(0, cx, cy, solid_png(), ImageFormat::Png)
            .expect("add an image");

        let document = save_and_reopen(&mut app, "add-image");
        let objects = list_objects(&document, 0).expect("list after the image add");
        let image = objects
            .iter()
            .find(|object| object.kind == ObjectKind::Image)
            .expect("an image object is listed after the add");
        assert!(
            image.rect.contains_point(Point::new(cx, cy)),
            "image rect {} {} {} {} does not contain the click ({cx}, {cy})",
            image.rect.x0(),
            image.rect.y0(),
            image.rect.x1(),
            image.rect.y1()
        );
    }

    #[test]
    fn load_image_file_identifies_png_and_rejects_other_bytes() {
        let directory =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/edit-tool");
        std::fs::create_dir_all(&directory).expect("create the edit-tool temp directory");
        let png = directory.join(format!("tiny-{}.png", std::process::id()));
        let bad = directory.join(format!("bad-{}.png", std::process::id()));
        let bytes = solid_png();
        std::fs::write(&png, &bytes).expect("write the PNG");

        let (loaded, format) = load_image_file(&png).expect("load the PNG");
        assert_eq!(format, ImageFormat::Png);
        assert_eq!(loaded, bytes);

        std::fs::write(&bad, b"not an image").expect("write the bad file");
        assert!(load_image_file(&bad).is_err());
        let _ = std::fs::remove_file(&png);
        let _ = std::fs::remove_file(&bad);
    }

    /// A deterministic solid 8x8 RGB PNG.
    fn solid_png() -> Vec<u8> {
        let color = [220u8, 40, 90];
        let mut raw = Vec::new();
        for _ in 0..8 {
            raw.resize(raw.len() + 1, 0);
            for _ in 0..8 {
                raw.extend_from_slice(&color);
            }
        }
        let mut zlib = vec![0x78, 0x01];
        let length = raw.len() as u16;
        zlib.push(1);
        zlib.extend_from_slice(&length.to_le_bytes());
        zlib.extend_from_slice(&(!length).to_le_bytes());
        zlib.extend_from_slice(&raw);
        zlib.extend_from_slice(&adler32(&raw).to_be_bytes());

        let mut png = b"\x89PNG\r\n\x1a\n".to_vec();
        let mut ihdr = Vec::new();
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&8u32.to_be_bytes());
        ihdr.extend_from_slice(&[8, 2, 0, 0, 0]);
        push_chunk(&mut png, b"IHDR", &ihdr);
        push_chunk(&mut png, b"IDAT", &zlib);
        push_chunk(&mut png, b"IEND", &[]);
        png
    }

    fn push_chunk(png: &mut Vec<u8>, kind: &[u8; 4], data: &[u8]) {
        png.extend_from_slice(&(data.len() as u32).to_be_bytes());
        png.extend_from_slice(kind);
        png.extend_from_slice(data);
        png.extend_from_slice(&[0, 0, 0, 0]);
    }

    fn adler32(data: &[u8]) -> u32 {
        let mut a = 1u32;
        let mut b = 0u32;
        for byte in data {
            a = (a + u32::from(*byte)) % 65_521;
            b = (b + a) % 65_521;
        }
        (b << 16) | a
    }
}
