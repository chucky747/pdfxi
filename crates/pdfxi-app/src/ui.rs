//! Native viewer shell built on `egui`/`eframe`.
//!
//! This module is only compiled when the `ui` feature is enabled. It layers a
//! desktop window on top of the UI-agnostic [`AppState`] model: a toolbar, a
//! document tab strip, a status header, and a placeholder canvas area. All
//! document mutations go through [`AppState`] methods, so the state logic stays
//! independent of the window and can be unit tested without one.
//!
//! The canvas draws page rectangles laid out by [`crate::canvas::CanvasLayout`],
//! reports the page under the cursor, and shows copied text from a
//! [`crate::selection::SelectionModel`] in the status line. Real rasterization
//! stays optional: without a renderer, the rectangles and selection overlay are
//! the placeholder. All layout/selection logic lives in window-free modules so
//! it can be tested without this GUI layer.

use egui::Key;

use crate::{
    AppState, CanvasHit, CanvasLayout, CanvasPoint, CanvasSize, DEFAULT_ZOOM, OwnedSpan,
    OwnedTextPage, PageSize, SelectionModel, TabId, Toolset, ViewMode, Zoom, app_name,
};

/// Amount by which each zoom-in / zoom-out step changes a custom zoom scale.
const ZOOM_STEP: f32 = 0.25;

/// Placeholder page count assigned to a demo tab (no real document is loaded).
pub const DEMO_PAGE_COUNT: usize = 3;

/// Width of a demo page in PDF points (US Letter).
const DEMO_PAGE_WIDTH_PT: f32 = 612.0;

/// Height of a demo page in PDF points (US Letter).
const DEMO_PAGE_HEIGHT_PT: f32 = 792.0;

/// Format a one-line status string describing the active document.
///
/// The result contains the active tab's display name, zoom, rotation in
/// degrees, and view-mode label. When no document is open it returns a fixed
/// message. The function is pure and needs no GUI context.
pub fn active_tab_status(state: &AppState) -> String {
    match state.active_tab() {
        Some(tab) => format!(
            "{} | zoom {} | rotation {} deg | view {}",
            tab.display_name(),
            tab.zoom(),
            tab.rotation().degrees(),
            tab.view_mode(),
        ),
        None => "No document open".to_string(),
    }
}

/// Format the placeholder text displayed in the canvas area.
///
/// With an active document this reports the one-based current page and the
/// document's page count; without one it prompts the user to open a document.
/// The function is pure and needs no GUI context.
pub fn canvas_placeholder(state: &AppState) -> String {
    match state.active_tab() {
        Some(tab) => format!(
            "Page {} of {} — page rendering is not available in this shell yet",
            tab.current_page() + 1,
            tab.page_count(),
        ),
        None => "Open a document to begin".to_string(),
    }
}

/// Build deterministic US-Letter page sizes for a demo document of `count` pages.
pub fn demo_page_sizes(count: usize) -> Vec<PageSize> {
    (0..count)
        .map(|_| PageSize::new(DEMO_PAGE_WIDTH_PT, DEMO_PAGE_HEIGHT_PT))
        .collect()
}

/// Build placeholder text pages so selection can be demonstrated without a
/// renderer. Each page carries one short line of text near the top-left.
pub fn demo_text_pages(count: usize) -> Vec<OwnedTextPage> {
    (0..count)
        .map(|page| {
            let text = format!("Page {} demo text", page + 1);
            let width = 7.0 * text.chars().count() as f32;
            OwnedTextPage::new(page, vec![OwnedSpan::new(text, 72.0, 72.0, width, 14.0)])
        })
        .collect()
}

/// Lay out the active tab's demo pages for a viewport, or `None` with no tab.
///
/// This is the only bridge between [`AppState`] and [`CanvasLayout`]; it is pure
/// and window-free so the geometry can be asserted in unit tests.
pub fn layout_for_state(state: &AppState, viewport: CanvasSize) -> Option<CanvasLayout> {
    let tab = state.active_tab()?;
    Some(CanvasLayout::standard(
        tab.view_mode(),
        tab.zoom(),
        viewport,
        &demo_page_sizes(tab.page_count()),
    ))
}

/// Convert a screen-space pointer position into unscrolled canvas coordinates.
///
/// `origin` is the canvas widget's top-left in screen space and `(scroll_x,
/// scroll_y)` is the current scroll offset. This is a pure helper used by the
/// pointer handlers and asserted in unit tests.
pub fn screen_to_canvas(
    pos: egui::Pos2,
    origin: egui::Pos2,
    scroll_x: f32,
    scroll_y: f32,
) -> CanvasPoint {
    CanvasPoint::new(pos.x - origin.x + scroll_x, pos.y - origin.y + scroll_y)
}

/// Compose the one-line canvas info shown in the status area.
///
/// Reports the page under the cursor and, when a selection exists, appends the
/// copy text. Pure and window-free.
pub fn canvas_info_line(hit: CanvasHit, selection_text: &str) -> String {
    let page_part = match hit {
        CanvasHit::Page(index) => format!("Page {}", index + 1),
        CanvasHit::Gap => "Between pages".to_string(),
        CanvasHit::Outside => "Outside pages".to_string(),
    };
    if selection_text.is_empty() {
        page_part
    } else {
        format!("{page_part} | Selected: {selection_text}")
    }
}

/// Launch the native PDFXI viewer window.
///
/// This is the entry point used by the `pdfxi-ui` binary. It builds a
/// [`eframe::NativeOptions`] with a sensible default viewport, starts the
/// eframe event loop with a default [`PdfxiApp`], and logs any startup or
/// runtime error to standard error instead of panicking. The call returns once
/// the window has been closed.
pub fn run() {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app_name())
            .with_inner_size([1280.0, 800.0]),
        ..eframe::NativeOptions::default()
    };

    if let Err(error) = eframe::run_native(
        app_name(),
        options,
        Box::new(|_creation_context| Ok(Box::new(PdfxiApp::default()))),
    ) {
        eprintln!("PDFXI viewer exited with an error: {error}");
    }
}

/// The eframe application implementing the PDFXI viewer shell.
///
/// It owns an [`AppState`] and renders it as a toolbar, a document tab strip, a
/// status header, and a placeholder canvas. Every toolbar button and keyboard
/// shortcut maps to a method on this type that mutates only the underlying
/// state, keeping behavior testable without a window.
#[derive(Default)]
pub struct PdfxiApp {
    state: AppState,
    status: String,
    scroll_x: f32,
    scroll_y: f32,
    selection: SelectionModel,
    demo_text: Vec<OwnedTextPage>,
}

impl PdfxiApp {
    /// Create a viewer with no open documents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Borrow the underlying UI-agnostic application state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The most recent user-facing status message, or an empty string.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Open a new placeholder demo tab, make it active, and return its id.
    pub fn open_demo_tab(&mut self) -> TabId {
        let name = format!("Demo Document {}", self.state.tab_count() + 1);
        let id = self.state.open_tab(name);
        if let Some(tab) = self.state.tab_mut(id) {
            tab.set_page_count(DEMO_PAGE_COUNT);
        }
        self.demo_text = demo_text_pages(DEMO_PAGE_COUNT);
        self.selection.clear();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.status = format!("Opened demo tab {id}");
        id
    }

    /// Close the active tab, if any.
    pub fn close_active_tab(&mut self) {
        match self.state.active_tab_id() {
            Some(id) => {
                let _ = self.state.close_tab(id);
                self.status = format!("Closed tab {id}");
            }
            None => self.status = "No tab to close".to_string(),
        }
    }

    /// Make the tab with the given id active, if it exists.
    pub fn activate_tab(&mut self, id: TabId) {
        match self.state.activate_tab(id) {
            Ok(()) => self.status = format!("Activated tab {id}"),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Increase the active tab's custom zoom by one [`ZOOM_STEP`].
    pub fn zoom_in(&mut self) {
        self.adjust_zoom(ZOOM_STEP, "Zoom in");
    }

    /// Decrease the active tab's custom zoom by one [`ZOOM_STEP`].
    pub fn zoom_out(&mut self) {
        self.adjust_zoom(-ZOOM_STEP, "Zoom out");
    }

    /// Rotate the active tab 90 degrees clockwise.
    pub fn rotate_clockwise(&mut self) {
        match self.state.rotate_clockwise() {
            Ok(rotation) => self.status = format!("Rotated to {} degrees", rotation.degrees()),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Rotate the active tab 90 degrees counterclockwise.
    pub fn rotate_counterclockwise(&mut self) {
        match self.state.rotate_counterclockwise() {
            Ok(rotation) => self.status = format!("Rotated to {} degrees", rotation.degrees()),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Select the interaction tool used on the canvas.
    pub fn select_tool(&mut self, tool: Toolset) {
        self.state.select_tool(tool);
        self.status = format!("Tool: {}", tool.label());
    }

    /// Clear the current text selection (bound to Escape).
    pub fn clear_selection(&mut self) {
        self.selection.clear();
        self.status = "Selection cleared".to_string();
    }

    /// Copy text of the current selection, using the placeholder text pages.
    pub fn selected_text(&self) -> String {
        self.selection.selected_text(&self.demo_text)
    }

    /// Change the active tab's view mode.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        match self.state.set_view_mode(mode) {
            Ok(()) => self.status = format!("View mode: {}", mode.label()),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Apply a relative change to the active tab's custom zoom.
    fn adjust_zoom(&mut self, delta: f32, action: &str) {
        let new_zoom = match self.state.active_tab() {
            Some(tab) => {
                let current = tab.zoom().scale().unwrap_or(DEFAULT_ZOOM);
                Zoom::custom(current + delta)
            }
            None => {
                self.status = "No active tab to zoom".to_string();
                return;
            }
        };
        match self.state.set_zoom(new_zoom) {
            Ok(()) => self.status = format!("{action}: {new_zoom}"),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Render the toolbar panel.
    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(app_name());
            ui.separator();
            if ui.button("Open Demo").clicked() {
                self.open_demo_tab();
            }
            if ui.button("Close").clicked() {
                self.close_active_tab();
            }
            ui.separator();
            if ui.button("Zoom -").clicked() {
                self.zoom_out();
            }
            if ui.button("Zoom +").clicked() {
                self.zoom_in();
            }
            if ui.button("Rotate CCW").clicked() {
                self.rotate_counterclockwise();
            }
            if ui.button("Rotate CW").clicked() {
                self.rotate_clockwise();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Tool:");
            for tool in Toolset::ALL {
                let selected = self.state.active_tool() == tool;
                if ui
                    .selectable_label(
                        selected,
                        format!("{} [{}]", tool.label(), tool.keyboard_hint()),
                    )
                    .clicked()
                {
                    self.select_tool(tool);
                }
            }
        });

        ui.horizontal(|ui| {
            ui.label("View:");
            for mode in [
                ViewMode::SinglePage,
                ViewMode::Continuous,
                ViewMode::TwoPage,
                ViewMode::TwoPageContinuous,
            ] {
                let selected = self
                    .state
                    .active_tab()
                    .is_some_and(|tab| tab.view_mode() == mode);
                if ui.selectable_label(selected, mode.label()).clicked() {
                    self.set_view_mode(mode);
                }
            }
        });
    }

    /// Render the document tab strip.
    fn tabs_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            // Collect ids first so the mutable activate call does not overlap
            // the immutable borrow of the tab collection.
            let ids: Vec<TabId> = self.state.tabs().map(|tab| tab.id()).collect();
            if ids.is_empty() {
                ui.label("No documents open");
                return;
            }
            for id in ids {
                let selected = self.state.active_tab_id() == Some(id);
                let title = self.state.tab_title(id).unwrap_or_default();
                if ui.selectable_label(selected, title).clicked() {
                    self.activate_tab(id);
                }
            }
        });
    }

    /// Render the status header and the placeholder canvas area.
    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        if let Some(tab) = self.state.active_tab() {
            ui.heading(tab.title());
        } else {
            ui.heading("PDFXI Viewer");
        }
        ui.label(active_tab_status(&self.state));
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
        ui.separator();

        egui::Frame::new()
            .fill(ui.visuals().extreme_bg_color)
            .stroke(ui.visuals().widgets.inactive.fg_stroke)
            .inner_margin(egui::Margin::same(8))
            .show(ui, |ui| {
                let viewport = CanvasSize::new(ui.available_width(), ui.available_height());
                let Some(layout) = layout_for_state(&self.state, viewport) else {
                    ui.vertical_centered(|ui| {
                        ui.add_space(48.0);
                        ui.label(egui::RichText::new(canvas_placeholder(&self.state)).weak());
                    });
                    return;
                };

                let origin = ui.min_rect().left_top();
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

                // Text drag: anchor on press, advance the head while dragging.
                if text_tool && let Some(pos) = hover {
                    let point = screen_to_canvas(pos, origin, self.scroll_x, self.scroll_y);
                    if pressed {
                        self.selection.begin_at(point, &layout, &self.demo_text);
                    } else if dragging {
                        self.selection.update_to(point, &layout, &self.demo_text);
                    }
                }

                // Report the page under the cursor in canvas coordinates.
                let hit = hover.map_or(CanvasHit::Outside, |pos| {
                    layout.page_at(screen_to_canvas(pos, origin, self.scroll_x, self.scroll_y))
                });

                // Draw one outlined placeholder rectangle per laid-out page.
                // Real rasterization is optional and intentionally absent here.
                let painter = ui.painter();
                let stroke = ui.visuals().widgets.inactive.fg_stroke;
                for page in layout.pages() {
                    let rect = egui::Rect::from_min_size(
                        egui::pos2(
                            origin.x + page.rect.x - self.scroll_x,
                            origin.y + page.rect.y - self.scroll_y,
                        ),
                        egui::vec2(page.rect.width, page.rect.height),
                    );
                    painter.rect(
                        rect,
                        0.0,
                        egui::Color32::TRANSPARENT,
                        stroke,
                        egui::StrokeKind::Outside,
                    );
                }

                let copy_text = self.selection.selected_text(&self.demo_text);
                ui.label(canvas_info_line(hit, &copy_text));
            });
    }

    /// Apply keyboard shortcuts for the main commands (all with a Ctrl chord).
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let (open, close, zoom_in, zoom_out, rotate_cw, rotate_ccw, escape) = ctx.input(|input| {
            let ctrl = input.modifiers.ctrl;
            let shift = input.modifiers.shift;
            (
                ctrl && input.key_pressed(Key::N),
                ctrl && input.key_pressed(Key::W),
                ctrl && input.key_pressed(Key::Equals),
                ctrl && input.key_pressed(Key::Minus),
                ctrl && !shift && input.key_pressed(Key::R),
                ctrl && shift && input.key_pressed(Key::R),
                input.key_pressed(Key::Escape),
            )
        });

        if open {
            self.open_demo_tab();
        }
        if close {
            self.close_active_tab();
        }
        if zoom_in {
            self.zoom_in();
        }
        if zoom_out {
            self.zoom_out();
        }
        if rotate_cw {
            self.rotate_clockwise();
        }
        if rotate_ccw {
            self.rotate_counterclockwise();
        }
        if escape {
            self.clear_selection();
        }
    }
}

impl eframe::App for PdfxiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.handle_keyboard(ctx);

        egui::TopBottomPanel::top("pdfxi_toolbar").show(ctx, |ui| self.toolbar_ui(ui));
        egui::TopBottomPanel::top("pdfxi_tabs").show(ctx, |ui| self.tabs_ui(ui));
        egui::CentralPanel::default().show(ctx, |ui| self.canvas_ui(ui));
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::Rotation;

    #[test]
    fn active_tab_status_without_document_has_fixed_message() {
        let state = AppState::new();
        assert_eq!(active_tab_status(&state), "No document open");
    }

    #[test]
    fn active_tab_status_includes_title_zoom_rotation_and_view() {
        let mut state = AppState::new();
        state.open_tab("report.pdf");
        state
            .set_zoom(Zoom::custom(1.5))
            .expect("an active tab exists");
        state
            .set_rotation(Rotation::Deg90)
            .expect("an active tab exists");
        state
            .set_view_mode(ViewMode::TwoPageContinuous)
            .expect("an active tab exists");

        let status = active_tab_status(&state);
        assert!(status.contains("report.pdf"), "{status}");
        assert!(status.contains("150%"), "{status}");
        assert!(status.contains("90 deg"), "{status}");
        assert!(status.contains("Two Page Continuous"), "{status}");
    }

    #[test]
    fn canvas_placeholder_reports_page_count_and_prompts_when_empty() {
        let empty = AppState::new();
        assert_eq!(canvas_placeholder(&empty), "Open a document to begin");

        let mut state = AppState::new();
        state.open_tab("doc");
        assert!(
            canvas_placeholder(&state).contains("Page 1 of 0"),
            "{}",
            canvas_placeholder(&state)
        );
        if let Some(tab) = state.active_tab_mut() {
            tab.set_page_count(DEMO_PAGE_COUNT);
        }
        assert!(canvas_placeholder(&state).contains("Page 1 of 3"));
    }

    #[test]
    fn open_demo_tab_registers_tab_with_placeholder_page_count() {
        let mut app = PdfxiApp::new();
        let first = app.open_demo_tab();
        let second = app.open_demo_tab();
        assert_ne!(first, second);
        assert_eq!(app.state().tab_count(), 2);
        assert_eq!(app.state().active_tab_id(), Some(second));
        assert_eq!(
            app.state().tab(second).map(|tab| tab.page_count()),
            Some(DEMO_PAGE_COUNT)
        );
        assert!(!app.status().is_empty());
    }

    #[test]
    fn zoom_in_and_out_change_active_tab_zoom() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        app.zoom_in();
        assert_eq!(
            app.state().tab(id).and_then(|tab| tab.zoom().scale()),
            Some(1.25)
        );
        app.zoom_out();
        app.zoom_out();
        assert_eq!(
            app.state().tab(id).and_then(|tab| tab.zoom().scale()),
            Some(0.75)
        );
    }

    #[test]
    fn zoom_without_active_tab_reports_status_without_panicking() {
        let mut app = PdfxiApp::new();
        app.zoom_in();
        assert_eq!(app.status(), "No active tab to zoom");
    }

    #[test]
    fn rotate_and_view_mode_commands_update_active_tab() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        app.rotate_clockwise();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg90)
        );
        app.rotate_counterclockwise();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg0)
        );
        app.set_view_mode(ViewMode::SinglePage);
        assert_eq!(
            app.state().tab(id).map(|tab| tab.view_mode()),
            Some(ViewMode::SinglePage)
        );
    }

    #[test]
    fn activate_and_close_tab_switch_and_remove_documents() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        app.activate_tab(a);
        assert_eq!(app.state().active_tab_id(), Some(a));
        app.close_active_tab();
        assert_eq!(app.state().active_tab_id(), Some(b));
        assert_eq!(app.state().tab_count(), 1);
        app.close_active_tab();
        assert!(app.state().is_empty());
        app.close_active_tab();
        assert_eq!(app.status(), "No tab to close");
    }

    #[test]
    fn tool_selection_updates_shared_state() {
        let mut app = PdfxiApp::new();
        app.select_tool(Toolset::Highlight);
        assert_eq!(app.state().active_tool(), Toolset::Highlight);
        assert!(app.status().contains("Highlight"));
    }

    #[test]
    fn demo_geometry_helpers_are_deterministic() {
        let sizes = demo_page_sizes(3);
        assert_eq!(sizes.len(), 3);
        assert!(sizes.iter().all(
            |size| size.width_pt == DEMO_PAGE_WIDTH_PT && size.height_pt == DEMO_PAGE_HEIGHT_PT
        ));

        let pages = demo_text_pages(3);
        assert_eq!(pages.len(), 3);
        assert_eq!(pages[0].page_number(), 0);
        assert_eq!(pages[2].page_number(), 2);
        assert!(!pages[0].spans().is_empty());
        assert_eq!(pages[0], demo_text_pages(3)[0], "no time or randomness");
    }

    #[test]
    fn layout_for_state_tracks_active_tab() {
        let mut state = AppState::new();
        assert!(layout_for_state(&state, CanvasSize::new(800.0, 600.0)).is_none());

        let id = state.open_tab("demo");
        if let Some(tab) = state.tab_mut(id) {
            tab.set_page_count(4);
            tab.set_view_mode(ViewMode::TwoPageContinuous);
            tab.set_zoom(Zoom::custom(2.0));
        }
        let layout = layout_for_state(&state, CanvasSize::new(800.0, 600.0))
            .expect("active tab produces a layout");
        assert_eq!(layout.page_count(), 4);
        assert_eq!(layout.mode(), ViewMode::TwoPageContinuous);
        assert!((layout.scale() - 2.0).abs() < 0.001);
    }

    #[test]
    fn screen_to_canvas_accounts_for_origin_and_scroll() {
        let origin = egui::pos2(100.0, 50.0);
        let point = screen_to_canvas(egui::pos2(150.0, 70.0), origin, 20.0, 10.0);
        assert_eq!(point, CanvasPoint::new(70.0, 30.0));
    }

    #[test]
    fn canvas_info_line_reports_page_and_selection() {
        assert_eq!(canvas_info_line(CanvasHit::Page(0), ""), "Page 1");
        assert_eq!(canvas_info_line(CanvasHit::Gap, ""), "Between pages");
        assert_eq!(canvas_info_line(CanvasHit::Outside, ""), "Outside pages");
        assert_eq!(
            canvas_info_line(CanvasHit::Page(2), "Hello"),
            "Page 3 | Selected: Hello"
        );
    }

    #[test]
    fn clearing_selection_via_keyboard_path_updates_state() {
        let mut app = PdfxiApp::new();
        app.open_demo_tab();
        // With no drag, copy text is empty; the clear command is still safe.
        assert_eq!(app.selected_text(), "");
        app.clear_selection();
        assert_eq!(app.status(), "Selection cleared");
        assert_eq!(app.selected_text(), "");
    }
}
