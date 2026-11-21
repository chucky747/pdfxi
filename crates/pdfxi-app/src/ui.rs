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

use std::collections::{BTreeMap, BTreeSet, VecDeque};
use std::path::{Path, PathBuf};

use crate::OpenError;
use crate::design::{self, Palette, ThemeMode, TitlePalette, metrics};
use crate::settings::{self, LaunchOptions, LaunchPanel, ThemePreference, WindowControlStyle};
use egui::Key;

use crate::{
    AppState, CanvasHit, CanvasLayout, CanvasPoint, CanvasRect, CanvasSize, CloseDecision,
    DEFAULT_ZOOM, DocumentTab, FindMatch, FindOptions, FindState, OpenOutcome, OwnedSpan,
    OwnedTextPage, PageSize, SelectionModel, ShortcutKey, ShortcutModifiers, TabId,
    ThumbnailPaneState, Toolset, ViewCommand, ViewMode, Zoom, app_name, map_shortcut,
};

#[cfg(feature = "pdfium")]
use crate::raster::RasterCache;
use pdfxi_document::DocumentSession;
#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, PageIndex, PdfiumRenderer, Renderer};
#[cfg(feature = "pdfium")]
use pdfxi_text::TextExtractor;

#[path = "shell/mod.rs"]
mod shell;

#[path = "library.rs"]
pub mod library;

pub use shell::capabilities::{Capabilities, Capability, RailItem, ToolRow};
pub use shell::commands::AppCommand;
pub use shell::comments::AnnotationEntry;
use shell::preferences::{self as shell_preferences, ShellPreferences};
pub use shell::session::{CloseChoice, ClosePlan, plan_close, save_conflict_offers_save_as};
use shell::widgets::shell_panel_frame;
pub use shell::widgets::{
    CAPTION_BUTTON_WIDTH, CaptionAction, CaptionSide, caption_command, caption_layout,
};

/// An `egui` texture plus the raster key it currently holds.
///
/// Reusing one texture handle and calling [`egui::TextureHandle::set`] on zoom
/// or rotation avoids allocating a new GPU texture every frame.
#[cfg(feature = "pdfium")]
struct LiveTexture {
    handle: egui::TextureHandle,
    scale_bits: u32,
    rotation: u16,
    /// Renderer generation the pixels came from.
    generation: u64,
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
    // A page taller than the viewport cannot be centred; align its top so the
    // first line is visible instead of cutting it off.
    let desired = if geometry.rect.height >= viewport_height {
        geometry.rect.y
    } else {
        geometry.rect.y - (viewport_height - geometry.rect.height) * 0.5
    };
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

/// Locate the bundled user-guide folder, if the build includes one.
fn bundled_docs_dir() -> Option<PathBuf> {
    let mut candidates = Vec::new();
    if let Ok(executable) = std::env::current_exe()
        && let Some(directory) = executable.parent()
    {
        candidates.push(directory.join("docs"));
        candidates.push(directory.join("docs").join("book"));
        if let Some(parent) = directory.parent() {
            candidates.push(parent.join("docs"));
        }
    }
    candidates.push(PathBuf::from("docs"));
    candidates.into_iter().find(|path| path.is_dir())
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
            let print_requested = arguments.iter().any(|argument| argument == "--print");
            // View-only deep link: open the Nth annotation's thread at startup.
            let open_annotation = arguments.iter().find_map(|argument| {
                argument
                    .strip_prefix("--open-annotation=")
                    .and_then(|value| value.parse::<usize>().ok())
            });
            let launch = settings::parse_launch_arguments(&arguments);
            let mut app = PdfxiApp::new_for_window();
            app.apply_launch_options(&launch);
            app.open_startup_argument(arguments);
            app.apply_launch_title(launch.title.as_deref());
            app.apply_launch_panel(launch.panel);
            app.apply_launch_tool(launch.tool);
            if let Some(question) = launch.assistant_question {
                app.request_launch_question(question);
            }
            if print_requested {
                app.dispatch(AppCommand::Print, None);
            }
            if let Some(index) = open_annotation
                && let Some(entry) = app.annotations().get(index.saturating_sub(1))
            {
                app.dispatch(
                    AppCommand::OpenAnnotation {
                        page: entry.page,
                        id: entry.view.id.clone(),
                    },
                    None,
                );
            }
            Ok(Box::new(app))
        }),
    )
    .map_err(|error| format!("PDFXI viewer exited with an error: {error}"))
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
    /// Feature families enabled in this build.
    capabilities: Capabilities,
    /// OpenCode connection screen state and its worker-thread probe.
    assistant: shell::assistant_connect::AssistantConnection,
    /// Per-document assistant conversation and its worker-thread turn.
    assistant_chat: shell::assistant_chat::AssistantChat,
    /// A launch/deep-link question to ask once an explicit connect succeeds.
    ///
    /// `None` for every normal run: opening the panel never sets it.
    pending_assistant_question: Option<String>,
    /// Active left rail and side-panel content.
    active_rail: RailItem,
    /// Whether the right tools panel is open.
    tools_open: bool,
    /// Tools-panel search query.
    tools_search: String,
    /// Page thumbnails as a grid rather than a list.
    grid_view: bool,
    /// Editable page-number box text.
    page_box: String,
    /// Set while the page-number box owns keyboard focus.
    page_box_focused: bool,
    /// Persisted shell preferences (tools panel visibility).
    shell_prefs: ShellPreferences,
    /// Where shell preferences are saved; `None` for headless viewers.
    shell_prefs_path: Option<PathBuf>,
    /// Local library, recents and stars.
    library: library::Library,
    /// PDFs found in the library folder on the last refresh.
    library_entries: Vec<library::LibraryEntry>,
    /// Whether the library folder has been read this run.
    library_loaded: bool,
    /// Cached storage total for the meter.
    storage_bytes: u64,
    /// Whether the help window is open.
    help_open: bool,
    /// Editable display name in the identity popover.
    identity_name: String,
    /// Whether the identity popover is open.
    identity_open: bool,
    /// One editing session per open tab, keyed by tab id.
    pub(super) sessions: BTreeMap<TabId, DocumentSession>,
    /// Tabs whose document permissions deny modification.
    pub(super) read_only_tabs: BTreeSet<TabId>,
    /// Open password prompt, if any.
    pub(super) password_prompt: Option<shell::session::PasswordPrompt>,
    /// Open unsaved-changes prompt, if any.
    pub(super) pending_close: Option<shell::session::PendingClose>,
    /// Open external-modification dialog, if any.
    pub(super) save_conflict: Option<shell::session::SaveConflict>,
    /// Comments list, rail badge, note draft and thread popup.
    comments: shell::comments::CommentsUiState,
    /// Fill & Sign state: cached AcroForm model, panel, draft and notices.
    forms: shell::forms::FormsUiState,
    /// Certificate signing card and signature verification readout.
    signing: shell::signing::SigningUiState,
    /// Fill & Sign typed/drawn/image signature placement card.
    signature_place: shell::signature_place::SignaturePlaceState,
    /// Pre-print panel, progress and worker state.
    print_ui: shell::print_dialog::PrintUiState,
    /// Revision of the active session bound to the renderer.
    ///
    /// Alongside `renderer_path` this key notices edits, undo and redo so the
    /// canvas is re-rasterized from the session's current bytes.
    renderer_revision: usize,
    /// Monotonic count of renderer loads; a change means the canvas source is
    /// newer than any raster or texture built from the previous document state.
    render_generation: u64,
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
    /// Set when opening a signed document should reveal its signature block.
    pending_signature_scroll: Option<usize>,
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
        let defaults = Self::default();
        let identity_name = defaults.settings.display_name.clone();
        Self {
            page_box: "1".to_owned(),
            identity_name,
            ..defaults
        }
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
        self.shell_prefs_path = shell_preferences::default_path();
        if let Some(prefs_path) = &self.shell_prefs_path {
            self.shell_prefs = shell_preferences::load(prefs_path);
            self.tools_open = self.shell_prefs.tools_open;
        }
        self.identity_name = self.settings.display_name.clone();
        self.assistant
            .set_saved_model(self.settings.assistant_model.clone());
        self.assistant
            .set_last_endpoint(self.settings.assistant_endpoint.clone());
        self.capabilities = if self.settings.shared_folder.is_some() {
            Capabilities::available().with(Capability::Shared)
        } else {
            Capabilities::available()
        };
        self.library = library::Library::new(library::Library::default_path());
        self.refresh_library();
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

    /// Apply a `--set-title=` title edit through the active session.
    ///
    /// The same `session.edit` path a document-properties dialog would use;
    /// it exists so captures and headless verification can show a dirty tab.
    fn apply_launch_title(&mut self, title: Option<&str>) {
        let Some(title) = title else {
            return;
        };
        if let Err(error) = self.set_active_document_title(title) {
            self.status = error.to_string();
        }
    }

    /// Apply a `--rail=`/`--panel=` deep link after startup documents open.
    fn apply_launch_panel(&mut self, panel: Option<LaunchPanel>) {
        let Some(panel) = panel else {
            return;
        };
        self.active_rail = match panel {
            LaunchPanel::Home => RailItem::Home,
            LaunchPanel::Files => RailItem::Files,
            LaunchPanel::Recent => RailItem::Recent,
            LaunchPanel::Starred => RailItem::Starred,
            LaunchPanel::Pages => RailItem::Pages,
            LaunchPanel::Shared => RailItem::Shared,
            LaunchPanel::Comments => RailItem::Comments,
            LaunchPanel::AiAssistant => RailItem::AiAssistant,
        };
    }

    /// Apply a `--tool=` deep link after startup documents open.
    ///
    /// The tool name maps to the same [`AppCommand`] its panel control emits, so
    /// a deep link can never do more than the real control. When the capability
    /// is off the command does what a click would: it reports the tool is not
    /// available and changes nothing else.
    fn apply_launch_tool(&mut self, tool: Option<settings::LaunchTool>) {
        let Some(tool) = tool else {
            return;
        };
        let command = match tool {
            settings::LaunchTool::Edit => AppCommand::EditDocument,
            settings::LaunchTool::Comment => AppCommand::Comment,
            settings::LaunchTool::Highlight => AppCommand::Highlight,
            settings::LaunchTool::Fill => AppCommand::FillAndSign,
            settings::LaunchTool::Sign => AppCommand::Sign,
            settings::LaunchTool::Organize => AppCommand::Organize,
            settings::LaunchTool::Export => AppCommand::Export,
            settings::LaunchTool::Convert => AppCommand::ConvertToPdf,
            settings::LaunchTool::Protect => AppCommand::Protect,
            settings::LaunchTool::AiAssistant => AppCommand::AiAssistant,
        };
        self.dispatch(command, None);
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

    /// Whether the right tools panel is currently open.
    #[must_use]
    pub fn tools_open(&self) -> bool {
        self.tools_open
    }

    /// Whether the Pages panel draws thumbnails as a grid rather than a list.
    #[must_use]
    pub fn grid_view(&self) -> bool {
        self.grid_view
    }

    /// The rail item whose side-panel content is currently shown.
    #[must_use]
    pub fn active_rail(&self) -> RailItem {
        self.active_rail
    }

    /// The local display name used for comment authors and signers.
    #[must_use]
    pub fn display_name(&self) -> &str {
        &self.settings.display_name
    }

    /// Avatar initials derived from the display name.
    #[must_use]
    pub fn initials(&self) -> String {
        library::initials(&self.settings.display_name)
    }

    /// Borrow the local library, recents and stars.
    #[must_use]
    pub fn library(&self) -> &library::Library {
        &self.library
    }

    /// PDFs found in the library folder on the last refresh.
    #[must_use]
    pub fn library_entries(&self) -> &[library::LibraryEntry] {
        &self.library_entries
    }

    /// Cached storage total for the meter, in bytes.
    #[must_use]
    pub fn storage_bytes(&self) -> u64 {
        self.storage_bytes
    }

    /// Whether the help window is open.
    #[must_use]
    pub fn help_open(&self) -> bool {
        self.help_open
    }

    /// Whether `path` is starred.
    #[must_use]
    pub fn is_starred(&self, path: &Path) -> bool {
        self.library.is_starred(path)
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
        self.pending_signature_scroll = None;
        self.thumbs.reset(0);
        self.thumb_follow_active = true;
        self.assistant_chat
            .sync_document(self.state.active_tab_id());
        self.active_rail = RailItem::Pages;
        self.page_box = "1".to_owned();
        #[cfg(feature = "pdfium")]
        self.thumb_textures.clear();
    }

    /// Close the active tab, if any, using the configured [`UiClosePolicy`].
    ///
    /// A clean tab closes immediately. A dirty tab raises the save / discard /
    /// cancel prompt. A vetoed close leaves the workspace untouched and reports
    /// why through [`Self::status`].
    pub fn close_active_tab(&mut self) {
        if self.state.active_tab_id().is_some() {
            self.request_close_active_tab();
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
                self.forget_tab_state(id);
                self.status = format!("Closed tab {id}");
                self.restore_scroll_from_active();
                self.refresh_renderer_for_active_tab();
                true
            }
            crate::CloseOutcome::Cancelled { .. } => {
                self.status = format!("Unsaved changes in {title}; save or discard before closing");
                false
            }
        }
    }

    /// Close a tab immediately, ignoring unsaved changes.
    ///
    /// Used only after the user chose Discard, or after a successful save in
    /// the prompt. Model policy is not consulted, so a discarded dirty tab
    /// actually closes.
    pub(crate) fn force_close_tab(&mut self, id: TabId) -> bool {
        match self.state.close_tab(id) {
            Ok(_) => {
                self.forget_tab_state(id);
                self.status = format!("Closed tab {id}");
                self.restore_scroll_from_active();
                self.refresh_renderer_for_active_tab();
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Drop session, permission and dialog state owned by a closed tab.
    pub(super) fn forget_tab_state(&mut self, id: TabId) {
        self.sessions.remove(&id);
        self.read_only_tabs.remove(&id);
        if self
            .pending_close
            .as_ref()
            .is_some_and(|close| close.id == id)
        {
            self.pending_close = None;
        }
        if self
            .save_conflict
            .as_ref()
            .is_some_and(|conflict| conflict.id == id)
        {
            self.save_conflict = None;
        }
        self.refresh_annotations();
        self.refresh_forms();
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
        // Untitled tabs and design previews have no readable file behind them;
        // they keep the model-only path so tests and the welcome canvas work.
        if !path.is_file() {
            return self.open_document_without_session(path);
        }
        self.open_document_session(path, None)
    }

    /// Open a path without a document session (the pre-session behaviour).
    ///
    /// Only used for paths that are not readable files, where a session cannot
    /// exist; real documents always go through [`Self::open_document_session`].
    fn open_document_without_session(&mut self, path: &Path) -> OpenOutcome {
        let outcome = self.state.open_document(path);
        match outcome {
            OpenOutcome::Opened { id } => {
                self.finish_new_tab(id);
                self.refresh_renderer_for_active_tab();
                let title = self.state.tab_title(id).unwrap_or_default();
                self.library.record_open(path, &title, 0);
                self.status = format!("Opened {}", path.display());
            }
            OpenOutcome::Focused { id } => {
                self.on_active_tab_changed();
                let title = self.state.tab_title(id).unwrap_or_default();
                self.library.record_open(path, &title, 0);
                self.status = format!("Focused existing tab {id}");
            }
            OpenOutcome::PasswordRequired { .. } | OpenOutcome::Failed { .. } => {}
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
        let mut opened = false;
        for argument in args {
            if argument.starts_with('-') {
                continue;
            }
            if self.open_path_text(&argument) {
                opened = true;
            }
        }
        opened
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
        self.assistant_chat
            .sync_document(self.state.active_tab_id());
        self.refresh_renderer_for_active_tab();
        self.refresh_annotations();
        self.refresh_forms();
    }

    /// Bind the PDFium renderer to the active tab's document.
    ///
    /// Rasters and GPU textures are keyed by page, scale and rotation only, so
    /// they are invalidated whenever the document changes. Documents without text have no
    /// path: they use the built-in design preview and release the renderer.
    #[cfg(feature = "pdfium")]
    fn refresh_renderer_for_active_tab(&mut self) {
        let id = self.state.active_tab_id();
        let path = self
            .state
            .active_tab()
            .and_then(|tab| tab.path())
            .map(Path::to_path_buf);
        // Edits, undo and redo change the session bytes without changing the
        // path, so the revision is part of the cache key.
        let revision = id
            .and_then(|id| self.sessions.get(&id))
            .map_or(0, DocumentSession::revision_count);
        if path == self.renderer_path && revision == self.renderer_revision {
            return;
        }
        self.renderer_path = path.clone();
        self.renderer_revision = revision;
        self.raster_cancel.cancel();
        self.raster_cancel = CancellationToken::new();
        self.raster_cache.invalidate_all();
        self.textures.clear();
        self.pdfium_renderer = None;
        let loaded = match id.and_then(|id| self.sessions.get(&id)) {
            // Render the session's current bytes so a saved or undone edit is
            // what the canvas shows.
            Some(session) => PdfiumRenderer::from_bytes(session.bytes().to_vec()),
            None => match path {
                Some(path) => PdfiumRenderer::from_file(&path),
                None => return,
            },
        };
        match loaded {
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
                self.render_generation = self.render_generation.wrapping_add(1);
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
                // Passwords are resolved by `DocumentSession` before these
                // bytes reach PDFium, so any failure here is a backend error.
                let message = format!("Could not open this PDF: {error}");
                self.open_error_banner = Some(message.clone());
                self.status = message;
            }
        }
    }

    /// Force the canvas to rebuild from the active session's current bytes.
    ///
    /// Clearing the path defeats the `(path, revision)` early-return, and the
    /// reload drops the raster cache and every GPU texture the canvas holds.
    #[cfg(feature = "pdfium")]
    pub(crate) fn force_canvas_reload(&mut self) {
        self.renderer_path = None;
        self.refresh_renderer_for_active_tab();
    }

    /// Without PDFium the canvas has no document renderer to reload.
    #[cfg(not(feature = "pdfium"))]
    pub(crate) fn force_canvas_reload(&mut self) {}

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
        self.page_box = (page + 1).to_string();
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
            self.page_box = (active + 1).to_string();
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

    /// Apply keyboard shortcuts for the main find, copy, and view commands.
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let (
            open,
            next_tab,
            prev_tab,
            close,
            view_command,
            escape,
            open_find,
            next,
            prev,
            copy,
            save,
            save_as,
            undo,
            redo,
            print,
            assistant_panel,
        ) = ctx.input(|input| {
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
                ctrl && !shift && input.key_pressed(Key::S),
                ctrl && shift && input.key_pressed(Key::S),
                ctrl && !shift && input.key_pressed(Key::Z),
                ctrl && (input.key_pressed(Key::Y) || (shift && input.key_pressed(Key::Z))),
                ctrl && input.key_pressed(Key::P),
                ctrl && shift && input.key_pressed(Key::A),
            )
        });

        if open {
            self.dispatch(AppCommand::OpenDialog, Some(ctx));
        }
        if next_tab {
            self.dispatch(AppCommand::ActivateNextTab, Some(ctx));
        }
        if prev_tab {
            self.dispatch(AppCommand::ActivatePreviousTab, Some(ctx));
        }
        if close {
            self.dispatch(AppCommand::CloseActiveTab, Some(ctx));
        }
        if save {
            self.dispatch(AppCommand::Save, Some(ctx));
        }
        if save_as {
            self.dispatch(AppCommand::SaveAs, Some(ctx));
        }
        if undo {
            self.dispatch(AppCommand::Undo, Some(ctx));
        }
        if redo {
            self.dispatch(AppCommand::Redo, Some(ctx));
        }
        if print {
            self.dispatch(AppCommand::Print, Some(ctx));
        }
        if assistant_panel {
            self.dispatch(AppCommand::AiAssistant, Some(ctx));
        }
        if let Some(command) = view_command {
            let command = match command {
                ViewCommand::RotateClockwise => AppCommand::RotateClockwise,
                ViewCommand::RotateCounterclockwise => AppCommand::RotateCounterclockwise,
                ViewCommand::ResetRotation => AppCommand::ResetRotation,
                ViewCommand::FitPage => AppCommand::FitPage,
                ViewCommand::FitWidth => AppCommand::FitWidth,
                ViewCommand::ActualSize => AppCommand::ActualSize,
                ViewCommand::ZoomIn => AppCommand::ZoomIn,
                ViewCommand::ZoomOut => AppCommand::ZoomOut,
            };
            self.dispatch(command, Some(ctx));
        }
        if open_find {
            self.dispatch(AppCommand::OpenFind, Some(ctx));
        }
        if next {
            self.dispatch(AppCommand::FindNext, Some(ctx));
        }
        if prev {
            self.dispatch(AppCommand::FindPrevious, Some(ctx));
        }
        if copy {
            self.dispatch(AppCommand::CopySelection, Some(ctx));
        }
        if escape {
            if self.assistant_chat.is_thinking() {
                self.dispatch(AppCommand::AssistantStop, Some(ctx));
            } else {
                let command = if self.find_open {
                    AppCommand::CloseFind
                } else {
                    AppCommand::ClearSelection
                };
                self.dispatch(command, Some(ctx));
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
}
impl eframe::App for PdfxiApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_resize_handles(ctx);
        self.handle_keyboard(ctx);
        self.pump_text_extraction();
        self.poll_assistant_connection(ctx);
        self.poll_assistant_chat(ctx);

        let dropped: Vec<PathBuf> = ctx.input(|input| {
            input
                .raw
                .dropped_files
                .iter()
                .filter_map(|file| file.path.clone())
                .collect()
        });
        if !dropped.is_empty() {
            self.dispatch(AppCommand::OpenDropped(dropped), Some(ctx));
        }
        self.open_dialog_ui(ctx);
        self.shell_ui(ctx);
        self.print_ui_ui(ctx);
        self.note_draft_ui(ctx);
        self.forms_panel_ui(ctx);
        self.signing_dialog_ui(ctx);
        self.place_signature_dialog_ui(ctx);
        self.password_dialog_ui(ctx);
        self.close_prompt_ui(ctx);
        self.save_conflict_ui(ctx);
        self.identity_window_ui(ctx);
        self.help_window_ui(ctx);
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

impl PdfxiApp {
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
        // `pdfxi_text` reports a baseline in PDF user space (origin bottom-left).
        // The shell's `OwnedSpan` is top-down, so convert through the real page
        // height here; find overlays and highlight quads share this frame.
        let page_height = renderer
            .page_size(PageIndex::new(page))
            .map(|size| size.height_pt)
            .unwrap_or(792.0);
        let spans = extracted
            .spans()
            .iter()
            .map(|span| {
                OwnedSpan::new(
                    span.text.clone(),
                    span.x,
                    page_height - (span.y + span.height),
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

    /// Width of the left content panel; the assistant needs room for answers.
    fn panel_width(&self) -> f32 {
        if self.active_rail == RailItem::AiAssistant {
            300.0
        } else {
            metrics::PAGES
        }
    }

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

        self.error_banner_ui(ctx, palette);

        egui::TopBottomPanel::top("shell_command")
            .exact_height(metrics::TOOL_BAR)
            .frame(shell_panel_frame(palette, 14))
            .show(ctx, |ui| self.document_bar_ui(ui, palette));

        if self.find_open {
            egui::TopBottomPanel::top("shell_find")
                .frame(shell_panel_frame(palette, 14))
                .show(ctx, |ui| self.find_ui(ui));
        }

        egui::SidePanel::left("shell_rail")
            .exact_width(metrics::RAIL)
            .resizable(false)
            .frame(
                egui::Frame::new()
                    .fill(palette.panel)
                    .inner_margin(egui::Margin {
                        left: 4,
                        right: 4,
                        top: 10,
                        bottom: 8,
                    }),
            )
            .show(ctx, |ui| self.rail_ui(ui, palette));

        egui::SidePanel::left("shell_pages")
            .exact_width(self.panel_width())
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

        if self.tools_open {
            egui::SidePanel::right("shell_tools")
                .exact_width(metrics::TOOLS)
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
                .show(ctx, |ui| self.tools_panel_ui(ui, palette));
        }

        if self.state.active_tab().is_some() {
            egui::TopBottomPanel::bottom("shell_bottom")
                .exact_height(metrics::STATUS_BAR)
                .frame(shell_panel_frame(palette, 12))
                .show(ctx, |ui| self.bottom_bar_ui(ui, palette));
        }

        egui::CentralPanel::default()
            .frame(egui::Frame::new().fill(palette.canvas))
            .show(ctx, |ui| self.canvas_ui(ui));
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

    /// Show or hide the right tools panel and persist the choice.
    fn toggle_tools(&mut self) {
        self.tools_open = !self.tools_open;
        self.persist_shell_preferences();
    }

    /// Write the tools-panel state to the shell preferences file when present.
    fn persist_shell_preferences(&mut self) {
        self.shell_prefs.tools_open = self.tools_open;
        let Some(path) = self.shell_prefs_path.clone() else {
            return;
        };
        if let Err(error) = shell_preferences::save(&path, &self.shell_prefs) {
            self.status = format!("Could not save shell preferences: {error}");
        }
    }

    /// Report a capability-gated action that this build does not implement.
    fn not_available(&mut self, feature: &str) {
        self.status = format!("{feature} is not available in this build");
    }

    /// Apply a custom zoom percentage from the bottom bar.
    fn set_zoom_percent(&mut self, percent: u32) {
        let zoom = Zoom::custom(percent as f32 / 100.0);
        match self.state.set_zoom(zoom) {
            Ok(()) => self.status = format!("Zoom: {zoom}"),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Read the library folder, recompute storage, and cache the listing.
    fn refresh_library(&mut self) {
        self.library_entries.clear();
        self.storage_bytes = 0;
        let Some(root) = self.settings.library_root_or_default() else {
            self.library_loaded = true;
            return;
        };
        let _ = std::fs::create_dir_all(&root);
        if let Ok(entries) = library::list_pdfs(&root) {
            self.library_entries = entries;
        }
        self.library_loaded = true;
        self.storage_bytes = library::storage_used(&root);
        if let Some(shared) = &self.settings.shared_folder {
            self.storage_bytes = self
                .storage_bytes
                .saturating_add(library::storage_used(shared));
        }
    }

    /// Persist a new local display name.
    fn set_display_name(&mut self, name: String) {
        let trimmed = name.trim();
        if trimmed.is_empty() {
            self.status = "Enter a display name".to_owned();
            return;
        }
        self.settings.display_name = trimmed.to_owned();
        self.identity_name = trimmed.to_owned();
        self.persist_settings();
        self.status = "Display name updated".to_owned();
    }

    /// Open the library folder in the platform file manager.
    fn open_storage_folder(&mut self) {
        if let Some(root) = self.settings.library_root_or_default() {
            self.open_in_file_manager(&root);
        }
    }

    /// Reveal a path in the platform file manager.
    fn reveal_path(&mut self, path: &Path) {
        let target = if path.is_dir() {
            path.to_path_buf()
        } else {
            path.parent()
                .map(Path::to_path_buf)
                .unwrap_or_else(|| path.to_path_buf())
        };
        self.open_in_file_manager(&target);
    }

    /// Open the bundled user-guide folder when it exists.
    fn open_docs_folder(&mut self) {
        if let Some(directory) = bundled_docs_dir() {
            self.open_in_file_manager(&directory);
        }
    }

    /// Spawn the platform file manager for `path`.
    fn open_in_file_manager(&mut self, path: &Path) {
        #[cfg(windows)]
        let command = "explorer";
        #[cfg(target_os = "macos")]
        let command = "open";
        #[cfg(all(not(windows), not(target_os = "macos")))]
        let command = "xdg-open";
        match std::process::Command::new(command).arg(path).spawn() {
            Ok(_) => {}
            Err(error) => {
                self.status = format!("Could not open {}: {error}", path.display());
            }
        }
    }

    /// Validate and apply the bottom-bar page number.
    ///
    /// Accepts a one-based page inside the active document; rejects `0`,
    /// non-numbers, and pages past the end without changing the current page.
    /// Returns `true` when the page changed.
    pub fn submit_page_number(&mut self, text: &str) -> bool {
        let Some(tab) = self.state.active_tab() else {
            self.page_box = "1".to_owned();
            return false;
        };
        let count = tab.page_count();
        let current = tab.current_page();
        match text.trim().parse::<usize>() {
            Ok(page) if page >= 1 && page <= count => {
                self.dispatch(AppCommand::GoToPage(page - 1), None);
                self.page_box = page.to_string();
                true
            }
            _ => {
                self.page_box = (current + 1).to_string();
                if count > 0 {
                    self.status = format!("Enter a page between 1 and {count}");
                }
                false
            }
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

    #[test]
    fn every_parameterless_command_is_safe_without_a_document() {
        let mut app = PdfxiApp::new();
        let commands = [
            AppCommand::OpenDialog,
            AppCommand::CloseActiveTab,
            AppCommand::CloseOtherTabs,
            AppCommand::ActivateNextTab,
            AppCommand::ActivatePreviousTab,
            AppCommand::NextPage,
            AppCommand::PreviousPage,
            AppCommand::ZoomIn,
            AppCommand::ZoomOut,
            AppCommand::FitPage,
            AppCommand::FitWidth,
            AppCommand::ActualSize,
            AppCommand::RotateClockwise,
            AppCommand::RotateCounterclockwise,
            AppCommand::ResetRotation,
            AppCommand::ToggleFind,
            AppCommand::OpenFind,
            AppCommand::CloseFind,
            AppCommand::FindNext,
            AppCommand::FindPrevious,
            AppCommand::CopySelection,
            AppCommand::ClearSelection,
            AppCommand::ToggleTheme,
            AppCommand::StartWindowDrag,
            AppCommand::DismissOpenError,
            AppCommand::ClearOpenErrors,
        ];
        for command in commands {
            let label = command.label();
            assert!(!label.is_empty(), "every command needs a stable label");
            app.dispatch(command, None);
        }
        assert_eq!(app.state().tab_count(), 0);
        assert!(app.state().active_tab().is_none());
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn dispatched_commands_match_the_direct_view_calls() {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let path_text = path.to_str().expect("utf-8 fixture path");
        let mut dispatched = PdfxiApp::new();
        let mut direct = PdfxiApp::new();
        assert!(dispatched.open_path_text(path_text));
        assert!(direct.open_path_text(path_text));

        let page = |app: &PdfxiApp| app.state().active_tab().map(|tab| tab.current_page());
        dispatched.dispatch(AppCommand::NextPage, None);
        direct.step_page(1);
        assert_eq!(page(&dispatched), page(&direct));
        dispatched.dispatch(AppCommand::PreviousPage, None);
        direct.step_page(-1);
        assert_eq!(page(&dispatched), page(&direct));

        dispatched.dispatch(AppCommand::ZoomIn, None);
        direct.zoom_in();
        assert_eq!(
            dispatched.state().active_tab().map(|tab| tab.zoom()),
            direct.state().active_tab().map(|tab| tab.zoom())
        );

        dispatched.dispatch(AppCommand::RotateClockwise, None);
        direct.rotate_clockwise();
        assert_eq!(
            dispatched.state().active_tab().map(|tab| tab.rotation()),
            direct.state().active_tab().map(|tab| tab.rotation())
        );
    }

    #[test]
    fn capability_registry_gates_command_bar_commands() {
        let labels = |caps: Capabilities| -> Vec<&'static str> {
            shell::capabilities::command_bar_features(caps)
                .into_iter()
                .map(|button| button.command.label())
                .collect()
        };
        let hidden = labels(Capabilities::none());
        for command in [
            "edit-document",
            "comment",
            "highlight",
            "sign",
            "organize",
            "export",
            "ai-assistant",
        ] {
            assert!(!hidden.contains(&command), "{command} must be hidden");
        }
        let comments = labels(Capabilities::none().with(Capability::Comments));
        assert!(!comments.contains(&"edit-document"));
        assert!(comments.contains(&"comment"));
        assert!(comments.contains(&"highlight"));
        let edit = labels(Capabilities::none().with(Capability::Edit));
        assert!(edit.contains(&"edit-document"));
        let signing = labels(Capabilities::none().with(Capability::Signing));
        assert!(signing.contains(&"sign"));
        let export = labels(Capabilities::none().with(Capability::Export));
        assert!(export.contains(&"organize"));
        assert!(export.contains(&"export"));
        let ai = labels(Capabilities::none().with(Capability::AssistantConnect));
        assert!(
            ai.contains(&"ai-assistant"),
            "the assistant pill is reachable before a service exists"
        );
        let conversation = labels(Capabilities::none().with(Capability::AiAssistant));
        assert!(
            !conversation.contains(&"ai-assistant"),
            "the conversation capability does not own the placement; the connect entry does"
        );
    }

    #[test]
    fn tools_search_filters_enabled_rows_by_substring() {
        let all = Capabilities::none()
            .with(Capability::Comments)
            .with(Capability::Forms)
            .with(Capability::Export)
            .with(Capability::Protect);
        let rows = shell::capabilities::tool_rows_for(all, "sign");
        assert!(!rows.is_empty());
        assert!(
            rows.iter()
                .all(|row| row.label.to_lowercase().contains("sign"))
        );
        assert!(rows.iter().any(|row| row.label == "Fill & Sign"));
        assert!(shell::capabilities::tool_rows_for(all, "zzz").is_empty());
        let available = shell::capabilities::tool_rows_for(Capabilities::available(), "");
        assert!(available.iter().any(|row| row.label == "Comment"));
        assert!(
            !available.iter().any(|row| row.label == "Edit PDF"),
            "Edit PDF stays gated behind its own capability"
        );
    }

    #[test]
    fn page_number_box_validates_without_changing_the_page() {
        let mut app = app_with_pages(12);
        assert!(app.submit_page_number("3"));
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 2);
        assert!(!app.submit_page_number("0"));
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 2);
        assert!(!app.submit_page_number("abc"));
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 2);
        assert!(!app.submit_page_number("13"));
        assert_eq!(app.state().active_tab().expect("tab").current_page(), 2);
    }

    #[test]
    fn bottom_bar_zoom_choices_apply_through_dispatch() {
        let mut app = app_with_pages(3);
        app.dispatch(AppCommand::FitWidth, None);
        assert_eq!(
            app.state().active_tab().map(|tab| tab.zoom()),
            Some(Zoom::FitWidth)
        );
        app.dispatch(AppCommand::ActualSize, None);
        assert_eq!(
            app.state().active_tab().map(|tab| tab.zoom()),
            Some(Zoom::custom(DEFAULT_ZOOM))
        );
    }

    #[test]
    fn tools_panel_state_round_trips_through_shell_settings() {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir =
            std::env::temp_dir().join(format!("pdfxi-shell-prefs-{}-{unique}", std::process::id()));
        let path = dir.join("shell.toml");
        let mut app = PdfxiApp::new();
        app.shell_prefs_path = Some(path.clone());
        let before = app.tools_open;
        app.dispatch(AppCommand::ToggleTools, None);
        assert_eq!(app.tools_open, !before);
        let saved = shell::preferences::load(&path);
        assert_eq!(saved.tools_open, app.tools_open);
        app.dispatch(AppCommand::ToggleTools, None);
        assert_eq!(app.tools_open, before);
        let saved = shell::preferences::load(&path);
        assert_eq!(saved.tools_open, app.tools_open);
        assert!(shell::preferences::ShellPreferences::default().tools_open);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rail_entries_follow_their_capabilities() {
        let available = shell::capabilities::rail_items(Capabilities::available());
        assert!(available.contains(&RailItem::Files));
        assert!(available.contains(&RailItem::Starred));
        assert!(!available.contains(&RailItem::Shared));
        let with_shared =
            shell::capabilities::rail_items(Capabilities::available().with(Capability::Shared));
        assert!(with_shared.contains(&RailItem::Shared));
    }

    #[test]
    fn avatar_initials_use_up_to_two_words() {
        assert_eq!(library::initials("Ada Lovelace"), "AL");
        assert_eq!(library::initials("prince"), "P");
        assert_eq!(library::initials("   "), "P");
        // A fresh install uses the neutral author, not the OS account name.
        assert_eq!(settings::default_display_name(), "Reader");
        let app = PdfxiApp::new();
        assert_eq!(app.display_name(), "Reader");
        assert_eq!(app.initials(), "R");
    }

    #[test]
    fn help_window_opens_through_a_command() {
        let mut app = PdfxiApp::new();
        assert!(!app.help_open());
        app.dispatch(AppCommand::OpenHelp, None);
        assert!(app.help_open());
        app.dispatch(AppCommand::CloseHelp, None);
        assert!(!app.help_open());
    }
}
