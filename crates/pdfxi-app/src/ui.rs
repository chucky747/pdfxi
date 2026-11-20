//! Native viewer shell built on `egui`/`eframe`.
//!
//! This module is only compiled when the `ui` feature is enabled. It layers a
//! desktop window on top of the UI-agnostic [`AppState`] model: a toolbar, a
//! document tab strip, a command bar, and the document canvas. All
//! document mutations go through [`AppState`] methods, so the state logic stays
//! independent of the window and can be unit tested without one.
//!
//! The canvas draws page rectangles laid out by [`crate::canvas::CanvasLayout`],
//! reports the page under the cursor, and shows copied text from a
//! [`crate::selection::SelectionModel`] in the status line. An in-document find
//! bar drives [`crate::find::FindState`]: matches are highlighted on the page
//! rectangles, the view scrolls to the current match, and Ctrl+C copies the
//! current selection through egui's clipboard API.
//!
//! # Raster integration
//!
//! With the `pdfium` feature enabled, an explicitly opened PDF is bound to
//! a document chosen through the native picker or a dropped file; no fixture is
//! `PdfiumRenderer`. Visible pages are
//! rasterized into [the
//! bounded raster cache](crate::raster::RasterCache), and the active raster is
//! uploaded into an `egui` texture that is painted inside the page rectangle.
//! Without `pdfium`, no native library is loaded and the canvas keeps drawing
//! the outlined page rectangles. All layout, selection, and cache logic lives
//! in window-free modules so it can be tested without this GUI layer.
//!
//! # Thumbnail navigation pane
//!
//! A left side panel lists one thumbnail tile per page, driven by the
//! window-free [`crate::ThumbnailPaneState`]. With `pdfium` the visible tiles are
//! generated through `pdfxi-render`'s LRU [`pdfxi_render::thumbnail::ThumbnailCache`]
//! and uploaded into reusable `egui` textures; otherwise they are drawn as
//! placeholders. Clicking a tile marks that page active and scrolls the canvas
//! to it, while canvas scrolling updates the active tile so the two views stay
//! in sync. Only visible rows (plus a documented prefetch margin) are requested.
//!
//! # Tabbed document interface
//!
//! The top tab strip shows one labelled tile per open document. Selectable
//! labels highlight the active tab and activate on click; each tile also has a
//! close button. `Ctrl+Tab` / `Ctrl+Shift+Tab` walk the strip with wraparound and
//! `Ctrl+W` closes the active tab. Opening the same path twice focuses the
//! existing tab. Closing a dirty document is routed through a
//! `UiClosePolicy` hook so unsaved work is not silently discarded. Each tab
//! keeps its own page, zoom, layout, rotation, and scroll offset, which the
//! shell saves on switch-away and restores on switch-back. An empty Home state
//! is shown while no document is open. All of these paths are exposed as
//! window-free `PdfxiApp` methods so they are unit tested without a GUI.
//!
//! # View rotation and page display
//!
//! The toolbar exposes clockwise/counterclockwise rotation, a rotation reset,
//! and fit page / fit width / actual size controls, each with a keyboard path
//! via [`crate::map_shortcut`]. The status line reports rotation and zoom
//! together. Rotation is per-tab and independent of a PDF page's `/Rotate`; the
//! layout composes them (rotation first, then zoom) so fit modes, hit testing,
//! overlay placement, and scroll clamping all follow what is displayed. Every
//! command goes through [`AppState`] and returns immediately - no UI action
//! performs blocking I/O.
//!

use std::collections::VecDeque;
use std::path::{Path, PathBuf};

use crate::OpenError;
use crate::design::{self, Icon, Palette, ThemeMode, TitlePalette, icon, metrics};
use crate::settings::{self, LaunchOptions, ThemePreference, WindowControlStyle};
use egui::Key;

use crate::{
    AppState, CanvasHit, CanvasLayout, CanvasPoint, CanvasRect, CanvasSize, CloseDecision,
    DEFAULT_ZOOM, DocumentTab, FindMatch, FindOptions, FindState, OpenOutcome, OwnedSpan,
    OwnedTextPage, PageSize, SelectionModel, ShortcutKey, ShortcutModifiers, TabId,
    ThumbnailPaneState, Toolset, ViewCommand, ViewMode, Zoom, app_name, map_shortcut,
};

#[cfg(feature = "pdfium")]
use crate::Rotation;
#[cfg(feature = "pdfium")]
use crate::raster::{RasterCache, RasterKey};
#[cfg(feature = "pdfium")]
use crate::uv_quad_for_rotation;
#[cfg(feature = "pdfium")]
use pdfxi_render::{
    CancellationToken, PageIndex, PdfiumRenderer, Renderer, thumbnail::ThumbnailSpec,
};
#[cfg(feature = "pdfium")]
use pdfxi_text::TextExtractor;

/// An `egui` texture plus the raster key it currently holds.
///
/// Reusing one texture handle and calling [`egui::TextureHandle::set`] on zoom
/// or rotation avoids allocating a new GPU texture every frame.
#[cfg(feature = "pdfium")]
struct LiveTexture {
    handle: egui::TextureHandle,
    scale_bits: u32,
    rotation: u16,
}

/// Amount by which each zoom-in / zoom-out step changes a custom zoom scale.
const ZOOM_STEP: f32 = 0.25;

/// Pages whose text is extracted synchronously when a document opens.
///
/// Larger documents are filled a couple of pages per frame so opening never
/// blocks the window on text extraction. Find works over the pages extracted
/// so far.
#[cfg(feature = "pdfium")]
const EAGER_TEXT_PAGES: usize = 8;

/// Height of one row in the thumbnail side panel, in UI points.
#[allow(dead_code)]
const THUMB_ROW_HEIGHT: f32 = 140.0;

/// Maximum side (width and height) of a generated thumbnail, in pixels.
#[cfg(feature = "pdfium")]
#[allow(dead_code)]
const THUMB_MAX_SIDE: u32 = 120;

/// Width of a thumbnail tile in the side panel, in UI points.
#[allow(dead_code)]
const THUMB_TILE_WIDTH: f32 = 96.0;

/// Height of a thumbnail tile in the side panel, in UI points.
#[allow(dead_code)]
const THUMB_TILE_HEIGHT: f32 = 124.0;

/// Maximum number of thumbnails generated in a single frame.
///
/// Bounding per-frame work keeps scrolling responsive even for long documents.
#[cfg(feature = "pdfium")]
#[allow(dead_code)]
const THUMBS_PER_FRAME: usize = 4;

/// Maximum page rasters rendered synchronously in one frame.
///
/// A low-zoom spread can intersect many pages; rendering them all in one frame
/// would freeze the window. The rest render on later frames.
#[cfg(feature = "pdfium")]
const RASTERS_PER_FRAME: usize = 2;

/// How the shell handles a close request for a tab with unsaved changes.
///
/// This is the UI-side implementation of the workspace's unsaved-close policy
/// hook. It is a small `Copy` value so every close path (button, `Ctrl+W`, and
/// close-others) can consult it without borrowing the whole application.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UiClosePolicy {
    /// Discard unsaved changes and close immediately.
    DiscardDirty,
    /// Refuse to close a dirty tab and show a status prompt instead.
    ///
    /// This is the default: it prevents accidental data loss. A production
    /// viewer would raise a save/discard/cancel dialog here; the headless shell
    /// reports the refusal through [`PdfxiApp::status`] so the behavior is
    /// deterministic and testable without a window.
    #[default]
    RequireClean,
}

impl UiClosePolicy {
    /// Translate this policy into a model-level [`CloseDecision`] for `tab`.
    fn decide(self, tab: &DocumentTab) -> CloseDecision {
        match self {
            Self::DiscardDirty => CloseDecision::Close,
            Self::RequireClean if tab.is_dirty() => CloseDecision::Keep,
            Self::RequireClean => CloseDecision::Close,
        }
    }
}

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
            "Page {} of {} - page rendering is not available in this shell yet",
            tab.current_page() + 1,
            tab.page_count(),
        ),
        None => "Open a document to begin".to_string(),
    }
}

/// Lay out the active tab's real pages for a viewport, or `None` with no tab.
///
/// `sizes` are the document's page sizes in points. When a size is missing
/// (a page whose geometry has not been measured yet) a US-Letter rectangle is
/// required for every page; the shell never invents page sizes or text.
pub fn layout_for_state(
    state: &AppState,
    viewport: CanvasSize,
    sizes: &[PageSize],
) -> Option<CanvasLayout> {
    let tab = state.active_tab()?;
    let page_count = tab.page_count();
    // Every page must have a real measured size; when one is missing the
    // caller surfaces an error instead of inventing a Letter page.
    if sizes.len() < page_count {
        return None;
    }
    Some(CanvasLayout::standard_rotated(
        tab.view_mode(),
        tab.zoom(),
        viewport,
        &sizes[..page_count],
        tab.rotation(),
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

/// Translate an `egui` key into the toolkit-independent [`ShortcutKey`].
///
/// Returning `None` for keys outside the view-command map lets other widgets
/// (notably the find text field) keep their normal key handling.
fn native_shortcut_key(key: Key) -> Option<ShortcutKey> {
    match key {
        Key::R => Some(ShortcutKey::R),
        Key::Num0 => Some(ShortcutKey::Zero),
        Key::Num1 => Some(ShortcutKey::One),
        Key::Num2 => Some(ShortcutKey::Two),
        Key::Equals => Some(ShortcutKey::Equals),
        Key::Minus => Some(ShortcutKey::Minus),
        _ => None,
    }
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

/// Map a find match's character range to a highlight rectangle in page-local
/// points (origin at the page's top-left, `y` increasing downward).
///
/// Character offsets are resolved against the spans of the matched page using
/// the same even interpolation as selection hit testing. Returns `None` when the
/// page has no spans or the range is empty. The rectangle covers every touched
/// span so multi-line matches are fully highlighted.
pub fn match_rect(hit: &FindMatch, pages: &[OwnedTextPage]) -> Option<CanvasRect> {
    let page = pages.iter().find(|page| page.page_number() == hit.page())?;
    let spans = page.spans();
    if spans.is_empty() || hit.start() >= hit.end() {
        return None;
    }

    let mut cursor = 0usize;
    let mut min_x = f32::INFINITY;
    let mut max_x = f32::NEG_INFINITY;
    let mut min_y = f32::INFINITY;
    let mut max_y = f32::NEG_INFINITY;

    for span in spans {
        let count = span.text.chars().count();
        let span_start = cursor;
        let span_end = cursor.saturating_add(count);
        if count > 0 && span_end > hit.start() && span_start < hit.end() {
            let from = hit.start().saturating_sub(span_start);
            let to = hit.end().min(span_end).saturating_sub(span_start);
            let char_width = if span.width > 0.0 {
                span.width / count as f32
            } else {
                0.0
            };
            let x0 = span.x + from as f32 * char_width;
            let x1 = span.x + to as f32 * char_width;
            min_x = min_x.min(x0);
            max_x = max_x.max(x1);
            min_y = min_y.min(span.y);
            max_y = max_y.max(span.y + span.height);
        }
        cursor = span_end;
    }

    if min_x.is_finite()
        && max_x.is_finite()
        && min_y.is_finite()
        && max_y.is_finite()
        && max_x > min_x
        && max_y > min_y
    {
        return Some(CanvasRect::new(min_x, min_y, max_x - min_x, max_y - min_y));
    }

    // Zero-width spans cannot form a visible box; fall back to the first span's
    // box so the match is still indicated rather than silently dropped.
    let first = spans.first()?;
    Some(CanvasRect::new(
        first.x,
        first.y,
        first.width.max(1.0),
        first.height.max(1.0),
    ))
}

/// Compute the vertical scroll offset that brings `page` into view.
///
/// The page is centred in the viewport when it is smaller than the viewport and
/// aligned to the top otherwise; the result is clamped to the layout's scroll
/// bounds so it is always finite and non-negative.
pub fn scroll_offset_for_page(layout: &CanvasLayout, page: usize) -> f32 {
    let Some(geometry) = layout.page(page) else {
        return 0.0;
    };
    let viewport_height = layout.viewport().height;
    let desired = geometry.rect.y - (viewport_height - geometry.rect.height) * 0.5;
    layout.clamp_scroll(0.0, desired).y
}

/// Determine the active page from a canvas scroll offset.
///
/// The page whose rectangle contains the vertical centre of the viewport wins;
/// if the centre falls in a gap, the page containing the viewport's top edge is
/// used, and finally the closest page above the top edge. This pure helper lets
/// the thumbnail pane follow canvas scrolling without any GUI context.
pub fn active_page_from_canvas_scroll(layout: &CanvasLayout, scroll_y: f32) -> usize {
    let viewport_height = layout.viewport().height;
    let centre = scroll_y + viewport_height.max(0.0) * 0.5;
    let top = scroll_y + 1.0;

    for page in layout.pages() {
        if centre >= page.rect.y && centre < page.rect.y + page.rect.height {
            return page.page;
        }
    }
    for page in layout.pages() {
        if top >= page.rect.y && top < page.rect.y + page.rect.height {
            return page.page;
        }
    }

    let mut best_page = 0usize;
    let mut best_top = f32::NEG_INFINITY;
    for page in layout.pages() {
        if page.rect.y <= scroll_y && page.rect.y > best_top {
            best_top = page.rect.y;
            best_page = page.page;
        }
    }
    best_page
}

/// Build the application icon shown by the OS window switcher.
fn app_icon() -> egui::IconData {
    let size = 32u32;
    let mut rgba = vec![0u8; (size * size * 4) as usize];
    for y in 0..size {
        for x in 0..size {
            let index = ((y * size + x) * 4) as usize;
            if x >= 2 && x + 2 < size && y >= 2 && y + 2 < size {
                rgba[index] = 0xc8;
                rgba[index + 1] = 0x34;
                rgba[index + 2] = 0x2b;
                rgba[index + 3] = 0xff;
                let dx = (x as i32 - 16).abs();
                let dy = (y as i32 - 16).abs();
                if (dx - dy).abs() <= 2 && dx <= 9 {
                    rgba[index] = 0xff;
                    rgba[index + 1] = 0xff;
                    rgba[index + 2] = 0xff;
                }
            }
        }
    }
    egui::IconData {
        rgba,
        width: size,
        height: size,
    }
}

/// Launch the native PDFXI viewer window.
///
/// This is the entry point used by the `pdfxi-ui` binary. It builds a
/// [`eframe::NativeOptions`] with a sensible default viewport and starts the
/// eframe event loop with a default [`PdfxiApp`]. The call returns once the
/// window has been closed; a startup or runtime failure is returned as a
/// readable message so the host can log it and show it to the user instead of
/// writing to a console the GUI subsystem does not have.
pub fn run() -> Result<(), String> {
    let options = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_title(app_name())
            .with_inner_size([1280.0, 800.0])
            .with_decorations(false)
            .with_resizable(true)
            .with_icon(std::sync::Arc::new(app_icon())),
        ..eframe::NativeOptions::default()
    };

    eframe::run_native(
        app_name(),
        options,
        Box::new(|_creation_context| {
            let arguments: Vec<String> = std::env::args().skip(1).collect();
            let launch = settings::parse_launch_arguments(&arguments);
            let mut app = PdfxiApp::new_for_window();
            app.apply_launch_options(&launch);
            app.open_startup_argument(arguments);
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| format!("PDFXI viewer exited with an error: {error}"))
}

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

/// The eframe application implementing the PDFXI viewer shell.
///
/// It owns an [`AppState`] and renders it as a toolbar, a document tab strip, a
/// command bar, and the document canvas. Every control and keyboard
/// shortcut maps to a method on this type that mutates only the underlying
/// state, keeping behavior testable without a window.
#[derive(Default)]
pub struct PdfxiApp {
    state: AppState,
    status: String,
    /// Unsaved-change policy consulted by every tab-close action.
    close_policy: UiClosePolicy,
    /// Typed errors surfaced by recent document-open attempts.
    open_errors: Vec<OpenError>,
    /// Whether the in-app "Open PDF" path dialog is visible.
    open_dialog_open: bool,
    /// Dismissible banner for the most recent open failure.
    open_error_banner: Option<String>,
    /// Real page sizes in points, indexed by page of the active document.
    page_sizes: Vec<PageSize>,
    /// Real selectable text for each page extracted so far.
    text_pages: Vec<OwnedTextPage>,
    /// Pages whose text is still pending, extracted a few per frame.
    pending_text_pages: VecDeque<usize>,
    /// Active shell appearance.
    theme: ThemeMode,
    /// Theme preference that resolves to [`Self::theme`] each frame.
    theme_preference: ThemePreference,
    /// Which window controls the custom title bar draws.
    window_controls: WindowControlStyle,
    /// Settings as persisted on disk (launch flags never modify this).
    settings: settings::Settings,
    /// Where settings are saved; `None` for viewers that never touch disk.
    settings_path: Option<PathBuf>,
    /// Path currently bound to the PDFium renderer.
    #[cfg(feature = "pdfium")]
    renderer_path: Option<PathBuf>,
    /// Theme last installed on the egui context.
    applied_theme: Option<ThemeMode>,
    scroll_x: f32,
    scroll_y: f32,
    /// Active text selection over real page text; empty until text is wired.
    selection: SelectionModel,
    find: FindState,
    /// Editable query mirrored into `find` on change.
    find_query: String,
    /// Checkbox state for case-sensitive matching.
    find_case: bool,
    /// Checkbox state for whole-word matching.
    find_word: bool,
    /// Whether the find bar is visible.
    find_open: bool,
    /// Set when navigation should scroll to the current match next frame.
    pending_scroll: bool,
    /// Set when a thumbnail click should scroll the canvas next frame.
    pending_page_scroll: Option<usize>,
    /// Window-free thumbnail navigation pane model.
    thumbs: ThumbnailPaneState,
    /// When true, the next pane frame scrolls its active row into view.
    thumb_follow_active: bool,
    /// GPU textures for generated thumbnails, keyed by page.
    #[cfg(feature = "pdfium")]
    thumb_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    /// Bounded cache of real page rasters from the open document.
    #[cfg(feature = "pdfium")]
    raster_cache: RasterCache,
    /// The native renderer; `None` when the fixture could not be opened.
    #[cfg(feature = "pdfium")]
    pdfium_renderer: Option<PdfiumRenderer>,
    /// Cancellation token for the open document's raster work.
    ///
    /// Held across frames so a long render can be cancelled when the user
    /// switches documents instead of being scoped to one frame.
    #[cfg(feature = "pdfium")]
    raster_cancel: CancellationToken,
    /// One GPU texture per laid-out page, refreshed on zoom/rotation changes.
    #[cfg(feature = "pdfium")]
    textures: std::collections::HashMap<usize, LiveTexture>,
}

impl PdfxiApp {
    /// Create a viewer with no open documents.
    pub fn new() -> Self {
        Self::default()
    }

    /// Create an empty viewer window with persisted preferences applied.
    ///
    /// No document is opened automatically: with no PDF there is nothing
    /// honest to show, so the shell starts on its Home state. Tests use
    /// [`Self::new`] and drive tabs explicitly.
    pub fn new_for_window() -> Self {
        let mut app = Self::new();
        app.load_persisted_settings();
        app
    }

    /// Load the per-user settings file, when the platform exposes one.
    ///
    /// A missing, unreadable, or corrupt file leaves the defaults in place, so
    /// startup never blocks or fails on settings.
    fn load_persisted_settings(&mut self) {
        let Some(path) = settings::default_path() else {
            return;
        };
        self.settings = settings::load(&path);
        self.theme_preference = self.settings.theme;
        self.window_controls = self.settings.window_controls;
        self.settings_path = Some(path);
    }

    /// Apply one run's launch flags on top of the saved settings.
    ///
    /// Only the effective fields change; [`Self::settings`] keeps the on-disk
    /// values so a flag is never written back by an unrelated save.
    fn apply_launch_options(&mut self, options: &LaunchOptions) {
        let effective = self.settings.with_overrides(options);
        self.theme_preference = effective.theme;
        self.window_controls = effective.window_controls;
    }

    /// The theme preference currently in effect.
    #[must_use]
    pub fn theme_preference(&self) -> ThemePreference {
        self.theme_preference
    }

    /// The window control style currently in effect.
    #[must_use]
    pub fn window_controls(&self) -> WindowControlStyle {
        self.window_controls
    }

    /// Select a theme preference and persist it when possible.
    pub fn set_theme_preference(&mut self, preference: ThemePreference) {
        self.theme_preference = preference;
        self.settings.theme = preference;
        self.persist_settings();
    }

    /// Select a window control style and persist it when possible.
    pub fn set_window_controls(&mut self, style: WindowControlStyle) {
        self.window_controls = style;
        self.settings.window_controls = style;
        self.persist_settings();
    }

    /// Save the persisted settings; a failure is reported in the status line.
    fn persist_settings(&mut self) {
        let Some(path) = self.settings_path.clone() else {
            return;
        };
        if let Err(error) = settings::save(&path, &self.settings) {
            self.status = format!("Could not save settings: {error}");
        }
    }

    /// Borrow the underlying UI-agnostic application state.
    pub fn state(&self) -> &AppState {
        &self.state
    }

    /// The most recent user-facing status message, or an empty string.
    pub fn status(&self) -> &str {
        &self.status
    }

    /// Initialize per-tab shell state for a newly opened document.
    ///
    /// The page count is filled in by the renderer once the document is open;
    /// until then it stays zero so the shell never invents content. Find,
    /// selection, and thumbnails start empty and are populated only from real
    /// document data.
    fn finish_new_tab(&mut self, id: TabId) {
        if let Some(tab) = self.state.tab_mut(id) {
            tab.set_page_count(0);
        }
        // A newly focused document starts with one pane bound to it. Any prior
        self.find.set_pages(Vec::new());
        self.page_sizes.clear();
        self.text_pages.clear();
        self.pending_text_pages.clear();
        self.selection.clear();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.pending_scroll = false;
        self.pending_page_scroll = None;
        self.thumbs.reset(0);
        self.thumb_follow_active = true;
        #[cfg(feature = "pdfium")]
        self.thumb_textures.clear();
    }

    /// Close the active tab, if any, using the configured [`UiClosePolicy`].
    ///
    /// A vetoed close leaves the workspace untouched and reports why through
    /// [`Self::status`]. With no active tab the status is `"No tab to close"`,
    /// preserving the original shell behavior.
    pub fn close_active_tab(&mut self) {
        if let Some(id) = self.state.active_tab_id() {
            let _ = self.close_tab_by_id(id);
        } else {
            self.status = "No tab to close".to_string();
        }
    }

    /// Close a specific tab, consulting the unsaved-change policy hook.
    ///
    /// Returns `true` when the tab was actually removed. Returns `false` when
    /// the tab was kept by the policy, did not exist, or no tab was active; in
    /// every case [`Self::status`] describes the outcome.
    pub fn close_tab_by_id(&mut self, id: TabId) -> bool {
        let policy = self.close_policy;
        let title = self.state.tab_title(id).unwrap_or_default();
        let outcome = match self
            .state
            .close_tab_with_policy(id, |tab| policy.decide(tab))
        {
            Ok(outcome) => outcome,
            Err(error) => {
                self.status = error.to_string();
                return false;
            }
        };
        match outcome {
            crate::CloseOutcome::Closed { .. } => {
                self.status = format!("Closed tab {id}");
                self.restore_scroll_from_active();
                true
            }
            crate::CloseOutcome::Cancelled { .. } => {
                self.status = format!("Unsaved changes in {title}; save or discard before closing");
                false
            }
        }
    }

    /// Make the tab with the given id active, if it exists.
    ///
    /// The outgoing tab's scroll position is saved on its own state before the
    /// switch and the incoming tab's position is restored afterwards.
    pub fn activate_tab(&mut self, id: TabId) {
        self.save_scroll_to_active();
        match self.state.activate_tab(id) {
            Ok(()) => {
                self.on_active_tab_changed();
                self.status = format!("Activated tab {id}");
            }
            Err(error) => {
                self.restore_scroll_from_active();
                self.status = error.to_string();
            }
        }
    }

    /// Activate the next tab with wraparound (the `Ctrl+Tab` action).
    ///
    /// Returns `false` with a status message when no tab is active.
    pub fn activate_next_tab(&mut self) -> bool {
        self.save_scroll_to_active();
        match self.state.activate_next() {
            Ok(id) => {
                self.on_active_tab_changed();
                self.status = format!("Activated tab {id}");
                true
            }
            Err(error) => {
                self.restore_scroll_from_active();
                self.status = error.to_string();
                false
            }
        }
    }

    /// Activate the previous tab with wraparound (the `Ctrl+Shift+Tab` action).
    ///
    /// Returns `false` with a status message when no tab is active.
    pub fn activate_previous_tab(&mut self) -> bool {
        self.save_scroll_to_active();
        match self.state.activate_previous() {
            Ok(id) => {
                self.on_active_tab_changed();
                self.status = format!("Activated tab {id}");
                true
            }
            Err(error) => {
                self.restore_scroll_from_active();
                self.status = error.to_string();
                false
            }
        }
    }

    /// Open a document path, focusing an existing tab when the path is already
    /// open (the model's duplicate-path rule).
    ///
    /// A newly opened document starts with empty find, selection, and thumbnail state;
    /// a reused one simply restores its saved view and becomes active.
    pub fn open_document(&mut self, path: &Path) -> OpenOutcome {
        self.save_scroll_to_active();
        let outcome = self.state.open_document(path);
        match outcome {
            OpenOutcome::Opened { id } => {
                self.finish_new_tab(id);
                self.refresh_renderer_for_active_tab();
                self.status = format!("Opened {}", path.display());
            }
            OpenOutcome::Focused { id } => {
                self.on_active_tab_changed();
                self.status = format!("Focused existing tab {id}");
            }
        }
        outcome
    }

    /// Open a user-supplied path string, from the toolbar dialog or a drop.
    ///
    /// Rejects empty input, surrounding quotes, control characters and
    /// non-PDF extensions before touching state, so a hostile clipboard or drop
    /// payload cannot reach the document loader. Returns `true` when the tab
    /// opened or an existing tab for the same path was focused.
    pub fn open_path_text(&mut self, raw: &str) -> bool {
        let trimmed = raw.trim().trim_matches('"');
        if trimmed.is_empty() {
            self.open_error_banner = Some("Choose a PDF file to open.".to_owned());
            return false;
        }
        if trimmed.chars().any(char::is_control) {
            self.open_error_banner = Some("That path contains invalid characters.".to_owned());
            return false;
        }
        let path = Path::new(trimmed);
        if !Self::is_pdf_path(path) {
            self.open_error_banner = Some(format!("{} is not a PDF file.", path.display()));
            return false;
        }
        if !path.is_file() {
            self.open_error_banner = Some(format!("{} could not be found.", path.display()));
            return false;
        }
        self.open_document(path);
        true
    }

    /// Open the first path passed on the command line, if any.
    ///
    /// The desktop shortcut and "Open with" both launch the viewer with a path
    /// as the first argument. Flags are skipped, and a bad path reports a status
    /// instead of failing startup. Returns `true` when a tab was opened.
    pub fn open_startup_argument(&mut self, args: impl IntoIterator<Item = String>) -> bool {
        for argument in args {
            if argument.starts_with('-') {
                continue;
            }
            return self.open_path_text(&argument);
        }
        false
    }

    /// Whether a path carries a `.pdf` extension (case-insensitive).
    #[must_use]
    pub fn is_pdf_path(path: &Path) -> bool {
        path.extension()
            .and_then(|extension| extension.to_str())
            .is_some_and(|extension| extension.eq_ignore_ascii_case("pdf"))
    }

    /// Open every PDF in a dropped-file batch; returns how many were opened.
    ///
    /// Non-PDF drops are ignored; a batch with no PDFs reports it instead of
    /// silently doing nothing.
    pub fn open_dropped(&mut self, paths: &[PathBuf]) -> usize {
        let mut opened = 0usize;
        let mut pdfs = 0usize;
        for path in paths {
            if !Self::is_pdf_path(path) {
                continue;
            }
            pdfs += 1;
            let text = path.to_string_lossy();
            if self.open_path_text(text.as_ref()) {
                opened += 1;
            }
        }
        if opened > 0 {
            self.status = format!("Opened {opened} dropped file(s)");
        } else if pdfs == 0 && !paths.is_empty() {
            self.status = "Dropped files are not PDFs".to_owned();
        }
        opened
    }

    /// Draw the in-app "Open PDF" path dialog.
    ///
    /// The shell links no native file picker, so this is intentionally a typed
    /// path plus drag-and-drop support. It mutates only in-memory state.
    fn open_dialog_ui(&mut self, _ctx: &egui::Context) {
        if !self.open_dialog_open {
            return;
        }
        // Consume the request first so a failed or cancelled picker cannot
        // reopen the dialog on the next frame.
        self.open_dialog_open = false;
        // The native picker is modal; it runs between frames, never inside a
        // paint callback, so the window cannot deadlock itself. Multi-select
        // opens each chosen file in its own tab.
        let chosen = rfd::FileDialog::new()
            .set_title("Open PDF")
            .add_filter("PDF documents", &["pdf"])
            .pick_files();
        if let Some(paths) = chosen {
            for path in paths {
                self.open_document(&path);
            }
        }
    }

    /// Close every tab except the active one, consulting the configured
    /// [`UiClosePolicy`] for each candidate tab.
    ///
    /// This applies the same unsaved-change rule as the tab-strip close button
    /// and `Ctrl+W`: with [`UiClosePolicy::RequireClean`] a dirty tab is kept
    /// open; with [`UiClosePolicy::DiscardDirty`] it is closed. Returns a typed
    /// report of the ids that were closed and the ids the policy kept, so a
    /// caller can see exactly what happened without parsing the status string.
    pub fn close_other_tabs(&mut self) -> crate::CloseOthersOutcome {
        let policy = self.close_policy;
        let outcome = self
            .state
            .close_other_tabs_with_policy(|tab| policy.decide(tab));
        let kept = outcome.kept.len();
        self.status = if kept == 0 {
            format!("Closed {} other tab(s)", outcome.closed.len())
        } else {
            format!(
                "Closed {} other tab(s); kept {} with unsaved changes",
                outcome.closed.len(),
                kept
            )
        };
        outcome
    }

    /// Move a tab to a new position in the strip. Returns `false` on a bad id
    /// or index, with the reason in [`Self::status`].
    pub fn reorder_tab(&mut self, id: TabId, new_index: usize) -> bool {
        match self.state.reorder_tab(id, new_index) {
            Ok(()) => {
                self.status = format!("Moved tab {id} to position {}", new_index + 1);
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// A headless snapshot of the tab strip: `(id, label, is_active)` triples in
    /// insertion order.
    ///
    /// The real `egui` tab strip renders from this same data, so the labels,
    /// order, and active highlight can be asserted without opening a window.
    pub fn tab_strip_snapshot(&self) -> Vec<(TabId, String, bool)> {
        self.state
            .tabs()
            .map(|tab| {
                let id = tab.id();
                (id, tab.title(), self.state.active_tab_id() == Some(id))
            })
            .collect()
    }

    /// Select the unsaved-change policy used by tab-close actions.
    pub fn set_close_policy(&mut self, policy: UiClosePolicy) {
        self.close_policy = policy;
    }

    /// The unsaved-change policy currently used by tab-close actions.
    pub fn close_policy(&self) -> UiClosePolicy {
        self.close_policy
    }

    /// The live canvas scroll offset of the active view.
    pub fn scroll_offset(&self) -> CanvasPoint {
        CanvasPoint::new(self.scroll_x, self.scroll_y)
    }

    /// Persist the live scroll offset onto the active tab before a switch.
    fn save_scroll_to_active(&mut self) {
        if let Some(tab) = self.state.active_tab_mut() {
            tab.set_scroll_offset(CanvasPoint::new(self.scroll_x, self.scroll_y));
        }
    }

    /// Load the active tab's saved scroll offset into the live canvas view.
    fn restore_scroll_from_active(&mut self) {
        if let Some(tab) = self.state.active_tab() {
            let offset = tab.scroll_offset();
            self.scroll_x = offset.x;
            self.scroll_y = offset.y;
        } else {
            self.scroll_x = 0.0;
            self.scroll_y = 0.0;
        }
    }

    /// Restore per-tab view state and drop navigation requests aimed at the
    /// tab that was just left.
    fn on_active_tab_changed(&mut self) {
        self.restore_scroll_from_active();
        self.pending_scroll = false;
        self.pending_page_scroll = None;
        self.refresh_renderer_for_active_tab();
    }

    /// Bind the PDFium renderer to the active tab's document.
    ///
    /// Rasters and GPU textures are keyed by page, scale and rotation only, so
    /// they are invalidated whenever the document changes. Documents without text have no
    /// path: they use the built-in design preview and release the renderer.
    #[cfg(feature = "pdfium")]
    fn refresh_renderer_for_active_tab(&mut self) {
        let path = self
            .state
            .active_tab()
            .and_then(|tab| tab.path())
            .map(Path::to_path_buf);
        if path == self.renderer_path {
            return;
        }
        self.renderer_path = path.clone();
        self.raster_cancel.cancel();
        self.raster_cancel = CancellationToken::new();
        self.raster_cache.invalidate_all();
        self.textures.clear();
        self.pdfium_renderer = None;
        let Some(path) = path else {
            return;
        };
        match PdfiumRenderer::from_file(&path) {
            Ok(renderer) => {
                let count = Renderer::page_count(&renderer).unwrap_or(0);
                self.page_sizes.clear();
                for page in 0..count {
                    match renderer.page_size(PageIndex::new(page)) {
                        Ok(size) => self
                            .page_sizes
                            .push(PageSize::new(size.width_pt, size.height_pt)),
                        Err(error) => {
                            self.open_error_banner = Some(format!(
                                "Could not read the size of page {}: {error}",
                                page + 1
                            ));
                            break;
                        }
                    }
                }
                self.pdfium_renderer = Some(renderer);
                if let Some(tab) = self.state.active_tab_mut() {
                    tab.set_page_count(count);
                }
                self.text_pages.clear();
                self.pending_text_pages.clear();
                // Bound the work done while the document opens: a small
                // document is fully searchable immediately, a large one fills
                // its text a couple of pages per frame.
                let eager = count.min(EAGER_TEXT_PAGES);
                for page in 0..eager {
                    if let Some(text) = self.extract_text_page(page) {
                        self.text_pages.push(text);
                    }
                }
                for page in eager..count {
                    self.pending_text_pages.push_back(page);
                }
                self.sync_find_pages();
                self.thumbs.reset(count);
                self.selection.clear();
                self.thumb_textures.clear();
            }
            Err(error) => {
                // `pdfium-render` surfaces a password failure only as text, so
                // this is a string heuristic rather than a typed error.
                let message = if error.to_string().to_ascii_lowercase().contains("password") {
                    "This PDF is password-protected. Password-protected PDFs are not supported yet."
                        .to_owned()
                } else {
                    format!("Could not open this PDF: {error}")
                };
                self.open_error_banner = Some(message.clone());
                self.status = message;
            }
        }
    }

    /// No-op without the `pdfium` feature.
    #[cfg(not(feature = "pdfium"))]
    fn refresh_renderer_for_active_tab(&mut self) {}

    /// Drop per-document raster state and cancel outstanding work.
    ///
    /// Rasters are keyed by page index, scale and rotation, not by document, so
    /// the cache must be cleared whenever the active document changes or a tab
    /// closes; otherwise the next document could display another document's
    /// page pixels. The cancellation token is cancelled first so any render in
    /// flight for the old document stops.
    #[cfg(feature = "pdfium")]
    #[allow(dead_code)]
    fn reset_raster_state(&mut self) {
        self.raster_cancel.cancel();
        self.raster_cancel = CancellationToken::new();
        self.raster_cache.invalidate_all();
        self.textures.clear();
    }

    /// No-op without the `pdfium` feature, which has no raster state.
    #[cfg(not(feature = "pdfium"))]
    #[allow(dead_code)]
    fn reset_raster_state(&mut self) {}

    /// Increase the active tab's custom zoom by one `ZOOM_STEP`.
    pub fn zoom_in(&mut self) {
        self.adjust_zoom(ZOOM_STEP, "Zoom in");
    }

    /// Decrease the active tab's custom zoom by one `ZOOM_STEP`.
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

    /// Reset the active tab's view rotation to zero.
    pub fn reset_rotation(&mut self) {
        match self.state.reset_rotation() {
            Ok(rotation) => {
                self.status = format!("Rotation reset to {} degrees", rotation.degrees())
            }
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Fit the whole (rotated) page in the viewport.
    pub fn fit_page(&mut self) {
        match self.state.fit_page() {
            Ok(()) => self.status = format!("Page display: {}", Zoom::FitPage),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Fit the width of the (rotated) page to the viewport.
    pub fn fit_width(&mut self) {
        match self.state.fit_width() {
            Ok(()) => self.status = format!("Page display: {}", Zoom::FitWidth),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Show the active page at 100% actual size.
    pub fn actual_size(&mut self) {
        match self.state.actual_size() {
            Ok(()) => self.status = format!("Page display: {}", Zoom::custom(DEFAULT_ZOOM)),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Dispatch a typed [`ViewCommand`] to the matching action.
    ///
    /// Both the toolbar and keyboard paths funnel through this method, so every
    /// command has one implementation and status message. Rotation and display
    /// commands are window-free and never block on I/O.
    pub fn apply_view_command(&mut self, command: ViewCommand) {
        match command {
            ViewCommand::RotateClockwise => self.rotate_clockwise(),
            ViewCommand::RotateCounterclockwise => self.rotate_counterclockwise(),
            ViewCommand::ResetRotation => self.reset_rotation(),
            ViewCommand::FitPage => self.fit_page(),
            ViewCommand::FitWidth => self.fit_width(),
            ViewCommand::ActualSize => self.actual_size(),
            ViewCommand::ZoomIn => self.zoom_in(),
            ViewCommand::ZoomOut => self.zoom_out(),
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

    /// Copy text of the current selection.
    ///
    /// Real page text is not wired into the shell yet, so the selection model
    /// is empty and this returns an empty string instead of invented content.
    pub fn selected_text(&self) -> String {
        self.selection.selected_text(&self.text_pages)
    }

    /// Show the find bar (bound to Ctrl+F).
    pub fn open_find(&mut self) {
        self.find_open = true;
        self.status = "Find".to_string();
    }

    /// Hide the find bar (bound to Escape while it is open).
    pub fn close_find(&mut self) {
        self.find_open = false;
    }

    /// Whether the find bar is currently visible.
    pub fn is_find_open(&self) -> bool {
        self.find_open
    }

    /// Borrow the find state for introspection.
    pub fn find_state(&self) -> &FindState {
        &self.find
    }

    /// Set the find query from the UI and recompute matches.
    pub fn set_find_query(&mut self, query: &str) {
        self.find_query = query.to_string();
        self.find.set_query(query);
    }

    /// Apply the checkbox options to the find model and recompute.
    pub fn set_find_options(&mut self, case_sensitive: bool, whole_word: bool) {
        self.find_case = case_sensitive;
        self.find_word = whole_word;
        self.find
            .set_options(FindOptions::new(case_sensitive).with_whole_word(whole_word));
    }

    /// Move to the next match and request a scroll on the next canvas frame.
    pub fn find_next(&mut self) {
        self.find.next_match();
        self.pending_scroll = true;
        self.status = self.find.match_summary();
    }

    /// Move to the previous match and request a scroll on the next canvas frame.
    pub fn find_previous(&mut self) {
        self.find.previous_match();
        self.pending_scroll = true;
        self.status = self.find.match_summary();
    }

    /// Change the active tab's view mode.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        match self.state.set_view_mode(mode) {
            Ok(()) => self.status = format!("View mode: {}", mode.label()),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Borrow the window-free thumbnail pane state for introspection.
    pub fn thumbnail_state(&self) -> &ThumbnailPaneState {
        &self.thumbs
    }

    /// Select a thumbnail: mark its page active and scroll the canvas to it.
    ///
    /// This is the click handler for the thumbnail side panel. It returns `true`
    /// when a valid page was selected and `false` for an out-of-range click.
    pub fn select_thumbnail_page(&mut self, page: usize) -> bool {
        if self.thumbs.select_page(page).is_none() {
            return false;
        }
        if let Some(tab) = self.state.active_tab_mut() {
            tab.set_current_page(page);
        }
        self.pending_page_scroll = Some(page);
        self.thumb_follow_active = true;
        self.status = format!("Page {} of {}", page + 1, self.thumbs.page_count());
        true
    }

    /// Update the active thumbnail page from the current canvas scroll offset.
    ///
    /// The canvas calls this every frame; it is exposed so the follow behavior
    /// is testable without a window. Returns `true` when the active page changed.
    pub fn sync_active_page_from_canvas(&mut self, layout: &CanvasLayout) -> bool {
        let active = active_page_from_canvas_scroll(layout, self.scroll_y);
        if self.thumbs.set_active_page(active) {
            self.thumb_follow_active = true;
            // Page position is per-tab view state, so scrolling a document and
            // then switching away and back returns to the same page.
            if let Some(tab) = self.state.active_tab_mut() {
                tab.set_current_page(active);
            }
            true
        } else {
            false
        }
    }

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

    /// Render the left thumbnail navigation side panel.
    ///
    /// With `pdfium`, only visible rows (plus the pane's documented prefetch
    /// margin) are enqueued, and at most [`THUMBS_PER_FRAME`] are generated per
    /// frame so scrolling never blocks on rasterization. Without it, every tile
    /// is drawn as a placeholder. Clicking a tile calls
    /// [`Self::select_thumbnail_page`]; scrolling updates the pane's visible
    /// range so the next frame requests the right thumbnails.
    #[allow(dead_code)]
    fn thumbnails_ui(&mut self, ui: &mut egui::Ui) {
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        if self.thumbs.page_count() != page_count {
            self.thumbs.reset(page_count);
            #[cfg(feature = "pdfium")]
            self.thumb_textures.clear();
        }
        self.thumbs.set_row_height(THUMB_ROW_HEIGHT);
        self.thumbs
            .set_viewport_height(ui.available_height().max(1.0));

        #[cfg(feature = "pdfium")]
        {
            if let Some(renderer) = self.pdfium_renderer.as_ref() {
                let spec = ThumbnailSpec::new(THUMB_MAX_SIDE, THUMB_MAX_SIDE);
                let _ = self.thumbs.enqueue_visible(&spec);
                let token = CancellationToken::new();
                for _ in 0..THUMBS_PER_FRAME {
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
        let tile_size = egui::vec2(THUMB_TILE_WIDTH, THUMB_TILE_HEIGHT);
        #[cfg(feature = "pdfium")]
        let spec = ThumbnailSpec::new(THUMB_MAX_SIDE, THUMB_MAX_SIDE);

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
            self.select_thumbnail_page(page);
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

    /// Rasterize the pages visible in `layout` into the cache (PDFium build only).
    ///
    /// A fresh [`CancellationToken`] is scoped to this frame; the renderer
    /// checks it between steps so a closed document can stop outstanding work.
    /// Each raster is keyed by that page's effective rotation, matching the
    /// rotation used when painting the cached texture below.
    #[cfg(feature = "pdfium")]
    fn ensure_rasters(&mut self, layout: &CanvasLayout, pixel_ratio: f32) {
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
    fn raster_texture(
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

    /// Render the in-document find bar.
    ///
    /// The query field mirrors into [`FindState`] on change; the two toggles
    /// rebuild the search with new options; Next/Previous navigate with
    /// wrap-around and request a scroll to the current match.
    fn find_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.label("Find:");
            let response = ui.text_edit_singleline(&mut self.find_query);
            if response.changed() {
                self.find.set_query(self.find_query.as_str());
            }
            if ui.checkbox(&mut self.find_case, "Match case").changed() {
                self.set_find_options(self.find_case, self.find_word);
            }
            if ui.checkbox(&mut self.find_word, "Whole word").changed() {
                self.set_find_options(self.find_case, self.find_word);
            }
            if ui.button("Previous").clicked() {
                self.find_previous();
            }
            if ui.button("Next").clicked() {
                self.find_next();
            }
            ui.label(self.find.match_summary());
            if ui.button("Close").clicked() {
                self.close_find();
            }
        });
    }

    /// Render the document tab strip.
    ///
    /// Labels, order, and the active highlight come from the window-free
    /// [`Self::tab_strip_snapshot`]. Each tab also gets a small close button;
    /// close requests are deferred until after the `egui` closure so the
    /// workspace is never mutated while the UI borrows it. With no open tab the
    /// strip shows a fixed placeholder.
    #[allow(dead_code)]
    fn tabs_ui(&mut self, ui: &mut egui::Ui) {
        let snapshot = self.tab_strip_snapshot();
        if snapshot.is_empty() {
            ui.label("No documents open");
            return;
        }
        let mut activate = None;
        let mut close = None;
        ui.horizontal_wrapped(|ui| {
            for (id, label, is_active) in snapshot {
                if ui.selectable_label(is_active, label).clicked() {
                    activate = Some(id);
                }
                if ui.small_button("-").clicked() {
                    close = Some(id);
                }
            }
        });
        if let Some(id) = activate {
            self.activate_tab(id);
        }
        if let Some(id) = close {
            self.close_tab_by_id(id);
        }
    }

    /// Render the document canvas, or the empty Home state with no document.
    fn canvas_ui(&mut self, ui: &mut egui::Ui) {
        let palette = Palette::for_mode(self.theme);
        let canvas_rect = ui.max_rect();
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
        if self.state.active_tab().is_some() {
            self.floating_page_bar_ui(ui, canvas_rect, palette);
        }
    }

    /// Apply keyboard shortcuts for the main find, copy, and view commands.
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let (open, next_tab, prev_tab, close, view_command, escape, open_find, next, prev, copy) =
            ctx.input(|input| {
                let ctrl = input.modifiers.ctrl;
                let shift = input.modifiers.shift;
                // Rotation, fit modes, and zoom are resolved through the shared
                // shortcut map, so the toolbar and keyboard paths cannot drift.
                let view_command = [
                    Key::R,
                    Key::Num0,
                    Key::Num1,
                    Key::Num2,
                    Key::Equals,
                    Key::Minus,
                ]
                .into_iter()
                .find(|key| input.key_pressed(*key))
                .and_then(native_shortcut_key)
                .and_then(|key| {
                    map_shortcut(
                        key,
                        ShortcutModifiers {
                            ctrl,
                            shift,
                            alt: input.modifiers.alt,
                        },
                    )
                });
                // Pane commands use Ctrl+Shift and are resolved through the
                // shared map so the toolbar and keyboard paths cannot drift.
                (
                    ctrl && input.key_pressed(Key::N),
                    ctrl && !shift && input.key_pressed(Key::Tab),
                    ctrl && shift && input.key_pressed(Key::Tab),
                    ctrl && !shift && input.key_pressed(Key::W),
                    view_command,
                    input.key_pressed(Key::Escape),
                    ctrl && input.key_pressed(Key::F),
                    !shift && input.key_pressed(Key::F3),
                    shift && input.key_pressed(Key::F3),
                    ctrl && input.key_pressed(Key::C),
                )
            });

        if open {
            self.open_dialog_open = true;
        }
        if next_tab {
            self.activate_next_tab();
        }
        if prev_tab {
            self.activate_previous_tab();
        }
        if close {
            self.close_active_tab();
        }
        if let Some(command) = view_command {
            self.apply_view_command(command);
        }
        if open_find {
            self.open_find();
        }
        if next {
            self.find_next();
        }
        if prev {
            self.find_previous();
        }
        if copy {
            let text = self.selected_text();
            if !text.is_empty() {
                ctx.copy_text(text);
                self.status = "Copied selection to clipboard".to_string();
            }
        }
        if escape {
            if self.find_open {
                self.close_find();
            } else {
                self.clear_selection();
            }
        }
    }
}

impl PdfxiApp {
    /// Record a typed document-open error for display in the shell.
    pub fn record_open_error(&mut self, error: OpenError) {
        self.open_errors.push(error);
    }

    /// Borrow the typed open errors recorded so far.
    /// Dismiss the open-error banner.
    pub fn dismiss_open_error(&mut self) {
        self.open_error_banner = None;
    }

    /// The message shown in the dismissible open-error banner, if any.
    #[must_use]
    pub fn open_error_banner(&self) -> Option<&str> {
        self.open_error_banner.as_deref()
    }

    /// Recent document-open failures, with typed errors and paths.
    pub fn open_errors(&self) -> &[OpenError] {
        &self.open_errors
    }

    /// Forget all recorded open errors.
    pub fn clear_open_errors(&mut self) {
        self.open_errors.clear();
    }

    fn open_errors_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Recent document-open failures:");
        let messages: Vec<String> = self.open_errors.iter().map(ToString::to_string).collect();
        for message in messages {
            ui.label(message);
        }
        if ui.button("Clear").clicked() {
            self.clear_open_errors();
        }
    }
}
impl eframe::App for PdfxiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_resize_handles(ctx);
        self.handle_keyboard(ctx);
        self.pump_text_extraction();

        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.open_dropped(&dropped);
        }
        self.open_dialog_ui(ctx);
        self.shell_ui(ctx);
        if !self.open_errors.is_empty() {
            let mut open = true;
            egui::Window::new("Open errors")
                .open(&mut open)
                .show(ctx, |ui| self.open_errors_ui(ui));
            if !open {
                self.clear_open_errors();
            }
        }
    }
}

// ---------------------------------------------------------------------------
// ---------------------------------------------------------------------------
// Product shell: Acrobat-style chrome around the existing viewer surface.
// ---------------------------------------------------------------------------

/// Panel background frame with horizontal padding.
fn shell_panel_frame(palette: Palette, horizontal: i8) -> egui::Frame {
    egui::Frame::new()
        .fill(palette.panel)
        .inner_margin(egui::Margin {
            left: horizontal,
            right: horizontal,
            top: 0,
            bottom: 0,
        })
}

/// A borderless icon button used across the shell.
fn shell_icon_button(
    ui: &mut egui::Ui,
    palette: Palette,
    glyph: Icon,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 7.0, palette.surface);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.text_muted
    };
    icon(ui.painter(), rect.shrink(size * 0.27), glyph, color);
    response
}

/// A macOS-style traffic light in the custom title bar.
///
/// The three lights are the window controls: red closes, amber minimizes and
/// green toggles maximize. The glyph appears on hover so the bar stays quiet.
fn traffic_light(
    ui: &mut egui::Ui,
    fill: egui::Color32,
    glyph: Icon,
    hover_text: &str,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(20.0, 20.0), egui::Sense::click());
    let center = rect.center();
    let lit = if response.hovered() {
        fill.gamma_multiply(1.08)
    } else {
        fill
    };
    ui.painter().circle_filled(center, 6.0, lit);
    if response.hovered() {
        icon(
            ui.painter(),
            egui::Rect::from_center_size(center, egui::Vec2::splat(7.0)),
            glyph,
            egui::Color32::from_black_alpha(160),
        );
    }
    response.on_hover_text(hover_text)
}

/// Width of one Windows caption button in the title bar, in UI points.
pub const CAPTION_BUTTON_WIDTH: f32 = 46.0;

/// A window-control action drawn in the custom title bar.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CaptionAction {
    /// Minimize the window.
    Minimize,
    /// Toggle between maximized and restored.
    ToggleMaximize,
    /// Close the window.
    Close,
}

/// The edge of the title bar that holds the caption buttons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CaptionSide {
    /// Left edge (macOS traffic lights).
    Left,
    /// Right edge (Windows caption buttons).
    Right,
}

/// Ordered caption buttons for a control style, and the side they occupy.
///
/// The array is ordered left to right exactly as it is drawn.
#[must_use]
pub fn caption_layout(style: WindowControlStyle) -> (CaptionSide, [CaptionAction; 3]) {
    match style {
        WindowControlStyle::Windows => (
            CaptionSide::Right,
            [
                CaptionAction::Minimize,
                CaptionAction::ToggleMaximize,
                CaptionAction::Close,
            ],
        ),
        WindowControlStyle::MacOs => (
            CaptionSide::Left,
            [
                CaptionAction::Close,
                CaptionAction::Minimize,
                CaptionAction::ToggleMaximize,
            ],
        ),
    }
}

/// The viewport command a caption button sends.
#[must_use]
pub fn caption_command(action: CaptionAction, maximized: bool) -> egui::ViewportCommand {
    match action {
        CaptionAction::Minimize => egui::ViewportCommand::Minimized(true),
        CaptionAction::ToggleMaximize => egui::ViewportCommand::Maximized(!maximized),
        CaptionAction::Close => egui::ViewportCommand::Close,
    }
}

/// Draw a native-weight Windows caption glyph.
///
/// Glyphs are 10 pt wide with a one-physical-pixel stroke, snapped to half
/// pixels so they stay crisp hairlines at any scale. The general icon set is
/// deliberately not used here: its strokes are too heavy for caption buttons.
fn caption_glyph(
    painter: &egui::Painter,
    center: egui::Pos2,
    action: CaptionAction,
    maximized: bool,
    color: egui::Color32,
) {
    let pixels = painter.pixels_per_point();
    let snap = |value: f32| ((value * pixels - 0.5).round() + 0.5) / pixels;
    let stroke = egui::Stroke::new(1.0 / pixels, color);
    let cx = snap(center.x);
    let cy = snap(center.y);
    let left = cx - 5.0;
    let right = cx + 5.0;
    let top = cy - 5.0;
    let bottom = cy + 5.0;
    match action {
        CaptionAction::Minimize => {
            painter.line_segment([egui::pos2(left, cy), egui::pos2(right, cy)], stroke);
        }
        CaptionAction::ToggleMaximize if maximized => {
            // Restore: a back square offset up and right behind a front one.
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(left + 2.0, top),
                    egui::pos2(right, bottom - 2.0),
                ),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
            painter.rect_stroke(
                egui::Rect::from_min_max(
                    egui::pos2(left, top + 2.0),
                    egui::pos2(right - 2.0, bottom),
                ),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        CaptionAction::ToggleMaximize => {
            painter.rect_stroke(
                egui::Rect::from_min_max(egui::pos2(left, top), egui::pos2(right, bottom)),
                0.0,
                stroke,
                egui::StrokeKind::Middle,
            );
        }
        CaptionAction::Close => {
            painter.line_segment([egui::pos2(left, top), egui::pos2(right, bottom)], stroke);
            painter.line_segment([egui::pos2(right, top), egui::pos2(left, bottom)], stroke);
        }
    }
}

/// A borderless icon button on the dark title bar.
fn title_icon_button(
    ui: &mut egui::Ui,
    palette: TitlePalette,
    glyph: Icon,
    size: f32,
) -> egui::Response {
    let (rect, response) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::click());
    if response.hovered() {
        ui.painter().rect_filled(rect, 7.0, palette.hover);
    }
    let color = if response.hovered() {
        palette.text
    } else {
        palette.text_muted
    };
    icon(ui.painter(), rect.shrink(size * 0.27), glyph, color);
    response
}

/// Toolbar button with an icon and label.
fn shell_tool_button(
    ui: &mut egui::Ui,
    palette: Palette,
    glyph: Icon,
    label: &str,
    active: bool,
) -> egui::Response {
    let font = egui::FontId::proportional(12.0);
    let galley = ui
        .painter()
        .layout_no_wrap(label.to_owned(), font.clone(), palette.text);
    let width = galley.size().x + 40.0;
    let (rect, response) = ui.allocate_exact_size(egui::vec2(width, 32.0), egui::Sense::click());
    let background = if active {
        palette.accent_soft
    } else if response.hovered() {
        palette.surface
    } else {
        egui::Color32::TRANSPARENT
    };
    ui.painter().rect_filled(rect, 8.0, background);
    let color = if active { palette.accent } else { palette.text };
    icon(
        ui.painter(),
        egui::Rect::from_center_size(
            egui::pos2(rect.left() + 15.0, rect.center().y),
            egui::Vec2::splat(14.0),
        ),
        glyph,
        color,
    );
    ui.painter().text(
        egui::pos2(rect.left() + 27.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    response
}

/// Thin vertical separator.
fn shell_divider(ui: &mut egui::Ui, palette: Palette) {
    let (rect, _) = ui.allocate_exact_size(egui::vec2(9.0, 22.0), egui::Sense::hover());
    ui.painter().line_segment(
        [
            egui::pos2(rect.center().x, rect.top()),
            egui::pos2(rect.center().x, rect.bottom()),
        ],
        egui::Stroke::new(1.0_f32, palette.border),
    );
}

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
    /// Resize the undecorated window when the pointer is near an edge.
    ///
    /// The window has no native frame, so the shell owns the edge hit zones.
    /// The zones are deliberately thin (6 pt) and only react to the primary
    /// button, matching the behaviour of a normal Windows frame. A maximized
    /// window has no draggable frame edges, so no resize is started there.
    fn window_resize_handles(&self, ctx: &egui::Context) {
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

    /// Measure real page sizes in points from the renderer.
    ///
    /// Each page is rasterized at a tiny scale and its pixel size converted
    /// back to points. Measuring is capped so an enormous document cannot stall
    /// the open path; pages beyond the cap keep the US-Letter layout fallback.
    #[cfg(feature = "pdfium")]
    /// Extract one page's real text, or `None` when the renderer cannot.
    #[cfg(feature = "pdfium")]
    fn extract_text_page(&self, page: usize) -> Option<OwnedTextPage> {
        let renderer = self.pdfium_renderer.as_ref()?;
        let cancellation = pdfxi_text::CancellationToken::new();
        let extracted = renderer
            .extract_page(page, &cancellation, &mut |_| {})
            .ok()?;
        let spans = extracted
            .spans()
            .iter()
            .map(|span| {
                OwnedSpan::new(
                    span.text.clone(),
                    span.x,
                    span.y - span.height,
                    span.width,
                    span.height,
                )
            })
            .collect();
        Some(OwnedTextPage::new(page, spans))
    }

    /// Push the extracted pages into the find model.
    fn sync_find_pages(&mut self) {
        let pages: Vec<(usize, String)> = self
            .text_pages
            .iter()
            .map(|page| {
                let text: String = page.spans().iter().map(|span| span.text.as_str()).collect();
                (page.page_number(), text)
            })
            .collect();
        self.find.set_pages(pages);
    }

    /// Extract at most two pending pages for this frame.
    #[cfg(feature = "pdfium")]
    fn pump_text_extraction(&mut self) {
        if self.pending_text_pages.is_empty() {
            return;
        }
        for _ in 0..2 {
            let Some(page) = self.pending_text_pages.pop_front() else {
                break;
            };
            if let Some(text) = self.extract_text_page(page) {
                self.text_pages.push(text);
            }
        }
        self.sync_find_pages();
    }

    /// Text extraction is a no-op without a native renderer.
    #[cfg(not(feature = "pdfium"))]
    fn pump_text_extraction(&mut self) {}

    /// Resolve the theme preference to the palette to draw now.
    ///
    /// `Follow system` reads the OS preference from the `egui` context; when
    /// the platform reports nothing (or after the first frame has not arrived
    /// yet) it falls back to the light theme instead of guessing dark.
    fn resolve_theme(&self, ctx: &egui::Context) -> ThemeMode {
        match self.theme_preference {
            ThemePreference::Light => ThemeMode::Light,
            ThemePreference::Dark => ThemeMode::Dark,
            ThemePreference::System => match ctx.system_theme() {
                Some(egui::Theme::Dark) => ThemeMode::Dark,
                _ => ThemeMode::Light,
            },
        }
    }

    /// Draw the complete product shell.
    fn shell_ui(&mut self, ctx: &egui::Context) {
        self.theme = self.resolve_theme(ctx);
        if self.applied_theme != Some(self.theme) {
            design::apply(ctx, self.theme);
            self.applied_theme = Some(self.theme);
        }
        let palette = Palette::for_mode(self.theme);
        let title_palette = TitlePalette::for_mode(self.theme);

        egui::TopBottomPanel::top("shell_title")
            .exact_height(metrics::TITLE_BAR)
            // No side padding: the Windows caption buttons must reach the
            // window edge. The title bar applies its own insets.
            .frame(egui::Frame::new().fill(title_palette.background))
            .show(ctx, |ui| self.title_bar_ui(ui, title_palette));

        if let Some(message) = self.open_error_banner.clone() {
            egui::TopBottomPanel::top("shell_error")
                .frame(shell_panel_frame(palette, 14))
                .show(ctx, |ui| {
                    ui.horizontal_centered(|ui| {
                        ui.label(egui::RichText::new(message).color(palette.accent));
                        ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                            if ui.button("Dismiss").clicked() {
                                self.open_error_banner = None;
                            }
                        });
                    });
                });
        }

        egui::TopBottomPanel::top("shell_command")
            .exact_height(metrics::TOOL_BAR)
            .frame(shell_panel_frame(palette, 14))
            .show(ctx, |ui| self.document_bar_ui(ui, palette));

        if self.find_open {
            egui::TopBottomPanel::top("shell_find")
                .frame(shell_panel_frame(palette, 14))
                .show(ctx, |ui| self.find_ui(ui));
        }

        if self.state.active_tab().is_some() {
            egui::SidePanel::left("shell_pages")
                .exact_width(metrics::PAGES)
                .resizable(false)
                .frame(
                    egui::Frame::new()
                        .fill(palette.panel)
                        .inner_margin(egui::Margin {
                            left: 14,
                            right: 12,
                            top: 12,
                            bottom: 10,
                        }),
                )
                .show(ctx, |ui| self.side_panel_ui(ui, palette));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.canvas))
            .show(ctx, |ui| self.canvas_ui(ui));
    }

    /// The document command bar: only controls wired to real document state.
    fn document_bar_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let has_document = self.state.active_tab().is_some();
        ui.horizontal_centered(|ui| {
            if shell_tool_button(ui, palette, Icon::File, "Open", false).clicked() {
                self.open_dialog_open = true;
            }
            ui.add_space(8.0);
            ui.add_enabled_ui(has_document, |ui| {
                if shell_icon_button(ui, palette, Icon::ArrowLeft, 30.0)
                    .on_hover_text("Rotate counter-clockwise")
                    .clicked()
                {
                    self.rotate_counterclockwise();
                }
                if shell_icon_button(ui, palette, Icon::ArrowRight, 30.0)
                    .on_hover_text("Rotate clockwise")
                    .clicked()
                {
                    self.rotate_clockwise();
                }
                if shell_tool_button(ui, palette, Icon::FitPage, "Fit", false)
                    .on_hover_text("Fit the page in the window")
                    .clicked()
                {
                    self.fit_page();
                }
                if shell_tool_button(ui, palette, Icon::Fullscreen, "Actual", false)
                    .on_hover_text("Actual size")
                    .clicked()
                {
                    self.actual_size();
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if shell_tool_button(ui, palette, Icon::Search, "Find", self.find_open).clicked() {
                    if self.find_open {
                        self.close_find();
                    } else {
                        self.open_find();
                    }
                }
            });
        });
    }

    /// Empty-document Home state: app mark, open action, drop hint, shortcut.
    fn empty_home_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.vertical_centered(|ui| {
            ui.add_space(96.0);
            let (mark, _) = ui.allocate_exact_size(egui::vec2(56.0, 56.0), egui::Sense::hover());
            ui.painter().rect_filled(mark, 14.0, palette.accent);
            icon(
                ui.painter(),
                mark.shrink(15.0),
                Icon::File,
                egui::Color32::WHITE,
            );
            ui.add_space(16.0);
            ui.label(
                egui::RichText::new("Open a PDF to begin")
                    .size(20.0)
                    .strong()
                    .color(palette.text),
            );
            ui.add_space(14.0);
            let open =
                egui::Button::new(egui::RichText::new("Open a PDF").color(egui::Color32::WHITE))
                    .fill(palette.accent)
                    .corner_radius(8.0)
                    .min_size(egui::vec2(150.0, 36.0));
            if ui.add(open).clicked() {
                self.open_dialog_open = true;
            }
            ui.add_space(10.0);
            ui.label(egui::RichText::new("or drop a file here").color(palette.text_muted));
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new("Ctrl+O")
                    .monospace()
                    .color(palette.text_muted),
            );
        });
    }

    fn title_bar_ui(&mut self, ui: &mut egui::Ui, palette: TitlePalette) {
        let panel = ui.max_rect();
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
            ui.ctx().send_viewport_cmd(egui::ViewportCommand::StartDrag);
        }
        if drag.double_clicked() {
            ui.ctx()
                .send_viewport_cmd(caption_command(CaptionAction::ToggleMaximize, maximized));
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
                    ui.ctx()
                        .send_viewport_cmd(caption_command(*action, maximized));
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
                            ui.ctx()
                                .send_viewport_cmd(caption_command(action, maximized));
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
                    self.open_dialog_open = true;
                }
                if let Some(id) = activate {
                    self.activate_tab(id);
                }
                if let Some(id) = close_tab {
                    self.close_tab_by_id(id);
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
                        let preference = match self.theme {
                            ThemeMode::Light => ThemePreference::Dark,
                            ThemeMode::Dark => ThemePreference::Light,
                        };
                        self.set_theme_preference(preference);
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
                        self.set_theme_preference(preference);
                    }
                    if let Some(style) = controls_choice {
                        self.set_window_controls(style);
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

    fn side_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
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

    fn step_page(&mut self, delta: isize) {
        let Some(tab) = self.state.active_tab() else {
            self.status = "No document open".to_owned();
            return;
        };
        let count = tab.page_count();
        if count == 0 {
            return;
        }
        let current = tab.current_page() as isize;
        let target = (current + delta).clamp(0, count as isize - 1) as usize;
        self.select_thumbnail_page(target);
    }

    /// Floating page and zoom controls over the bottom of the canvas.
    ///
    /// The reference keeps the page controls in a compact pill instead of a
    /// full-width status bar, so the canvas keeps its full height.
    fn floating_page_bar_ui(
        &mut self,
        ui: &mut egui::Ui,
        canvas_rect: egui::Rect,
        palette: Palette,
    ) {
        let width = 260.0_f32;
        let pos = egui::pos2(
            canvas_rect.center().x - width / 2.0,
            canvas_rect.bottom() - 54.0,
        );
        egui::Area::new(egui::Id::new("shell-page-bar"))
            .order(egui::Order::Foreground)
            .fixed_pos(pos)
            .show(ui.ctx(), |ui| {
                egui::Frame::new()
                    .fill(palette.panel)
                    .stroke(egui::Stroke::new(1.0_f32, palette.border))
                    .corner_radius(20.0)
                    .inner_margin(egui::Margin {
                        left: 8,
                        right: 8,
                        top: 4,
                        bottom: 4,
                    })
                    .show(ui, |ui| {
                        ui.horizontal_centered(|ui| {
                            if shell_icon_button(ui, palette, Icon::ChevronLeft, 24.0)
                                .on_hover_text("Previous page")
                                .clicked()
                            {
                                self.step_page(-1);
                            }
                            let (current, count) = self
                                .state
                                .active_tab()
                                .map(|tab| (tab.current_page(), tab.page_count()))
                                .unwrap_or((0, 0));
                            ui.label(
                                egui::RichText::new(format!("{} of {}", current + 1, count.max(1)))
                                    .size(10.5)
                                    .color(palette.text),
                            );
                            if shell_icon_button(ui, palette, Icon::ChevronRight, 24.0)
                                .on_hover_text("Next page")
                                .clicked()
                            {
                                self.step_page(1);
                            }
                            shell_divider(ui, palette);
                            let zoom_label = self
                                .state
                                .active_tab()
                                .map(|tab| match tab.zoom().scale() {
                                    Some(scale) => format!("{:.0}%", scale * 100.0),
                                    None => "Fit".to_owned(),
                                })
                                .unwrap_or_else(|| "Untitled".to_owned());
                            ui.menu_button(
                                egui::RichText::new(zoom_label)
                                    .size(10.5)
                                    .color(palette.text),
                                |ui| {
                                    if ui.button("Fit Page").clicked() {
                                        self.fit_page();
                                        ui.close();
                                    }
                                    if ui.button("Fit Width").clicked() {
                                        self.fit_width();
                                        ui.close();
                                    }
                                    if ui.button("Actual Size").clicked() {
                                        self.actual_size();
                                        ui.close();
                                    }
                                    ui.separator();
                                    if ui.button("Zoom In").clicked() {
                                        self.zoom_in();
                                        ui.close();
                                    }
                                    if ui.button("Zoom Out").clicked() {
                                        self.zoom_out();
                                        ui.close();
                                    }
                                },
                            );
                            if shell_icon_button(ui, palette, Icon::ZoomOut, 24.0)
                                .on_hover_text("Zoom out")
                                .clicked()
                            {
                                self.zoom_out();
                            }
                            if shell_icon_button(ui, palette, Icon::ZoomIn, 24.0)
                                .on_hover_text("Zoom in")
                                .clicked()
                            {
                                self.zoom_in();
                            }
                            if shell_icon_button(ui, palette, Icon::FitPage, 24.0)
                                .on_hover_text("Fit page")
                                .clicked()
                            {
                                self.fit_page();
                            }
                        });
                    });
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

#[cfg(test)]
mod tests {
    use super::*;

    fn app_with_pages(pages: usize) -> PdfxiApp {
        let mut app = PdfxiApp::new();
        let id = app.state.open_tab("test.pdf");
        app.finish_new_tab(id);
        if let Some(tab) = app.state.tab_mut(id) {
            tab.set_page_count(pages);
        }
        app.thumbs.reset(pages);
        app
    }

    #[test]
    fn startup_state_has_no_tabs() {
        let app = PdfxiApp::new();
        assert_eq!(app.state().tab_count(), 0);
        assert!(app.state().active_tab().is_none());
        assert!(app.selected_text().is_empty());
    }

    #[test]
    fn opening_a_document_starts_without_invented_content() {
        let app = app_with_pages(3);
        let tab = app.state().active_tab().expect("active tab");
        assert_eq!(tab.page_count(), 3);
        assert!(app.find_state().matches().is_empty());
        assert!(app.selected_text().is_empty());
    }

    #[test]
    fn view_commands_work_with_and_without_a_document() {
        let mut empty = PdfxiApp::new();
        empty.zoom_in();
        empty.rotate_clockwise();
        empty.fit_page();
        empty.actual_size();
        assert!(empty.state().active_tab().is_none());

        let mut app = app_with_pages(2);
        app.zoom_in();
        app.rotate_clockwise();
        app.fit_page();
        app.actual_size();
        assert!(app.state().active_tab().is_some());
    }

    #[test]
    fn tabs_open_close_and_reorder() {
        let mut app = PdfxiApp::new();
        let first = app.state.open_tab("one.pdf");
        app.finish_new_tab(first);
        let second = app.state.open_tab("two.pdf");
        app.finish_new_tab(second);
        assert_eq!(app.state().tab_count(), 2);
        assert!(app.activate_next_tab());
        assert!(app.reorder_tab(second, 0));
        app.close_active_tab();
        assert_eq!(app.state().tab_count(), 1);
    }

    #[test]
    fn open_path_text_rejects_invalid_input() {
        let mut app = PdfxiApp::new();
        assert!(!app.open_path_text(""));
        assert!(!app.open_path_text("notes.txt"));
        assert_eq!(app.state().tab_count(), 0);
    }

    #[test]
    fn caption_layout_matches_the_window_control_style() {
        assert_eq!(
            caption_layout(WindowControlStyle::Windows),
            (
                CaptionSide::Right,
                [
                    CaptionAction::Minimize,
                    CaptionAction::ToggleMaximize,
                    CaptionAction::Close,
                ],
            )
        );
        assert_eq!(
            caption_layout(WindowControlStyle::MacOs),
            (
                CaptionSide::Left,
                [
                    CaptionAction::Close,
                    CaptionAction::Minimize,
                    CaptionAction::ToggleMaximize,
                ],
            )
        );
    }

    #[test]
    fn caption_commands_follow_the_maximized_state() {
        assert_eq!(
            caption_command(CaptionAction::Minimize, false),
            egui::ViewportCommand::Minimized(true)
        );
        assert_eq!(
            caption_command(CaptionAction::ToggleMaximize, false),
            egui::ViewportCommand::Maximized(true)
        );
        assert_eq!(
            caption_command(CaptionAction::ToggleMaximize, true),
            egui::ViewportCommand::Maximized(false)
        );
        assert_eq!(
            caption_command(CaptionAction::Close, false),
            egui::ViewportCommand::Close
        );
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn real_document_uses_real_page_geometry_and_text() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let mut app = PdfxiApp::new();
        assert!(
            app.open_path_text(path.to_str().expect("utf-8 fixture path")),
            "the real fixture must open"
        );
        let tab = app.state().active_tab().expect("an open tab");
        assert_eq!(tab.page_count(), 2);
        assert!(
            app.page_sizes.len() >= 2,
            "page sizes are measured from the document"
        );

        let renderer = PdfiumRenderer::from_file(&path).expect("fixture opens in PDFium");
        for (page, size) in app.page_sizes.iter().take(2).enumerate() {
            let expected_width = renderer
                .page_size(PageIndex::new(page))
                .expect("page size")
                .width_pt;
            assert!(
                (size.width_pt - expected_width).abs() < 1.0,
                "page {page} width {} vs renderer {expected_width}",
                size.width_pt
            );
        }

        app.set_find_query("lighthouse");
        assert!(
            app.find_state().matches().iter().any(|hit| hit.page() == 0),
            "lighthouse is found on page 1 from real text"
        );
        app.set_find_query("meadow");
        assert!(
            app.find_state().matches().iter().any(|hit| hit.page() == 1),
            "meadow is found on page 2 from real text"
        );
    }

    #[test]
    fn opening_a_non_pdf_shows_an_error_and_no_tab() {
        let mut app = PdfxiApp::new();
        assert!(!app.open_path_text("notes.txt"));
        assert_eq!(app.state().tab_count(), 0);
        assert!(app.open_error_banner().is_some());
        app.dismiss_open_error();
        assert!(app.open_error_banner().is_none());
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn two_real_documents_switch_and_close_independently() {
        let root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../fixtures");
        let first = root.join("minimal.pdf");
        let second = root.join("text.pdf");
        let mut app = PdfxiApp::new();
        assert!(app.open_path_text(first.to_str().expect("utf-8 path")));
        assert!(app.open_path_text(second.to_str().expect("utf-8 path")));
        assert_eq!(app.state().tab_count(), 2);

        let second_id = app.state().active_tab_id().expect("second tab active");
        let ids = app.tab_strip_snapshot();
        assert_eq!(ids.len(), 2);
        let first_id = ids
            .iter()
            .map(|entry| entry.0)
            .find(|id| *id != second_id)
            .expect("first tab");
        app.activate_tab(first_id);
        assert_eq!(app.state().active_tab_id(), Some(first_id));
        assert!(app.close_tab_by_id(first_id));
        assert_eq!(app.state().tab_count(), 1);
        assert_eq!(app.state().active_tab_id(), Some(second_id));
    }

    #[test]
    fn zoom_clamps_and_fit_width_uses_the_page_width() {
        let mut app = app_with_pages(1);
        app.fit_width();
        assert_eq!(
            app.state().active_tab().map(|tab| tab.zoom()),
            Some(Zoom::FitWidth)
        );
        let wide = layout_for_state(
            app.state(),
            CanvasSize::new(800.0, 600.0),
            &[PageSize::new(612.0, 792.0)],
        )
        .expect("fit-width layout")
        .pages()[0]
            .scale;
        let narrow = layout_for_state(
            app.state(),
            CanvasSize::new(800.0, 600.0),
            &[PageSize::new(306.0, 792.0)],
        )
        .expect("fit-width layout")
        .pages()[0]
            .scale;
        assert!(
            (narrow / wide - 2.0).abs() < 0.01,
            "half the page width doubles the fit scale ({narrow} vs {wide})"
        );

        for _ in 0..200 {
            app.zoom_in();
        }
        let maximum = app
            .state()
            .active_tab()
            .expect("tab")
            .zoom()
            .scale()
            .expect("custom zoom");
        assert!(maximum <= crate::MAX_ZOOM + 1e-4, "zoom {maximum}");
        for _ in 0..400 {
            app.zoom_out();
        }
        let minimum = app
            .state()
            .active_tab()
            .expect("tab")
            .zoom()
            .scale()
            .expect("custom zoom");
        assert!(minimum >= crate::MIN_ZOOM - 1e-4, "zoom {minimum}");
    }

    #[test]
    fn page_navigation_clamps_at_the_first_and_last_page() {
        let mut app = app_with_pages(3);
        app.step_page(-1);
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 0);
        app.step_page(1);
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 1);
        app.step_page(1);
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 2);
        app.step_page(1);
        assert_eq!(
            app.state().active_tab().expect("tab").current_page(),
            2,
            "the last page is a hard stop"
        );
        app.step_page(-1);
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 1);
        app.step_page(-1);
        app.step_page(-1);
        assert_eq!(
            app.state().active_tab().expect("tab").current_page(),
            0,
            "the first page is a hard stop"
        );
    }
}
