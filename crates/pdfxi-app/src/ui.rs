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
//! [`crate::selection::SelectionModel`] in the status line. An in-document find
//! bar drives [`crate::find::FindState`]: matches are highlighted on the page
//! rectangles, the view scrolls to the current match, and Ctrl+C copies the
//! current selection through egui's clipboard API.
//!
//! # Raster integration
//!
//! With the `pdfium` feature enabled, the demo viewer opens
//! `fixtures/minimal.pdf` (resolved from the workspace root via
//! `demo_fixture_path`) through `PdfiumRenderer`. Visible pages are
//! rasterized into [the
//! bounded raster cache](crate::raster::RasterCache), and the active raster is
//! uploaded into an `egui` texture that is painted inside the page rectangle.
//! Without `pdfium`, no native library is loaded and the canvas keeps drawing
//! the outlined placeholder pages. All layout, selection, and cache logic lives
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
//! shell saves on switch-away and restores on switch-back. A fixed placeholder
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
//! command goes through [`AppState`] and returns immediately — no UI action
//! performs blocking I/O.
//!
//! # Optional content layers
//!
//! A right-side Layers panel lists the active tab's optional content groups in
//! deterministic order, with checkboxes for ordinary layers, radio buttons for
//! radio groups, disabled rows for locked groups, and show-all / hide-all
//! controls. State is isolated per tab by [`crate::LayerRegistry`]. Panel actions
//! call window-free `PdfxiApp` methods that only mutate in-memory state, so the
//! UI never blocks. The visibility plan these methods expose is a boundary for a
//! renderer backend; actual optional-content rasterization depends on backend
//! support that `pdfxi-render` does not yet provide.
//!
//! # Bookmarks, attachments, and links panels
//!
//! Three right-side panels list the active tab's outline bookmarks, embedded-file
//! attachments, and per-page links. They are driven by the window-free
//! [`crate::BookmarkRegistry`], [`crate::AttachmentRegistry`], and
//! [`crate::LinkRegistry`] models: expand/collapse, selection, and link hit
//! testing are pure in-memory operations exposed as `PdfxiApp` helpers and
//! unit tested without a GUI. Activating an attachment builds an
//! [`crate::AttachmentOpenRequest`] that a host with a stream backend must
//! fulfil; the shell itself never reads embedded file bytes.
//!
//! # Split and tile panes
//!
//! A toolbar row and keyboard shortcuts drive the window-free
//! [`crate::split::PaneManager`]: horizontal/vertical splits, `1x1`..`2x2` tile
//! grids, pane focus, and close-pane actions. Each pane keeps its own scroll,
//! zoom, and page (or mirrors its tab under `MirrorTab` sharing), and panes are
//! logical rectangles inside this single central canvas — the shell never
//! creates native multi-window OS handles. Split/tile helpers on
//! [`crate::ui::PdfxiApp`] only mutate in-memory state and return immediately, so they are exercised by
//! window-free unit tests and never perform blocking I/O.

use std::path::{Path, PathBuf};

use crate::{
    OpenError, PaperSize, PrintContext, PrintDialogState, PrintModelError, PrintPageRange,
    PrintPlan,
};
use egui::Key;
use pdfxi_render::{Duplex, Orientation, PageScaleMode};

use crate::{
    AppState, ArticleThread, AttachmentInfo, AttachmentOpenRequest, AttachmentRegistry,
    AttachmentState, AttachmentSummary, BookmarkInfo, BookmarkRegistry, BookmarkSummary, CanvasHit,
    CanvasLayout, CanvasPoint, CanvasRect, CanvasSize, CloseDecision, DEFAULT_ZOOM, Destination,
    DocumentTab, FindMatch, FindOptions, FindState, Fit, LayerRegistry, LinkId, LinkInfo,
    LinkPoint, LinkRect, LinkRegistry, LinkSummary, LinkTarget, OcPropertiesSummary, OcgInfo,
    OpenOutcome, OwnedSpan, OwnedTextPage, PageSize, PaneCommand, PaneKey, PaneManager, PaneRect,
    RadioGroup, ResolvedLink, SelectionModel, ShortcutKey, ShortcutModifiers, TabId,
    ThumbnailPaneState, Toolset, ViewCommand, ViewMode, ViewSharing, VisibilityPlan, Zoom,
    app_name, map_pane_shortcut, map_shortcut,
};

#[cfg(feature = "pdfium")]
use crate::Rotation;
#[cfg(feature = "pdfium")]
use crate::raster::{RasterCache, RasterKey, demo_fixture_path};
#[cfg(feature = "pdfium")]
use crate::uv_quad_for_rotation;
#[cfg(feature = "pdfium")]
use pdfxi_render::{CancellationToken, PdfiumRenderer, Renderer, thumbnail::ThumbnailSpec};

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

/// Height of one row in the thumbnail side panel, in UI points.
const THUMB_ROW_HEIGHT: f32 = 140.0;

/// Maximum side (width and height) of a generated thumbnail, in pixels.
#[cfg(feature = "pdfium")]
const THUMB_MAX_SIDE: u32 = 120;

/// Width of a thumbnail tile in the side panel, in UI points.
const THUMB_TILE_WIDTH: f32 = 96.0;

/// Height of a thumbnail tile in the side panel, in UI points.
const THUMB_TILE_HEIGHT: f32 = 124.0;

/// Maximum number of thumbnails generated in a single frame.
///
/// Bounding per-frame work keeps scrolling responsive even for long documents.
#[cfg(feature = "pdfium")]
const THUMBS_PER_FRAME: usize = 4;

/// Placeholder page count assigned to a demo tab (no real document is loaded).
pub const DEMO_PAGE_COUNT: usize = 3;

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

/// Build a deterministic demo `/OCProperties` summary for the shell.
///
/// The demo document has ordinary layers (Background, Annotations, Grid) plus a
/// radio-button group (English / French labels). It lets the Layers panel and
/// its radio/locked/visibility logic be exercised without loading a real PDF.
/// A production loader would feed the caller's parsed summary through
/// [`LayerRegistry::install`] instead.
pub fn demo_oc_properties() -> OcPropertiesSummary {
    OcPropertiesSummary::new()
        .add_group(OcgInfo::new("oc-bg", "Background", true, false))
        .add_group(OcgInfo::new("oc-anno", "Annotations", true, false))
        .add_group(OcgInfo::new("oc-grid", "Grid", false, false))
        .add_group(OcgInfo::new("oc-en", "English labels", true, false))
        .add_group(OcgInfo::new("oc-fr", "French labels", false, false))
        .with_radio_group(RadioGroup::new(["oc-en", "oc-fr"]))
}

/// Build a deterministic demo outline tree for the shell.
///
/// It contains a root, two nested chapters (one expanded by default), and a
/// top-level appendix, plus one article thread. A production loader would feed
/// the caller's parsed summary through [`BookmarkRegistry::install`] instead.
pub fn demo_bookmarks(page_count: usize) -> BookmarkSummary {
    let last = page_count.saturating_sub(1);
    BookmarkSummary::new()
        .add_bookmark(
            BookmarkInfo::new("bm-root", "Contents", Destination::new(0, Fit::Fit)).with_open(true),
        )
        .add_bookmark(
            BookmarkInfo::new(
                "bm-intro",
                "Introduction",
                Destination::new(0, Fit::FitH(Some(720.0))),
            )
            .with_parent("bm-root")
            .with_open(true),
        )
        .add_bookmark(
            BookmarkInfo::new(
                "bm-body",
                "Body",
                Destination::new(
                    last,
                    Fit::Xyz {
                        left: Some(0.0),
                        top: None,
                        zoom: None,
                    },
                ),
            )
            .with_parent("bm-root"),
        )
        .add_bookmark(BookmarkInfo::new(
            "bm-refs",
            "References",
            Destination::new(last, Fit::FitB),
        ))
        .add_article(ArticleThread::new(
            "art-main",
            "Demo article",
            (0..=last).collect::<Vec<_>>(),
        ))
}

/// Build a deterministic demo embedded-file list for the shell.
pub fn demo_attachments(page_count: usize) -> AttachmentSummary {
    let last = page_count.saturating_sub(1);
    AttachmentSummary::new()
        .add(AttachmentInfo::new(
            "notes.txt",
            "Editor notes",
            256,
            Some(last),
        ))
        .add(AttachmentInfo::new("data.csv", "", 4096, Some(0)))
        .add(AttachmentInfo::new(
            "supplement.pdf",
            "Supplement",
            8192,
            None,
        ))
}

/// Build a deterministic demo link set for the shell.
///
/// One URI link and one internal link live on page 0; a named link lives on the
/// last page and resolves through the registered `section.intro` name.
pub fn demo_links(page_count: usize) -> LinkSummary {
    let last = page_count.saturating_sub(1);
    LinkSummary::new()
        .add_link(LinkInfo::new(
            "lnk-web",
            0,
            LinkRect::new(72.0, 700.0, 120.0, 16.0),
            LinkTarget::Uri("https://example.test".into()),
        ))
        .add_link(LinkInfo::new(
            "lnk-jump",
            0,
            LinkRect::new(220.0, 700.0, 120.0, 16.0),
            LinkTarget::Internal { page: last },
        ))
        .add_link(LinkInfo::new(
            "lnk-named",
            last,
            LinkRect::new(72.0, 100.0, 160.0, 16.0),
            LinkTarget::Named("section.intro".into()),
        ))
        .add_name("section.intro", 0)
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
    Some(CanvasLayout::standard_rotated(
        tab.view_mode(),
        tab.zoom(),
        viewport,
        &demo_page_sizes(tab.page_count()),
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

/// Translate an `egui` key into the toolkit-independent [`PaneKey`].
///
/// Only keys used by [`map_pane_shortcut`] are mapped; other keys return `None`
/// so the find field and other widgets keep their normal handling.
fn native_pane_key(key: Key) -> Option<PaneKey> {
    match key {
        Key::H => Some(PaneKey::H),
        Key::V => Some(PaneKey::V),
        Key::Num1 => Some(PaneKey::One),
        Key::Num2 => Some(PaneKey::Two),
        Key::Num3 => Some(PaneKey::Three),
        Key::Num4 => Some(PaneKey::Four),
        Key::Period => Some(PaneKey::Period),
        Key::Comma => Some(PaneKey::Comma),
        Key::W => Some(PaneKey::W),
        Key::P => Some(PaneKey::P),
        _ => None,
    }
}

/// A stable, human-readable label for a [`crate::PaneLayout`].
///
/// Pure and window-free so it can be asserted by UI helper tests.
pub fn pane_layout_label(layout: crate::PaneLayout) -> String {
    use crate::PaneLayout;
    match layout {
        PaneLayout::Single => "Single".to_string(),
        PaneLayout::Split(split) => match split.direction {
            crate::SplitDirection::Horizontal => {
                format!("Split H {}", (split.ratio * 100.0).round() as i32)
            }
            crate::SplitDirection::Vertical => {
                format!("Split V {}", (split.ratio * 100.0).round() as i32)
            }
        },
        PaneLayout::Tile(grid) => format!("Tile {}x{}", grid.rows, grid.cols),
    }
}

/// A short, human-readable label for a link target.
///
/// Pure and window-free so the Links panel and unit tests share one formatting
/// path. Opening a URI is always the host's responsibility.
pub fn link_target_label(target: &LinkTarget) -> String {
    match target {
        LinkTarget::Internal { page } => format!("Page {}", page + 1),
        LinkTarget::Uri(uri) => format!("URI {uri}"),
        LinkTarget::Named(name) => format!("Named {name}"),
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

/// Build find input pairs from the demo viewer's selectable text pages.
///
/// The canvas, selection copy, and find bar all read from the same
/// [`OwnedTextPage`] snapshot, so a highlighted match corresponds exactly to
/// text the user can select and copy. Each page's spans are concatenated in
/// reading order to form the page's searchable text.
///
/// The viewer has no text-extraction backend of its own: `pdfxi-render` exposes
/// only rasters (no `text_page` method), and adding a direct `pdfxi-core` or
/// `pdfxi-text` dependency would break the deliberately dependency-light
/// default build and touch the shared lockfile. The demo therefore searches the
/// synthetic text it actually displays; a production loader can feed real
/// extracted text through [`FindState::set_pages`] instead.
pub fn demo_find_pages(pages: &[OwnedTextPage]) -> Vec<(usize, String)> {
    pages
        .iter()
        .map(|page| {
            let mut text = String::new();
            for span in page.spans() {
                text.push_str(&span.text);
            }
            (page.page_number(), text)
        })
        .collect()
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
        Box::new(|_creation_context| Ok(Box::new(PdfxiApp::new()))),
    ) {
        eprintln!("PDFXI viewer exited with an error: {error}");
    }
}

/// Paint a cached page texture into `rect`, rotating the texture UVs so the
/// raster appears turned rather than stretched into a swapped page rectangle.
///
/// The four UV corners come from [`uv_quad_for_rotation`]; they are assigned to
/// the displayed rectangle's corners in `(top-left, top-right, bottom-right,
/// bottom-left)` order and drawn as two triangles. This is a pure GPU-side
/// transform — no re-render or blocking I/O is involved.
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
/// status header, and a placeholder canvas. Every toolbar button and keyboard
/// shortcut maps to a method on this type that mutates only the underlying
/// state, keeping behavior testable without a window.
#[derive(Default)]
pub struct PdfxiApp {
    state: AppState,
    status: String,
    /// Unsaved-change policy consulted by every tab-close action.
    close_policy: UiClosePolicy,
    /// Window-free print-dialog state; submitting never contacts a printer.
    print_dialog: PrintDialogState,
    /// Typed errors surfaced by recent document-open attempts.
    open_errors: Vec<OpenError>,
    /// Whether the in-app "Open PDF" path dialog is visible.
    open_dialog_open: bool,
    /// Path typed into the "Open PDF" dialog.
    open_dialog_path: String,
    /// Zero-based pages used by the Selection print range.
    print_selection: Vec<usize>,
    scroll_x: f32,
    scroll_y: f32,
    selection: SelectionModel,
    demo_text: Vec<OwnedTextPage>,
    /// In-document find model, fed from `demo_text` when a tab opens.
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
    /// Per-tab optional content layer models and viewing states.
    layers: LayerRegistry,
    /// Per-tab outline bookmark trees and their expand/selection state.
    bookmarks: BookmarkRegistry,
    /// Per-tab embedded-file lists and their selection state.
    attachments: AttachmentRegistry,
    /// Per-tab link annotations, index, and named destinations.
    links: LinkRegistry,
    /// Most recent host attachment-open request (a metadata boundary).
    last_open_request: Option<AttachmentOpenRequest>,
    /// Logical split/tile panes inside the single central canvas.
    panes: PaneManager,
    /// GPU textures for generated thumbnails, keyed by page.
    #[cfg(feature = "pdfium")]
    thumb_textures: std::collections::HashMap<usize, egui::TextureHandle>,
    /// Bounded cache of real page rasters, filled from the demo fixture.
    #[cfg(feature = "pdfium")]
    raster_cache: RasterCache,
    /// The native renderer; `None` when the fixture could not be opened.
    #[cfg(feature = "pdfium")]
    pdfium_renderer: Option<PdfiumRenderer>,
    /// One GPU texture per laid-out page, refreshed on zoom/rotation changes.
    #[cfg(feature = "pdfium")]
    textures: std::collections::HashMap<usize, LiveTexture>,
}

impl PdfxiApp {
    /// Create a viewer with no open documents.
    ///
    /// With the `pdfium` feature this also opens the demo fixture from
    /// `fixtures/minimal.pdf`. A load failure is reported through [`Self::status`]
    /// and leaves the placeholder canvas in place rather than panicking.
    pub fn new() -> Self {
        #[cfg(feature = "pdfium")]
        {
            let mut app = Self::default();
            match demo_fixture_path("minimal.pdf") {
                Some(path) => match PdfiumRenderer::from_file(&path) {
                    Ok(renderer) => {
                        app.status = format!("Loaded demo fixture {}", path.display());
                        app.pdfium_renderer = Some(renderer);
                    }
                    Err(error) => {
                        app.status = format!("Could not open demo fixture: {error}");
                    }
                },
                None => {
                    app.status =
                        "No demo fixture in this layout; use Open… or drop a PDF".to_owned();
                }
            }
            app
        }
        #[cfg(not(feature = "pdfium"))]
        {
            Self::default()
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

    /// Open a new demo tab, make it active, and return its id.
    ///
    /// With `pdfium`, the tab's page count comes from the real fixture so the
    /// canvas never asks the renderer for a page the document does not have.
    pub fn open_demo_tab(&mut self) -> TabId {
        // Preserve the outgoing tab's view before a new tab becomes active.
        self.save_scroll_to_active();
        let name = format!("Demo Document {}", self.state.tab_count() + 1);
        let id = self.state.open_tab(name);
        self.finish_new_tab(id);
        self.status = format!("Opened demo tab {id}");
        id
    }

    /// Initialize the shared demo helpers for a newly opened tab.
    ///
    /// The shell has one find/selection/thumbnail helper set driven by the
    /// active tab's page count; this resets it for the new document and starts
    /// that document's view at the top. Per-tab view state (page, zoom, layout,
    /// rotation, and scroll) lives on the [`DocumentTab`] itself.
    fn finish_new_tab(&mut self, id: TabId) {
        let page_count = self.demo_page_count();
        if let Some(tab) = self.state.tab_mut(id) {
            tab.set_page_count(page_count);
        }
        // A newly focused document starts with one pane bound to it. Any prior
        // split/tile arrangement is intentionally discarded on tab open so the
        // pane set always starts from a deterministic, single-document state.
        self.panes = PaneManager::single(id);
        self.demo_text = demo_text_pages(page_count);
        // Search, selection, and copy all read the same displayed text snapshot.
        self.find.set_pages(demo_find_pages(&self.demo_text));
        self.selection.clear();
        self.scroll_x = 0.0;
        self.scroll_y = 0.0;
        self.pending_scroll = false;
        self.pending_page_scroll = None;
        self.thumbs.reset(page_count);
        self.thumb_follow_active = true;
        // Each tab gets its own optional content state, isolated by tab id.
        // The demo summary is caller-controlled and always valid; a real loader
        // would install its parsed summary through the same registry method.
        let _ = self.layers.install(id, demo_oc_properties());
        // The demo outline/attachments/links are caller-controlled and always
        // valid; a real loader would install parsed data through the registries.
        let _ = self
            .bookmarks
            .install(id, demo_bookmarks(page_count), page_count);
        let _ = self
            .attachments
            .install(id, demo_attachments(page_count), page_count);
        let _ = self.links.install(id, demo_links(page_count), page_count);
        self.last_open_request = None;
        #[cfg(feature = "pdfium")]
        self.thumb_textures.clear();
    }

    /// The page count used for demo tabs.
    #[cfg(feature = "pdfium")]
    fn demo_page_count(&self) -> usize {
        self.pdfium_renderer
            .as_ref()
            .map_or(DEMO_PAGE_COUNT, |renderer| {
                renderer.page_count().unwrap_or(DEMO_PAGE_COUNT)
            })
    }

    /// The page count used for demo tabs (fixed placeholder without PDFium).
    #[cfg(not(feature = "pdfium"))]
    fn demo_page_count(&self) -> usize {
        DEMO_PAGE_COUNT
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
                self.layers.remove(id);
                self.bookmarks.remove(id);
                self.attachments.remove(id);
                self.links.remove(id);
                // Tab close deterministically removes every pane bound to it;
                // pane-local view state is discarded, never written back.
                let _ = self.panes.close_tab(id);
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
    /// A newly opened document gets the same demo helpers as [`Self::open_demo_tab`];
    /// a reused one simply restores its saved view and becomes active.
    pub fn open_document(&mut self, path: &Path) -> OpenOutcome {
        self.save_scroll_to_active();
        let outcome = self.state.open_document(path);
        match outcome {
            OpenOutcome::Opened { id } => {
                self.finish_new_tab(id);
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
            self.status = "Enter a PDF path to open".to_owned();
            return false;
        }
        if trimmed.chars().any(char::is_control) {
            self.status = "Path contains control characters".to_owned();
            return false;
        }
        let path = Path::new(trimmed);
        if !Self::is_pdf_path(path) {
            self.status = format!("Not a PDF file: {}", path.display());
            return false;
        }
        if !path.is_file() {
            self.status = format!("File not found: {}", path.display());
            return false;
        }
        self.open_document(path);
        true
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
    fn open_dialog_ui(&mut self, ctx: &egui::Context) {
        if !self.open_dialog_open {
            return;
        }
        let mut visible = self.open_dialog_open;
        let mut submit = false;
        let mut cancel = false;
        egui::Window::new("Open PDF")
            .collapsible(false)
            .resizable(false)
            .open(&mut visible)
            .show(ctx, |ui| {
                ui.label("PDF path:");
                let response = ui.text_edit_singleline(&mut self.open_dialog_path);
                if response.lost_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter)) {
                    submit = true;
                }
                ui.horizontal(|ui| {
                    if ui.button("Open").clicked() {
                        submit = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
                ui.label("Tip: drag a PDF from Explorer onto the window.");
            });
        if cancel {
            visible = false;
        }
        if submit {
            let typed = self.open_dialog_path.clone();
            if self.open_path_text(&typed) {
                visible = false;
            }
        }
        self.open_dialog_open = visible;
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
    }

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

    /// Borrow the window-free split/tile pane manager.
    pub fn pane_manager(&self) -> &PaneManager {
        &self.panes
    }

    /// Apply a typed [`PaneCommand`] and report the outcome through [`Self::status`].
    ///
    /// This is the single implementation used by both the toolbar buttons and
    /// the keyboard path. It only mutates in-memory pane state, so it never
    /// blocks and is fully testable without a window.
    pub fn apply_pane_command(&mut self, command: PaneCommand) {
        let label = command.shortcut_label();
        match self.panes.apply_command(command) {
            Ok(()) => {
                self.status = format!(
                    "Panes: {} | {} pane(s) | focus {}",
                    pane_layout_label(self.panes.layout()),
                    self.panes.pane_count(),
                    self.panes.active_index(),
                );
            }
            Err(error) => self.status = format!("Pane command failed ({label}): {error}"),
        }
    }

    /// Focus a pane by index, reporting an invalid focus through the status.
    pub fn focus_pane(&mut self, index: usize) {
        match self.panes.focus_pane(index) {
            Ok(()) => self.status = format!("Focused pane {index}"),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Set the focused pane's scroll offset through the sharing rule.
    pub fn set_pane_scroll(&mut self, offset: CanvasPoint) {
        match self.panes.set_scroll(offset) {
            Ok(()) => self.status = "Pane scroll updated".to_string(),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Set the focused pane's zoom through the sharing rule.
    pub fn set_pane_zoom(&mut self, zoom: Zoom) {
        match self.panes.set_zoom(zoom) {
            Ok(()) => self.status = format!("Pane zoom: {zoom}"),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Set the focused pane's page through the sharing rule.
    pub fn set_pane_page(&mut self, page: usize) {
        match self.panes.set_page(page) {
            Ok(()) => self.status = format!("Pane page: {}", page + 1),
            Err(error) => self.status = error.to_string(),
        }
    }

    /// Set the pane view-sharing rule.
    pub fn set_view_sharing(&mut self, sharing: ViewSharing) {
        self.panes.set_sharing(sharing);
        self.status = format!("Pane sharing: {sharing:?}");
    }

    /// Resolve the current pane layout against a viewport, or `None` when the
    /// viewport is too small for the layout's dividers and minimum sizes.
    ///
    /// This is a pure helper: no OS window is created or measured.
    pub fn pane_geometry(&self, width: u32, height: u32) -> Option<Vec<PaneRect>> {
        self.panes.geometry(PaneRect::new(0, 0, width, height)).ok()
    }

    /// A short textual description of the active pane and layout.
    pub fn pane_status(&self) -> String {
        format!(
            "{} | {} pane(s) | focus {} | sharing {:?}",
            pane_layout_label(self.panes.layout()),
            self.panes.pane_count(),
            self.panes.active_index(),
            self.panes.sharing(),
        )
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
                let (rect, response) = ui.allocate_exact_size(tile_size, egui::Sense::click());
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
                    let full_uv =
                        egui::Rect::from_min_max(egui::pos2(0.0, 0.0), egui::pos2(1.0, 1.0));
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
    fn ensure_rasters(&mut self, layout: &CanvasLayout) {
        let Some(renderer) = self.pdfium_renderer.as_ref() else {
            return;
        };
        let scroll = CanvasPoint::new(self.scroll_x, self.scroll_y);
        let token = CancellationToken::new();
        match self
            .raster_cache
            .ensure_visible(renderer, layout, scroll, &token, &mut |_| {})
        {
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

    /// Install the demo layer set on the active tab.
    ///
    /// Returns `false` with a status message when no tab is active.
    pub fn install_demo_layers(&mut self) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.layers.install(tab, demo_oc_properties()) {
            Ok(()) => {
                self.status = format!("Installed demo layers for tab {tab}");
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Toggle an optional content layer on the active tab.
    ///
    /// All work is in-memory and returns immediately — the UI never blocks on
    /// I/O for a layer change. Locked or unknown ids report a typed error
    /// through [`Self::status`] and return `false`.
    pub fn toggle_layer(&mut self, id: &str) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.layers.toggle(tab, id) {
            Ok(visible) => {
                self.status = format!("Layer {id} {}", if visible { "shown" } else { "hidden" });
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Explicitly show or hide a layer on the active tab.
    pub fn set_layer_visible(&mut self, id: &str, visible: bool) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.layers.set_visible(tab, id, visible) {
            Ok(value) => {
                self.status = format!("Layer {id} {}", if value { "shown" } else { "hidden" });
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Show all unlocked layers on the active tab, preserving radio exclusivity.
    pub fn show_all_layers(&mut self) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.layers.show_all(tab) {
            Ok(()) => {
                self.status = "All layers shown".to_string();
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Hide all unlocked layers on the active tab.
    pub fn hide_all_layers(&mut self) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.layers.hide_all(tab) {
            Ok(()) => {
                self.status = "All layers hidden".to_string();
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// The visibility plan for the active tab, if it has layers installed.
    pub fn layer_visibility_plan(&self) -> Option<VisibilityPlan> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.layers.visibility_plan(tab))
    }

    /// Borrow the per-tab layer registry (for consumers and tests).
    pub fn layers(&self) -> &LayerRegistry {
        &self.layers
    }

    /// Render the optional content Layers side panel.
    ///
    /// The panel lists the active tab's layers in deterministic display order.
    /// Ordinary layers use checkboxes; radio-group members use radio buttons so
    /// the UI mirrors the model's exclusivity. Locked layers are rendered
    /// disabled. Show all / hide all drive the same window-free model methods as
    /// any other command. Row data is snapshotted before rendering so no
    /// immutable model borrow is held while a click mutates state.
    fn layers_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.state.active_tab_id() else {
            ui.label("No document open");
            return;
        };

        // Snapshot (id, name, locked, radio group, visible) rows up front.
        let rows: Vec<(String, String, bool, Option<usize>, bool)> =
            match (self.layers.model(tab), self.layers.state(tab)) {
                (Some(model), Some(state)) => model
                    .layers()
                    .map(|layer| {
                        let id = layer.id().to_string();
                        let visible = state.is_visible(model, id.as_str()).unwrap_or(false);
                        (
                            id,
                            layer.name().to_string(),
                            layer.is_locked(),
                            layer.radio_group(),
                            visible,
                        )
                    })
                    .collect(),
                _ => {
                    ui.label("No optional content layers");
                    return;
                }
            };

        ui.heading("Layers");
        ui.horizontal(|ui| {
            if ui.button("Show all").clicked() {
                let _ = self.show_all_layers();
            }
            if ui.button("Hide all").clicked() {
                let _ = self.hide_all_layers();
            }
        });
        ui.separator();

        // Derive the selected radio member per group from the current snapshot.
        let mut selected_by_group: std::collections::HashMap<usize, String> =
            std::collections::HashMap::new();
        for (id, _, _, radio_group, visible) in &rows {
            if let Some(group) = radio_group
                && *visible
            {
                selected_by_group.insert(*group, id.clone());
            }
        }

        for (id, name, locked, radio_group, visible) in rows {
            ui.horizontal(|ui| match radio_group {
                Some(group) => {
                    if locked {
                        ui.add_enabled(false, egui::RadioButton::new(visible, &name));
                    } else {
                        let selected = selected_by_group.entry(group).or_default();
                        let response = ui.radio_value(selected, id.clone(), name);
                        if response.clicked() {
                            let _ = self.set_layer_visible(id.as_str(), true);
                        }
                    }
                }
                None => {
                    let mut checked = visible;
                    if locked {
                        ui.add_enabled(false, egui::Checkbox::new(&mut checked, &name));
                    } else if ui.checkbox(&mut checked, &name).changed() {
                        let _ = self.set_layer_visible(id.as_str(), checked);
                    }
                }
            });
        }
    }

    /// Reinstall the demo outline/attachment/link data on the active tab.
    ///
    /// Returns `false` with a status message when no document is open. A test
    /// helper and a recovery path for a tab whose navigation data was cleared.
    pub fn install_demo_navigation_data(&mut self) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        let page_count = self.demo_page_count();
        // Each error type is handled in its own arm because the three models
        // deliberately expose distinct typed errors.
        if let Err(error) = self
            .bookmarks
            .install(tab, demo_bookmarks(page_count), page_count)
        {
            self.status = error.to_string();
            return false;
        }
        if let Err(error) = self
            .attachments
            .install(tab, demo_attachments(page_count), page_count)
        {
            self.status = error.to_string();
            return false;
        }
        if let Err(error) = self.links.install(tab, demo_links(page_count), page_count) {
            self.status = error.to_string();
            return false;
        }
        self.status = format!("Installed demo navigation data for tab {tab}");
        true
    }

    /// Borrow the per-tab bookmark registry.
    pub fn bookmarks(&self) -> &BookmarkRegistry {
        &self.bookmarks
    }

    /// Borrow the per-tab attachment registry.
    pub fn attachments(&self) -> &AttachmentRegistry {
        &self.attachments
    }

    /// Borrow the per-tab link registry.
    pub fn links(&self) -> &LinkRegistry {
        &self.links
    }

    /// Visible bookmark rows for the active tab, in deterministic order.
    pub fn bookmark_rows(&self) -> Vec<crate::VisibleBookmark> {
        self.state
            .active_tab_id()
            .map_or_else(Vec::new, |tab| self.bookmarks.visible_rows(tab))
    }

    /// Article-thread summary for the active tab: `(id, title, page count)` rows.
    pub fn article_summary(&self) -> Vec<(String, String, usize)> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.bookmarks.model(tab))
            .map(|model| {
                model
                    .articles()
                    .map(|article| {
                        (
                            article.id().to_string(),
                            article.title().to_string(),
                            article.pages().len(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Toggle a bookmark's expanded flag on the active tab.
    ///
    /// Returns `true` when the action succeeded (use
    /// [`Self::bookmark_rows`] or the registry state to observe the new
    /// expansion value); reports unknown ids and missing tabs through
    /// [`Self::status`].
    pub fn toggle_bookmark_expansion(&mut self, id: &str) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.bookmarks.toggle_expanded(tab, id) {
            Ok(expanded) => {
                self.status = format!(
                    "Bookmark {id} {}",
                    if expanded { "expanded" } else { "collapsed" }
                );
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Select a bookmark on the active tab.
    pub fn select_bookmark(&mut self, id: &str) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.bookmarks.select(tab, id) {
            Ok(()) => {
                self.status = format!("Selected bookmark {id}");
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Attachment rows for the active tab: `(name, description, size, page)`.
    pub fn attachment_rows(&self) -> Vec<(String, String, u64, Option<usize>)> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.attachments.model(tab))
            .map(|model| {
                model
                    .attachments()
                    .map(|attachment| {
                        (
                            attachment.name().to_string(),
                            attachment.description().to_string(),
                            attachment.size(),
                            attachment.page(),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Select an attachment on the active tab.
    pub fn select_attachment(&mut self, name: &str) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.attachments.select(tab, name) {
            Ok(()) => {
                self.status = format!("Selected attachment {name}");
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// Request that the host open an attachment on the active tab.
    ///
    /// This is the host open boundary: the app records the request and its
    /// status but never reads embedded file bytes or touches the filesystem. A
    /// host with a stream backend consumes [`Self::last_open_request`] and
    /// performs extraction/opening. Returns `false` when there is no active tab
    /// or the attachment is unknown.
    pub fn open_attachment(&mut self, name: &str) -> bool {
        let Some(tab) = self.state.active_tab_id() else {
            self.status = "No document open".to_string();
            return false;
        };
        match self.attachments.open_request(tab, name) {
            Ok(request) => {
                self.status = format!("Open request for attachment {name} (host must fulfil)");
                self.last_open_request = Some(request);
                true
            }
            Err(error) => {
                self.status = error.to_string();
                false
            }
        }
    }

    /// The most recent host open request produced by [`Self::open_attachment`].
    pub fn last_open_request(&self) -> Option<&AttachmentOpenRequest> {
        self.last_open_request.as_ref()
    }

    /// Link rows on `page` of the active tab: `(id, page, target label)`.
    pub fn link_rows(&self, page: usize) -> Vec<(LinkId, usize, String)> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.links.model(tab))
            .map(|model| {
                model
                    .links_for_page(page)
                    .map(|link| {
                        (
                            link.id().to_string(),
                            link.page(),
                            link_target_label(link.target()),
                        )
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Resolve a link on the active tab to its destination, if identifiable.
    pub fn resolve_link(&self, id: &str) -> Option<ResolvedLink> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.links.model(tab))
            .and_then(|model| model.resolve(id))
    }

    /// Hit-test a page-space point against the active tab's links.
    ///
    /// Returns the topmost link id at `(x, y)` on `page`, or `None`.
    pub fn hit_test_link(&self, page: usize, x: f32, y: f32) -> Option<String> {
        self.state
            .active_tab_id()
            .and_then(|tab| self.links.hit_test(tab, page, LinkPoint::new(x, y)))
            .map(str::to_string)
    }

    /// Render the Bookmarks side panel for the active tab.
    fn bookmarks_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.state.active_tab_id() else {
            ui.label("No document open");
            return;
        };
        let rows = self.bookmarks.visible_rows(tab);
        if rows.is_empty() {
            ui.heading("Bookmarks");
            ui.label("No bookmarks in this document");
            return;
        }

        ui.heading("Bookmarks");
        for row in &rows {
            let indent = (row.depth() * 12) as f32;
            ui.horizontal(|ui| {
                ui.add_space(indent);
                let marker = if row.has_children() {
                    if row.expanded() { "▾" } else { "▸" }
                } else {
                    "·"
                };
                let mut label = format!("{marker} {} (p. {})", row.title(), row.page() + 1);
                if row.selected() {
                    label.push_str(" ◀");
                }
                if ui.button(label).clicked() {
                    let id = row.id().to_string();
                    if row.has_children() {
                        let _ = self.toggle_bookmark_expansion(&id);
                    }
                    let _ = self.select_bookmark(&id);
                }
            });
        }

        let articles = self.article_summary();
        if !articles.is_empty() {
            ui.separator();
            ui.label("Articles");
            for (id, title, pages) in articles {
                ui.label(format!("{title} [{id}] — {pages} page(s)"));
            }
        }
    }

    /// Render the Attachments side panel for the active tab.
    fn attachments_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.state.active_tab_id() else {
            ui.label("No document open");
            return;
        };
        let rows = self.attachment_rows();
        ui.heading("Attachments");
        if rows.is_empty() {
            ui.label("No embedded attachments");
            return;
        }

        let selected = self
            .attachments
            .state(tab)
            .and_then(AttachmentState::selected)
            .map(str::to_string);
        for (name, description, size, page) in rows {
            let label = if description.is_empty() {
                format!("{name} ({size} B)")
            } else {
                format!("{name} — {description} ({size} B)")
            };
            let page_hint = page.map_or("page n/a".to_string(), |page| format!("p. {}", page + 1));
            ui.horizontal(|ui| {
                let mut text = label;
                if selected.as_deref() == Some(name.as_str()) {
                    text.push_str(" ◀");
                }
                if ui.button(text).clicked() {
                    let _ = self.select_attachment(&name);
                }
                ui.label(page_hint);
            });
        }
        ui.separator();
        ui.label("Opening bytes is delegated to the host (metadata only here).");
        if let Some(request) = self.last_open_request() {
            ui.label(format!("Requested: {}", request.name()));
        }
    }

    /// Render the Links side panel for the active tab's current page.
    fn links_ui(&mut self, ui: &mut egui::Ui) {
        let Some(tab) = self.state.active_tab_id() else {
            ui.label("No document open");
            return;
        };
        let page = self
            .state
            .active_tab()
            .map_or(0, |active| active.current_page());
        let rows = self.link_rows(page);
        ui.heading(format!("Links — page {}", page + 1));
        if rows.is_empty() {
            ui.label("No links on this page");
            return;
        }
        for (id, link_page, target) in rows {
            let resolved = self
                .links
                .model(tab)
                .and_then(|model| model.resolve(&id))
                .map_or(String::new(), |target| format!(" → {target:?}"));
            ui.label(format!("{id} (p. {}) {target}{resolved}", link_page + 1));
        }
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

    /// Render the toolbar panel.
    fn toolbar_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal(|ui| {
            ui.heading(app_name());
            ui.separator();
            if ui.button("Open…").clicked() {
                self.open_dialog_open = true;
            }
            if ui.button("Open Demo").clicked() {
                self.open_demo_tab();
            }
            if ui.button("Close").clicked() {
                self.close_active_tab();
            }
            if ui.button("Print...").clicked() {
                self.open_print_dialog();
            }
            ui.separator();
            if ui.button("Zoom -").clicked() {
                self.zoom_out();
            }
            if ui.button("Zoom +").clicked() {
                self.zoom_in();
            }
            if ui
                .button("Rotate CCW")
                .on_hover_text(ViewCommand::RotateCounterclockwise.shortcut_label())
                .clicked()
            {
                self.rotate_counterclockwise();
            }
            if ui
                .button("Rotate CW")
                .on_hover_text(ViewCommand::RotateClockwise.shortcut_label())
                .clicked()
            {
                self.rotate_clockwise();
            }
            if ui
                .button("Reset rotation")
                .on_hover_text(ViewCommand::ResetRotation.shortcut_label())
                .clicked()
            {
                self.reset_rotation();
            }
        });

        ui.horizontal(|ui| {
            ui.label("Display:");
            if ui
                .button("Fit page")
                .on_hover_text(ViewCommand::FitPage.shortcut_label())
                .clicked()
            {
                self.fit_page();
            }
            if ui
                .button("Fit width")
                .on_hover_text(ViewCommand::FitWidth.shortcut_label())
                .clicked()
            {
                self.fit_width();
            }
            if ui
                .button("Actual size")
                .on_hover_text(ViewCommand::ActualSize.shortcut_label())
                .clicked()
            {
                self.actual_size();
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

        ui.horizontal(|ui| {
            ui.label("Panes:");
            for (label, command) in [
                ("Split H", PaneCommand::SplitHorizontal),
                ("Split V", PaneCommand::SplitVertical),
                ("1x1", PaneCommand::Tile1x1),
                ("1x2", PaneCommand::Tile1x2),
                ("2x1", PaneCommand::Tile2x1),
                ("2x2", PaneCommand::Tile2x2),
            ] {
                if ui
                    .button(label)
                    .on_hover_text(command.shortcut_label())
                    .clicked()
                {
                    self.apply_pane_command(command);
                }
            }
            ui.separator();
            if ui
                .button("Next pane")
                .on_hover_text(PaneCommand::FocusNextPane.shortcut_label())
                .clicked()
            {
                self.apply_pane_command(PaneCommand::FocusNextPane);
            }
            if ui
                .button("Close pane")
                .on_hover_text(PaneCommand::ClosePane.shortcut_label())
                .clicked()
            {
                self.apply_pane_command(PaneCommand::ClosePane);
            }
            ui.label(self.pane_status());
        });
    }

    /// Render the document tab strip.
    ///
    /// Labels, order, and the active highlight come from the window-free
    /// [`Self::tab_strip_snapshot`]. Each tab also gets a small close button;
    /// close requests are deferred until after the `egui` closure so the
    /// workspace is never mutated while the UI borrows it. With no open tab the
    /// strip shows a fixed placeholder.
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
                if ui.small_button("×").clicked() {
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
        let selected = self.selection.selected_text(&self.demo_text);
        if !selected.is_empty() {
            ui.label(format!("Selected: {selected}"));
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

                // Fill the LRU cache with real rasters before painting. This
                // happens only with PDFium; otherwise the placeholder stands.
                #[cfg(feature = "pdfium")]
                self.ensure_rasters(&layout);

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

                #[cfg(feature = "pdfium")]
                let ctx = ui.ctx().clone();
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
                    #[cfg(feature = "pdfium")]
                    if let Some(texture) =
                        self.raster_texture(&ctx, page.page, page.scale, page.rotation)
                    {
                        // Paint the real page raster inside the rotated page
                        // rectangle, rotating its UVs to match the view.
                        paint_rotated_image(painter, texture.id(), rect, page.rotation);
                        continue;
                    }
                    // Without a cached raster (or without PDFium), draw the
                    // outlined placeholder rectangle.
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
                    let Some(local) = match_rect(find_hit, &self.demo_text) else {
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
                            origin.x + mapped.x - self.scroll_x,
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

                let copy_text = self.selection.selected_text(&self.demo_text);
                ui.label(canvas_info_line(hit, &copy_text));
            });
    }

    /// Apply keyboard shortcuts for the main find, copy, and view commands.
    fn handle_keyboard(&mut self, ctx: &egui::Context) {
        let (
            open,
            next_tab,
            prev_tab,
            close,
            view_command,
            pane_command,
            escape,
            open_find,
            next,
            prev,
            copy,
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
            let pane_command = (ctrl && shift)
                .then(|| {
                    [
                        Key::H,
                        Key::V,
                        Key::Num1,
                        Key::Num2,
                        Key::Num3,
                        Key::Num4,
                        Key::Period,
                        Key::Comma,
                        Key::W,
                        Key::P,
                    ]
                    .into_iter()
                    .find(|key| input.key_pressed(*key))
                    .and_then(native_pane_key)
                })
                .flatten()
                .and_then(|key| {
                    map_pane_shortcut(
                        key,
                        ShortcutModifiers {
                            ctrl,
                            shift,
                            alt: input.modifiers.alt,
                        },
                    )
                });
            (
                ctrl && input.key_pressed(Key::N),
                ctrl && !shift && input.key_pressed(Key::Tab),
                ctrl && shift && input.key_pressed(Key::Tab),
                ctrl && !shift && input.key_pressed(Key::W),
                view_command,
                pane_command,
                input.key_pressed(Key::Escape),
                ctrl && input.key_pressed(Key::F),
                !shift && input.key_pressed(Key::F3),
                shift && input.key_pressed(Key::F3),
                ctrl && input.key_pressed(Key::C),
            )
        });

        if open {
            self.open_demo_tab();
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
        if let Some(command) = pane_command {
            self.apply_pane_command(command);
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
    /// Borrow the window-free print-dialog model.
    pub fn print_dialog(&self) -> &PrintDialogState {
        &self.print_dialog
    }

    /// Show the print dialog.
    pub fn open_print_dialog(&mut self) {
        self.print_dialog.show();
    }

    /// Hide the print dialog.
    pub fn close_print_dialog(&mut self) {
        self.print_dialog.hide();
    }

    /// Select which pages the print job includes.
    pub fn set_print_range(&mut self, range: PrintPageRange) {
        self.print_dialog.model_mut().range = range;
    }

    /// Set the number of copies.
    pub fn set_print_copies(&mut self, copies: u32) {
        self.print_dialog.model_mut().copies = copies;
    }

    /// Set the duplex mode recorded for the host adapter.
    pub fn set_print_duplex(&mut self, duplex: Duplex) {
        self.print_dialog.model_mut().duplex = duplex;
    }

    /// Set portrait or landscape orientation.
    pub fn set_print_orientation(&mut self, orientation: Orientation) {
        self.print_dialog.model_mut().orientation = orientation;
    }

    /// Set fit, actual-size, or custom-percent scaling.
    pub fn set_print_scale(&mut self, scale: PageScaleMode) {
        self.print_dialog.model_mut().scale = scale;
    }

    /// Set the paper size used for fitting and recorded in the plan.
    pub fn set_print_paper(&mut self, paper: PaperSize) {
        self.print_dialog.model_mut().paper = paper;
    }

    /// Replace the zero-based pages used by the `Selection` print range.
    pub fn set_print_selected_pages(&mut self, pages: Vec<usize>) {
        self.print_selection = pages;
    }

    /// Build a print context from the active tab's page count and current page.
    pub fn print_context(&self) -> PrintContext {
        let (page_count, current_page) = self
            .state
            .active_tab()
            .map(|tab| (tab.page_count(), tab.current_page()))
            .unwrap_or((0, 0));
        PrintContext {
            page_count,
            current_page,
            selected_pages: self.print_selection.clone(),
        }
    }

    /// Submit the dialog's model without contacting a printer.
    ///
    /// This returns a data-only [`PrintPlan`]; a host adapter must spool it
    /// through `pdfxi_render::print` and forward the rasters to a driver.
    ///
    /// # Errors
    ///
    /// Propagates every [`PrintModelError`] from dialog submission.
    pub fn submit_print(&mut self) -> Result<PrintPlan, PrintModelError> {
        let context = self.print_context();
        self.print_dialog.submit(&context)
    }

    /// Record a typed document-open error for display in the shell.
    pub fn record_open_error(&mut self, error: OpenError) {
        self.open_errors.push(error);
    }

    /// Borrow the typed open errors recorded so far.
    pub fn open_errors(&self) -> &[OpenError] {
        &self.open_errors
    }

    /// Forget all recorded open errors.
    pub fn clear_open_errors(&mut self) {
        self.open_errors.clear();
    }

    fn print_ui(&mut self, ui: &mut egui::Ui) {
        ui.label("Submit builds a plan only; a host adapter performs printing.");
        {
            let model = self.print_dialog.model_mut();
            ui.horizontal(|ui| {
                ui.label("Pages:");
                ui.radio_value(&mut model.range, PrintPageRange::All, "All");
                ui.radio_value(&mut model.range, PrintPageRange::Current, "Current");
                ui.radio_value(&mut model.range, PrintPageRange::Selection, "Selection");
            });
            let mut custom = match &model.range {
                PrintPageRange::Custom(text) => text.clone(),
                _ => String::new(),
            };
            ui.horizontal(|ui| {
                ui.label("Custom:");
                if ui.text_edit_singleline(&mut custom).changed() && !custom.is_empty() {
                    model.range = PrintPageRange::Custom(custom.clone());
                }
            });
            ui.add(
                egui::DragValue::new(&mut model.copies)
                    .range(1..=999)
                    .prefix("Copies: "),
            );
            ui.horizontal(|ui| {
                ui.label("Orientation:");
                ui.radio_value(&mut model.orientation, Orientation::Portrait, "Portrait");
                ui.radio_value(&mut model.orientation, Orientation::Landscape, "Landscape");
            });
            ui.horizontal(|ui| {
                ui.label("Scale:");
                ui.radio_value(&mut model.scale, PageScaleMode::Actual, "Actual");
                ui.radio_value(&mut model.scale, PageScaleMode::Fit, "Fit");
            });
            let mut percent = match model.scale {
                PageScaleMode::CustomPercent(value) => value,
                _ => 100.0,
            };
            ui.horizontal(|ui| {
                ui.label("Custom %:");
                if ui
                    .add(egui::DragValue::new(&mut percent).range(1.0..=1000.0))
                    .changed()
                {
                    model.scale = PageScaleMode::CustomPercent(percent);
                }
            });
            ui.horizontal(|ui| {
                ui.label("Duplex:");
                ui.radio_value(&mut model.duplex, Duplex::None, "None");
                ui.radio_value(&mut model.duplex, Duplex::LongEdge, "Long edge");
                ui.radio_value(&mut model.duplex, Duplex::ShortEdge, "Short edge");
            });
            ui.horizontal(|ui| {
                ui.label("Paper:");
                ui.radio_value(&mut model.paper, PaperSize::Letter, "Letter");
                ui.radio_value(&mut model.paper, PaperSize::A4, "A4");
                ui.radio_value(&mut model.paper, PaperSize::Legal, "Legal");
            });
        }
        if ui.button("Submit (build plan)").clicked() {
            let _ = self.submit_print();
        }
        let status = self.print_dialog.status().to_owned();
        if !status.is_empty() {
            ui.label(status);
        }
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
        self.handle_keyboard(ctx);

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

        egui::SidePanel::left("pdfxi_thumbnails")
            .resizable(true)
            .default_width(156.0)
            .show(ctx, |ui| self.thumbnails_ui(ui));
        egui::SidePanel::right("pdfxi_layers")
            .resizable(true)
            .default_width(168.0)
            .show(ctx, |ui| self.layers_ui(ui));
        egui::SidePanel::right("pdfxi_bookmarks")
            .resizable(true)
            .default_width(168.0)
            .show(ctx, |ui| self.bookmarks_ui(ui));
        egui::SidePanel::right("pdfxi_attachments")
            .resizable(true)
            .default_width(168.0)
            .show(ctx, |ui| self.attachments_ui(ui));
        egui::SidePanel::right("pdfxi_links")
            .resizable(true)
            .default_width(150.0)
            .show(ctx, |ui| self.links_ui(ui));
        egui::TopBottomPanel::top("pdfxi_toolbar").show(ctx, |ui| self.toolbar_ui(ui));
        egui::TopBottomPanel::top("pdfxi_tabs").show(ctx, |ui| self.tabs_ui(ui));
        if self.find_open {
            egui::TopBottomPanel::top("pdfxi_find").show(ctx, |ui| self.find_ui(ui));
        }
        egui::CentralPanel::default().show(ctx, |ui| self.canvas_ui(ui));
        if self.print_dialog.is_open() {
            let mut open = true;
            egui::Window::new("Print")
                .open(&mut open)
                .show(ctx, |ui| self.print_ui(ui));
            if !open {
                self.print_dialog.hide();
            }
        }
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Rotation, TextAnchor};

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
        // With PDFium the count comes from the real fixture (one page);
        // without it the fixed placeholder count is used.
        let expected = app.demo_page_count();
        assert_eq!(
            app.state().tab(second).map(|tab| tab.page_count()),
            Some(expected)
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
    fn open_path_text_rejects_empty_control_and_non_pdf() {
        let mut app = PdfxiApp::new();
        assert!(!app.open_path_text("   "));
        assert_eq!(app.status(), "Enter a PDF path to open");

        assert!(!app.open_path_text("bad\u{0}path.pdf"));
        assert_eq!(app.status(), "Path contains control characters");

        assert!(!app.open_path_text("C:\\docs\\notes.txt"));
        assert!(app.status().starts_with("Not a PDF file"));

        assert!(!app.open_path_text("C:\\definitely\\missing\\file.pdf"));
        assert!(app.status().starts_with("File not found"));
        assert_eq!(app.state().tab_count(), 0);
    }

    #[test]
    fn open_path_text_opens_an_existing_pdf_and_focuses_duplicates() {
        let base = std::env::temp_dir().join("pdfxi-open-path-test");
        std::fs::create_dir_all(&base).expect("create temp dir");
        let pdf = base.join("sample.pdf");
        std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").expect("write pdf");
        let pdf_with_quotes = format!("\"{}\"", pdf.display());

        let mut app = PdfxiApp::new();
        assert!(app.open_path_text(&pdf_with_quotes));
        assert_eq!(app.state().tab_count(), 1);
        // A second open of the same path focuses rather than duplicates.
        assert!(app.open_path_text(&pdf.display().to_string()));
        assert_eq!(app.state().tab_count(), 1);

        std::fs::remove_dir_all(&base).expect("clean temp dir");
    }

    #[test]
    fn open_dropped_opens_only_pdfs_and_reports_the_batch() {
        let base = std::env::temp_dir().join("pdfxi-open-dropped-test");
        std::fs::create_dir_all(&base).expect("create temp dir");
        let pdf = base.join("drop.PDF");
        std::fs::write(&pdf, b"%PDF-1.4\n%%EOF\n").expect("write pdf");
        let txt = base.join("drop.txt");
        std::fs::write(&txt, b"not a pdf").expect("write txt");

        let mut app = PdfxiApp::new();
        let opened = app.open_dropped(&[pdf.clone(), txt]);
        assert_eq!(opened, 1);
        assert_eq!(app.state().tab_count(), 1);
        assert!(app.status().contains("Opened 1 dropped file"));

        let mut empty = PdfxiApp::new();
        assert_eq!(empty.open_dropped(&[]), 0);
        assert_eq!(empty.open_dropped(&[base.join("missing.pdf")]), 0);
        assert!(empty.status().starts_with("File not found"));

        std::fs::remove_dir_all(&base).expect("clean temp dir");
    }

    #[test]
    fn is_pdf_path_is_case_insensitive_and_extension_based() {
        assert!(PdfxiApp::is_pdf_path(Path::new("a.pdf")));
        assert!(PdfxiApp::is_pdf_path(Path::new("A.PDF")));
        assert!(PdfxiApp::is_pdf_path(Path::new("dir/with space/doc.Pdf")));
        assert!(!PdfxiApp::is_pdf_path(Path::new("a.txt")));
        assert!(!PdfxiApp::is_pdf_path(Path::new("pdf")));
        assert!(!PdfxiApp::is_pdf_path(Path::new("a.pdf.txt")));
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
    fn reset_rotation_and_page_display_controls_update_active_tab() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        app.rotate_clockwise();
        app.rotate_clockwise();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg180)
        );
        app.reset_rotation();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg0)
        );
        assert!(app.status().contains("reset"), "{}", app.status());

        app.fit_page();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.zoom()),
            Some(Zoom::FitPage)
        );
        app.fit_width();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.zoom()),
            Some(Zoom::FitWidth)
        );
        app.actual_size();
        assert_eq!(
            app.state().tab(id).map(|tab| tab.zoom()),
            Some(Zoom::custom(1.0))
        );
        assert!(app.status().contains("100%"), "{}", app.status());

        // With no active tab the commands report through status, never panic.
        app.close_active_tab();
        app.reset_rotation();
        app.fit_page();
        assert_eq!(app.status(), "no tab is currently active");
    }

    #[test]
    fn apply_view_command_dispatches_rotation_zoom_and_fit() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        app.apply_view_command(ViewCommand::RotateClockwise);
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg90)
        );
        app.apply_view_command(ViewCommand::RotateCounterclockwise);
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg0)
        );
        app.apply_view_command(ViewCommand::ResetRotation);
        assert_eq!(
            app.state().tab(id).map(|tab| tab.rotation()),
            Some(Rotation::Deg0)
        );
        app.apply_view_command(ViewCommand::FitWidth);
        assert_eq!(
            app.state().tab(id).map(|tab| tab.zoom()),
            Some(Zoom::FitWidth)
        );
        app.apply_view_command(ViewCommand::ZoomIn);
        // A fit mode has no explicit scale; zoom-in resolves to 100% + step.
        assert_eq!(
            app.state().tab(id).and_then(|tab| tab.zoom().scale()),
            Some(1.25)
        );
    }

    #[test]
    fn layout_for_state_applies_per_tab_view_rotation() {
        let mut state = AppState::new();
        let id = state.open_tab("rotated");
        if let Some(tab) = state.tab_mut(id) {
            tab.set_page_count(1);
            tab.set_zoom(Zoom::custom(1.0));
        }
        let flat = layout_for_state(&state, CanvasSize::new(1000.0, 1000.0))
            .expect("a layout is produced");
        assert_eq!(flat.rotation(), Rotation::Deg0);
        let flat_width = flat.page(0).map(|page| page.rect.width).unwrap_or(0.0);

        state.set_rotation(Rotation::Deg90).expect("active tab");
        let turned = layout_for_state(&state, CanvasSize::new(1000.0, 1000.0))
            .expect("a layout is produced");
        assert_eq!(turned.rotation(), Rotation::Deg90);
        let turned_width = turned.page(0).map(|page| page.rect.width).unwrap_or(0.0);
        // The demo page is portrait; a quarter turn makes it landscape.
        assert!(turned_width > flat_width);
    }

    #[test]
    fn demo_page_effective_rotation_matches_view_rotation() {
        // The demo shell has no intrinsic page /Rotate, so a page's effective
        // rotation equals the per-tab view rotation used for the raster key.
        // This documents why producer and consumer keys stay in sync.
        let mut state = AppState::new();
        let id = state.open_tab("demo");
        if let Some(tab) = state.tab_mut(id) {
            tab.set_page_count(1);
        }
        for view in [
            Rotation::Deg0,
            Rotation::Deg90,
            Rotation::Deg180,
            Rotation::Deg270,
        ] {
            state.set_rotation(view).expect("active tab");
            let layout = layout_for_state(&state, CanvasSize::new(800.0, 600.0))
                .expect("a layout is produced");
            assert_eq!(layout.rotation(), view);
            for page in layout.pages() {
                assert_eq!(page.page_rotation, Rotation::Deg0);
                assert_eq!(page.rotation, view);
            }
        }
    }

    #[test]
    fn status_indicator_reports_rotation_and_zoom_together() {
        let mut state = AppState::new();
        state.open_tab("report.pdf");
        state.set_rotation(Rotation::Deg270).expect("active tab");
        state.set_zoom(Zoom::FitWidth).expect("active tab");
        let status = active_tab_status(&state);
        assert!(status.contains("270 deg"), "{status}");
        assert!(status.contains("Fit Width"), "{status}");
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

    #[test]
    fn demo_find_pages_builds_searchable_text_from_spans() {
        let pages = demo_text_pages(3);
        let pairs = demo_find_pages(&pages);
        assert_eq!(
            pairs,
            vec![
                (0, "Page 1 demo text".to_string()),
                (1, "Page 2 demo text".to_string()),
                (2, "Page 3 demo text".to_string()),
            ]
        );
    }

    #[test]
    fn match_rect_maps_character_range_to_span_geometry() {
        let pages = [OwnedTextPage::new(
            0,
            vec![
                OwnedSpan::new("Hello", 0.0, 10.0, 50.0, 10.0),
                OwnedSpan::new("world", 60.0, 10.0, 50.0, 10.0),
            ],
        )];
        // Concatenated "Helloworld": 3..7 is "lowo", crossing both spans.
        let hit = FindMatch::new(0, 3, 7, "lowo");
        let rect = match_rect(&hit, &pages).expect("a matching page with spans");
        assert!((rect.x - 30.0).abs() < 0.001, "{}", rect.x);
        assert!((rect.y - 10.0).abs() < 0.001, "{}", rect.y);
        assert!((rect.width - 50.0).abs() < 0.001, "{}", rect.width);
        assert!((rect.height - 10.0).abs() < 0.001, "{}", rect.height);
        assert_eq!(match_rect(&FindMatch::new(0, 2, 2, ""), &pages), None);
        assert_eq!(match_rect(&FindMatch::new(9, 0, 1, "x"), &pages), None);
    }

    #[test]
    fn scroll_offset_for_page_centers_and_clamps() {
        let sizes = vec![PageSize::new(612.0, 792.0); 3];
        let layout = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &sizes,
        );
        let geometry = layout.page(1).expect("page 1 exists");
        let expected = geometry.rect.y - (600.0 - geometry.rect.height) * 0.5;
        let offset = scroll_offset_for_page(&layout, 1);
        assert!((offset - expected).abs() < 0.001, "{offset}");
        assert!(offset >= 0.0);
        assert_eq!(scroll_offset_for_page(&layout, 999), 0.0);
    }

    #[test]
    fn find_bar_query_and_navigation_update_the_model() {
        let mut app = PdfxiApp::new();
        app.open_demo_tab();
        assert!(!app.is_find_open());
        app.open_find();
        assert!(app.is_find_open());

        app.set_find_query("demo");
        assert_eq!(app.find_state().match_count(), app.demo_text.len());
        assert_eq!(
            app.find_state().match_summary(),
            format!("1 of {}", app.demo_text.len())
        );

        app.find_next();
        assert!(app.pending_scroll, "navigation requests a scroll");
        app.find_previous();
        app.set_find_options(true, false);
        assert!(app.find_state().options().case_sensitive);
        app.close_find();
        assert!(!app.is_find_open());
    }

    #[test]
    fn find_and_selection_share_the_same_demo_text() {
        let pages = demo_text_pages(2);
        let mut finder = FindState::new();
        finder.set_pages(demo_find_pages(&pages));
        finder.set_query("demo");
        assert_eq!(finder.match_count(), 2);
        let matched = finder.current().expect("a first match").text();

        let mut selection = SelectionModel::new();
        selection.drag(TextAnchor::new(0, 7), TextAnchor::new(0, 11));
        assert_eq!(selection.selected_text(&pages), "demo");
        assert_eq!(selection.selected_text(&pages), matched);
    }

    #[test]
    fn thumbnail_click_marks_active_and_requests_canvas_scroll() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        let count = app.thumbnail_state().page_count();
        assert!(count > 0, "a demo tab has at least one page");
        let target = count.saturating_sub(1);
        assert!(app.select_thumbnail_page(target));
        assert_eq!(app.thumbnail_state().active_page(), target);
        assert_eq!(app.thumbnail_state().selected_page(), Some(target));
        assert_eq!(app.pending_page_scroll, Some(target));
        assert_eq!(
            app.state().tab(id).map(|tab| tab.current_page()),
            Some(target)
        );
        assert!(
            !app.select_thumbnail_page(count),
            "an out-of-range click is ignored"
        );
    }

    #[test]
    fn thumbnail_active_page_follows_canvas_scroll() {
        // Use a five-page pane independent of the demo fixture's page count
        // (the PDFium fixture has only one page).
        let mut app = PdfxiApp::default();
        app.thumbs.reset(5);
        let sizes = vec![PageSize::new(DEMO_PAGE_WIDTH_PT, DEMO_PAGE_HEIGHT_PT); 5];
        let layout = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &sizes,
        );
        let second_top = layout.page(1).map(|page| page.rect.y).unwrap_or(0.0);
        app.scroll_y = second_top;
        assert!(app.sync_active_page_from_canvas(&layout));
        assert_eq!(app.thumbnail_state().active_page(), 1);
        assert!(
            !app.sync_active_page_from_canvas(&layout),
            "an unchanged frame is a no-op"
        );
        app.scroll_y = 0.0;
        assert!(app.sync_active_page_from_canvas(&layout));
        assert_eq!(app.thumbnail_state().active_page(), 0);
    }

    #[test]
    fn thumbnail_pane_model_exposes_visible_range_without_a_window() {
        let mut app = PdfxiApp::new();
        app.open_demo_tab();
        let count = app.thumbnail_state().page_count();
        let mut state = crate::ThumbnailPaneState::new(count);
        state.set_row_height(THUMB_ROW_HEIGHT);
        state.set_viewport_height(600.0);
        assert_eq!(state.visible_range().len(), count);
        assert_eq!(state.active_page(), 0);
        assert_eq!(state.metadata().len(), count);
    }

    #[test]
    fn active_page_from_canvas_scroll_helper_is_deterministic() {
        let sizes = vec![PageSize::new(612.0, 792.0); 4];
        let layout = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &sizes,
        );
        assert_eq!(active_page_from_canvas_scroll(&layout, 0.0), 0);
        let third_top = layout.page(2).map(|page| page.rect.y).unwrap_or(0.0);
        assert_eq!(active_page_from_canvas_scroll(&layout, third_top), 2);
        let empty = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &[],
        );
        assert_eq!(active_page_from_canvas_scroll(&empty, 10.0), 0);
    }

    #[test]
    fn active_page_from_canvas_scroll_picks_the_page_at_the_centre() {
        let sizes = vec![PageSize::new(612.0, 792.0); 3];
        let layout = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &sizes,
        );
        assert_eq!(active_page_from_canvas_scroll(&layout, 0.0), 0);
        assert_eq!(active_page_from_canvas_scroll(&layout, 900.0), 1);
        assert_eq!(active_page_from_canvas_scroll(&layout, 1800.0), 2);
        // Beyond the last page the closest preceding page wins.
        assert_eq!(active_page_from_canvas_scroll(&layout, 99_999.0), 2);
    }

    #[test]
    fn thumbnail_click_marks_page_active_and_requests_canvas_scroll() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        if let Some(tab) = app.state.tab_mut(id) {
            tab.set_page_count(5);
        }
        app.thumbs.reset(5);

        assert!(app.select_thumbnail_page(3));
        assert_eq!(app.thumbnail_state().active_page(), 3);
        assert_eq!(app.thumbnail_state().selected_page(), Some(3));
        assert_eq!(app.pending_page_scroll, Some(3));
        assert_eq!(app.state.tab(id).map(|tab| tab.current_page()), Some(3));
        assert!(app.status().contains("Page 4"));

        // A click outside the document is ignored and leaves state intact.
        assert!(!app.select_thumbnail_page(9));
        assert_eq!(app.thumbnail_state().active_page(), 3);
        assert_eq!(app.pending_page_scroll, Some(3));
    }

    #[test]
    fn active_page_follows_canvas_scrolling() {
        let mut app = PdfxiApp::new();
        let id = app.open_demo_tab();
        if let Some(tab) = app.state.tab_mut(id) {
            tab.set_page_count(6);
        }
        app.thumbs.reset(6);

        let sizes = vec![PageSize::new(612.0, 792.0); 6];
        let layout = CanvasLayout::standard(
            ViewMode::Continuous,
            Zoom::custom(1.0),
            CanvasSize::new(800.0, 600.0),
            &sizes,
        );

        app.scroll_y = 0.0;
        let _ = app.sync_active_page_from_canvas(&layout);
        assert_eq!(app.thumbnail_state().active_page(), 0);

        app.scroll_y = 900.0;
        assert!(app.sync_active_page_from_canvas(&layout));
        assert_eq!(app.thumbnail_state().active_page(), 1);
        assert!(app.thumb_follow_active, "a change asks the pane to follow");

        // An unchanged frame leaves the active page and follow flag settled.
        assert!(!app.sync_active_page_from_canvas(&layout));
    }

    #[cfg(feature = "pdfium")]
    #[test]
    fn demo_fixture_fills_the_cache_without_a_window() {
        let mut app = PdfxiApp::new();
        assert!(app.pdfium_renderer.is_some(), "{}", app.status());
        let id = app.open_demo_tab();
        // Minimal.pdf has exactly one page.
        assert_eq!(app.state().tab(id).map(|tab| tab.page_count()), Some(1));

        let layout = layout_for_state(app.state(), CanvasSize::new(800.0, 600.0))
            .expect("the demo tab produces a layout");
        let effective = layout
            .page(0)
            .map(|page| page.rotation)
            .expect("the page has geometry");
        app.ensure_rasters(&layout);
        assert_eq!(app.raster_cache.len(), 1, "the visible page was rasterized");
        assert!(
            app.raster_cache
                .contains(RasterKey::new(0, layout.scale(), effective)),
            "the producer keys by the same effective rotation used for painting"
        );
    }

    // ---- T-0069 tabbed-interface UI helpers -----------------------------

    #[test]
    fn tab_strip_snapshot_reflects_labels_order_and_active_highlight() {
        let mut app = PdfxiApp::new();
        assert!(app.tab_strip_snapshot().is_empty());

        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        let snapshot = app.tab_strip_snapshot();
        assert_eq!(snapshot.len(), 2);
        assert_eq!(snapshot[0].0, a);
        assert!(snapshot[0].1.contains("Demo Document 1"));
        assert!(!snapshot[0].2);
        assert_eq!(snapshot[1].0, b);
        assert!(snapshot[1].1.contains("Demo Document 2"));
        assert!(snapshot[1].2, "the newest tab is active");

        app.activate_tab(a);
        let snapshot = app.tab_strip_snapshot();
        assert!(snapshot[0].2);
        assert!(!snapshot[1].2);
    }

    #[test]
    fn keyboard_tab_helpers_wrap_and_close() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        let c = app.open_demo_tab();
        assert_eq!(app.state().active_tab_id(), Some(c));

        assert!(app.activate_next_tab(), "Ctrl+Tab wraps to the first tab");
        assert_eq!(app.state().active_tab_id(), Some(a));
        assert!(
            app.activate_previous_tab(),
            "Ctrl+Shift+Tab wraps to the last tab"
        );
        assert_eq!(app.state().active_tab_id(), Some(c));

        app.close_active_tab();
        assert_eq!(app.state().active_tab_id(), Some(b));
        app.close_active_tab();
        app.close_active_tab();
        assert!(app.tab_strip_snapshot().is_empty());

        assert!(!app.activate_next_tab(), "no active tab to walk from");
        assert_eq!(app.status(), "no tab is currently active");
    }

    #[test]
    fn ui_close_policy_rejects_closing_dirty_tabs_until_allowed() {
        let mut app = PdfxiApp::new();
        assert_eq!(app.close_policy(), UiClosePolicy::RequireClean);
        let id = app.open_demo_tab();
        app.state.tab_mut(id).expect("demo tab").set_dirty(true);
        assert_eq!(app.tab_strip_snapshot()[0].1, "Demo Document 1 *");

        assert!(!app.close_tab_by_id(id), "dirty close is vetoed by default");
        assert!(app.state().tab(id).is_some());
        assert!(app.status().contains("Unsaved changes"), "{}", app.status());

        app.set_close_policy(UiClosePolicy::DiscardDirty);
        assert!(app.close_tab_by_id(id));
        assert!(app.state().is_empty());
    }

    #[test]
    fn per_tab_scroll_offset_is_saved_and_restored_on_switch() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        app.scroll_x = 12.0;
        app.scroll_y = 345.0;

        let b = app.open_demo_tab();
        // Opening b saved a's scroll and started b at the top.
        assert_eq!(
            app.state().tab(a).map(|tab| tab.scroll_offset()),
            Some(CanvasPoint::new(12.0, 345.0))
        );
        assert_eq!(app.scroll_offset(), CanvasPoint::new(0.0, 0.0));

        app.scroll_x = 4.0;
        app.scroll_y = 50.0;
        app.activate_tab(a);
        assert_eq!(app.scroll_offset(), CanvasPoint::new(12.0, 345.0));
        assert_eq!(
            app.state().tab(b).map(|tab| tab.scroll_offset()),
            Some(CanvasPoint::new(4.0, 50.0))
        );

        app.scroll_y = 700.0;
        app.activate_tab(b);
        assert_eq!(app.scroll_offset(), CanvasPoint::new(4.0, 50.0));
        assert_eq!(
            app.state().tab(a).map(|tab| tab.scroll_offset()),
            Some(CanvasPoint::new(12.0, 700.0))
        );
    }

    #[test]
    fn open_document_dedups_and_restores_view_via_ui() {
        let mut app = PdfxiApp::new();
        let first = app.open_document(Path::new(r"C:\docs\a.pdf"));
        let id = match first {
            OpenOutcome::Opened { id } => id,
            other => panic!("unexpected outcome: {other:?}"),
        };
        app.scroll_y = 222.0;
        let again = app.open_document(Path::new("c:/docs/A.PDF"));
        assert_eq!(again, OpenOutcome::Focused { id });
        assert_eq!(app.state().tab_count(), 1);
        assert_eq!(app.scroll_offset(), CanvasPoint::new(0.0, 222.0));
        assert!(app.status().contains("Focused existing tab"));
    }

    #[test]
    fn reorder_and_close_others_via_ui_helpers() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        let c = app.open_demo_tab();
        assert!(app.reorder_tab(c, 0));
        let order: Vec<TabId> = app
            .tab_strip_snapshot()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(order, vec![c, a, b]);
        assert!(!app.reorder_tab(c, 9), "out-of-range reorder is rejected");

        app.activate_tab(b);
        let outcome = app.close_other_tabs();
        assert_eq!(outcome.closed, vec![c, a]);
        assert!(outcome.kept.is_empty());
        let order: Vec<TabId> = app
            .tab_strip_snapshot()
            .into_iter()
            .map(|(id, _, _)| id)
            .collect();
        assert_eq!(order, vec![b]);
        assert!(app.status().contains("Closed 2 other tab(s)"));
    }

    #[test]
    fn close_others_policy_keeps_dirty_tabs() {
        let mut app = PdfxiApp::new();
        let dirty = app.open_demo_tab();
        let clean = app.open_demo_tab();
        let active = app.open_demo_tab();
        app.state.tab_mut(dirty).expect("dirty tab").set_dirty(true);

        // RequireClean (default) vetoes the dirty tab exactly like Ctrl+W does.
        let outcome = app.close_other_tabs();
        assert_eq!(outcome.closed, vec![clean]);
        assert_eq!(outcome.kept, vec![dirty]);
        assert_eq!(app.state().active_tab_id(), Some(active));
        assert_eq!(app.state().tab_count(), 2);
        assert!(app.state().tab(dirty).is_some(), "dirty tab retained");
        assert!(
            app.status().contains("kept 1 with unsaved changes"),
            "{}",
            app.status()
        );

        // Opting in to discard closes the retained tab on a later request.
        app.set_close_policy(UiClosePolicy::DiscardDirty);
        let outcome = app.close_other_tabs();
        assert_eq!(outcome.closed, vec![dirty]);
        assert!(outcome.kept.is_empty());
        assert_eq!(app.state().tab_count(), 1);
        assert_eq!(app.state().active_tab_id(), Some(active));
    }

    #[test]
    fn close_button_targets_a_specific_tab_without_activating_it() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        assert_eq!(app.state().active_tab_id(), Some(b));

        assert!(app.close_tab_by_id(a));
        assert_eq!(app.state().active_tab_id(), Some(b));
        assert_eq!(app.state().tab_count(), 1);
        assert_eq!(app.status(), format!("Closed tab {a}"));
    }

    // ---- T-0089 optional content layers ---------------------------------

    #[test]
    fn demo_tab_installs_layers_and_exposes_a_visibility_plan() {
        let mut app = PdfxiApp::new();
        assert!(app.layer_visibility_plan().is_none(), "no tab yet");
        let id = app.open_demo_tab();
        assert!(app.layers().contains(id));
        let plan = app.layer_visibility_plan().expect("layers installed");
        assert_eq!(plan.visible_ids(), &["oc-bg", "oc-anno", "oc-en"]);
        assert!(plan.is_visible("oc-bg"));
        assert!(!plan.is_visible("oc-grid"));
    }

    #[test]
    fn layer_toggle_and_bulk_actions_via_ui_helpers() {
        let mut app = PdfxiApp::new();
        app.open_demo_tab();
        assert!(app.toggle_layer("oc-grid"));
        assert!(
            app.layer_visibility_plan()
                .expect("plan")
                .is_visible("oc-grid")
        );
        assert!(app.status().contains("oc-grid shown"), "{}", app.status());

        assert!(app.show_all_layers());
        let plan = app.layer_visibility_plan().expect("plan");
        // Radio group keeps only its first member visible.
        assert_eq!(
            plan.visible_ids(),
            &["oc-bg", "oc-anno", "oc-grid", "oc-en"]
        );

        assert!(app.hide_all_layers());
        assert!(app.layer_visibility_plan().expect("plan").is_empty());
    }

    #[test]
    fn radio_group_is_exclusive_through_ui_helpers() {
        let mut app = PdfxiApp::new();
        app.open_demo_tab();
        let plan = app.layer_visibility_plan().expect("plan");
        assert!(plan.is_visible("oc-en"));
        assert!(!plan.is_visible("oc-fr"));

        assert!(app.set_layer_visible("oc-fr", true));
        let plan = app.layer_visibility_plan().expect("plan");
        assert!(!plan.is_visible("oc-en"));
        assert!(plan.is_visible("oc-fr"));
    }

    #[test]
    fn per_tab_layer_state_is_isolated_and_removed_with_the_tab() {
        let mut app = PdfxiApp::new();
        let a = app.open_demo_tab();
        assert!(app.toggle_layer("oc-grid"));
        let b = app.open_demo_tab();
        // The new tab starts from the demo defaults, not tab A's toggled state.
        assert!(
            !app.layer_visibility_plan()
                .expect("plan b")
                .is_visible("oc-grid")
        );

        app.activate_tab(a);
        assert!(
            app.layer_visibility_plan()
                .expect("plan a")
                .is_visible("oc-grid")
        );
        app.activate_tab(b);
        assert!(
            !app.layer_visibility_plan()
                .expect("plan b")
                .is_visible("oc-grid")
        );

        assert!(app.close_tab_by_id(b));
        assert!(!app.layers().contains(b));
        assert!(app.layers().contains(a));
    }

    #[test]
    fn locked_unknown_and_no_tab_layer_actions_report_errors() {
        let mut app = PdfxiApp::new();
        assert!(!app.toggle_layer("oc-bg"), "no active tab");
        assert!(app.status().contains("No document open"));

        let tab = app.open_demo_tab();
        assert!(!app.toggle_layer("ghost"));
        assert!(
            app.status().contains("no optional content group"),
            "{}",
            app.status()
        );

        let locked =
            OcPropertiesSummary::new().add_group(OcgInfo::new("locked", "Locked", true, true));
        assert!(app.layers.install(tab, locked).is_ok());
        assert!(!app.toggle_layer("locked"));
        assert!(app.status().contains("locked"), "{}", app.status());
        assert!(
            app.layer_visibility_plan()
                .expect("plan")
                .is_visible("locked")
        );

        assert!(app.install_demo_layers(), "reinstall resets state");
        assert!(
            app.layer_visibility_plan()
                .expect("plan")
                .is_visible("oc-bg")
        );
    }
}

#[cfg(test)]
mod open_print_tests {
    use super::*;

    fn app_with_pages(pages: usize, current: usize) -> PdfxiApp {
        let mut app = PdfxiApp::new();
        let id = app.state.open_tab(format!("doc-{pages}.pdf"));
        if let Some(tab) = app.state.tab_mut(id) {
            tab.set_page_count(pages);
            tab.set_current_page(current);
        }
        app
    }

    #[test]
    fn print_context_is_derived_from_the_active_tab() {
        let app = app_with_pages(6, 4);
        let ctx = app.print_context();
        assert_eq!(ctx.page_count, 6);
        assert_eq!(ctx.current_page, 4);
        assert!(ctx.selected_pages.is_empty());

        let empty = PdfxiApp::new();
        let ctx = empty.print_context();
        assert_eq!(ctx.page_count, 0);
    }

    #[test]
    fn print_dialog_open_close_and_submit_never_contact_a_printer() {
        let mut app = app_with_pages(4, 2);
        assert!(!app.print_dialog().is_open());
        app.open_print_dialog();
        assert!(app.print_dialog().is_open());

        app.set_print_range(PrintPageRange::Current);
        let plan = app.submit_print().expect("plan built without a printer");
        assert_eq!(plan.pages, vec![2]);
        assert_eq!(plan.summary, app.print_dialog().status());
        assert_eq!(
            app.print_dialog().last_plan().map(|p| p.raster_count()),
            Some(1)
        );

        app.close_print_dialog();
        assert!(!app.print_dialog().is_open());
    }

    #[test]
    fn print_dialog_setters_update_the_model() {
        let mut app = app_with_pages(4, 0);
        app.set_print_copies(2);
        app.set_print_duplex(Duplex::ShortEdge);
        app.set_print_orientation(Orientation::Landscape);
        app.set_print_scale(PageScaleMode::Fit);
        app.set_print_paper(PaperSize::A4);
        app.set_print_selected_pages(vec![3, 1]);
        app.set_print_range(PrintPageRange::Selection);

        let model = app.print_dialog().model();
        assert_eq!(model.copies, 2);
        assert_eq!(model.duplex, Duplex::ShortEdge);
        assert_eq!(model.orientation, Orientation::Landscape);
        assert_eq!(model.scale, PageScaleMode::Fit);
        assert_eq!(model.paper, PaperSize::A4);
        assert_eq!(model.range, PrintPageRange::Selection);
        assert_eq!(app.print_context().selected_pages, vec![3, 1]);

        let plan = app.submit_print().expect("selection plan");
        assert_eq!(plan.pages, vec![1, 1, 3, 3]);
    }

    #[test]
    fn open_errors_are_recorded_and_cleared() {
        let mut app = PdfxiApp::new();
        assert!(app.open_errors().is_empty());
        app.record_open_error(OpenError::UnsupportedFormat);
        app.record_open_error(OpenError::Encrypted);
        assert_eq!(app.open_errors().len(), 2);
        assert_eq!(app.open_errors()[0], OpenError::UnsupportedFormat);
        app.clear_open_errors();
        assert!(app.open_errors().is_empty());
    }

    #[test]
    fn invalid_print_range_is_reported_through_the_shell() {
        let mut app = app_with_pages(4, 0);
        app.set_print_range(PrintPageRange::Custom("nope".to_owned()));
        assert!(app.submit_print().is_err());
        assert!(app.print_dialog().last_error().is_some());
        assert!(app.print_dialog().last_plan().is_none());
    }

    // ---- T-0126 split / tile UI helper behavior --------------------------

    #[test]
    fn pane_layout_label_is_stable() {
        use crate::{PaneLayout, SplitDirection, TileGrid};
        assert_eq!(pane_layout_label(PaneLayout::Single), "Single");
        let split = crate::SplitLayout::new(SplitDirection::Horizontal, 0.5, 4, 32)
            .expect("valid split layout");
        assert_eq!(pane_layout_label(PaneLayout::Split(split)), "Split H 50");
        assert_eq!(
            pane_layout_label(PaneLayout::Tile(TileGrid::new(2, 2).expect("valid grid"))),
            "Tile 2x2"
        );
    }

    #[test]
    fn pane_commands_update_the_manager_and_status() {
        let mut app = PdfxiApp::default();
        let tab = app.open_demo_tab();
        assert_eq!(app.pane_manager().pane_count(), 1);
        assert_eq!(app.pane_manager().bound_tabs(), vec![tab]);

        app.apply_pane_command(PaneCommand::SplitVertical);
        assert_eq!(app.pane_manager().pane_count(), 2);
        assert!(app.status().contains("Split V"), "{}", app.status());

        app.set_pane_zoom(Zoom::custom(1.5));
        app.apply_pane_command(PaneCommand::FocusNextPane);
        assert_eq!(app.pane_manager().active_index(), 1);
        assert!(app.status().contains("focus 1"), "{}", app.status());
        // The direct focus helper reports its own confirmation status.
        app.focus_pane(0);
        assert!(app.status().contains("Focused pane 0"), "{}", app.status());

        app.apply_pane_command(PaneCommand::Tile2x2);
        assert_eq!(app.pane_manager().pane_count(), 4);
        assert!(
            app.pane_status().contains("4 pane(s)"),
            "{}",
            app.pane_status()
        );

        // An invalid focus is reported through the status without panicking.
        app.focus_pane(99);
        assert!(
            app.status().contains("no pane at index 99"),
            "{}",
            app.status()
        );
    }

    #[test]
    fn pane_geometry_helper_is_window_free_and_reports_small_viewports() {
        let mut app = PdfxiApp::default();
        assert_eq!(
            app.open_demo_tab(),
            app.state().active_tab_id().expect("tab")
        );
        let geometry = app.pane_geometry(400, 300).expect("roomy viewport");
        assert_eq!(geometry, vec![crate::PaneRect::new(0, 0, 400, 300)]);

        app.apply_pane_command(PaneCommand::SplitHorizontal);
        let geometry = app.pane_geometry(400, 300).expect("roomy viewport");
        assert_eq!(geometry.len(), 2);
        assert!(
            geometry[0].bottom() < geometry[1].y,
            "top pane above bottom pane"
        );

        // A viewport too small for the divider/minimum has no geometry.
        assert_eq!(app.pane_geometry(10, 10), None);
    }

    #[test]
    fn closing_a_tab_removes_its_panes_through_the_shell() {
        let mut app = PdfxiApp::default();
        let first = app.open_demo_tab();
        app.apply_pane_command(PaneCommand::SplitHorizontal);
        assert_eq!(app.pane_manager().pane_count(), 2);

        let second = app.open_demo_tab();
        assert_eq!(
            app.pane_manager().pane_count(),
            1,
            "new tab resets to one pane"
        );
        assert_eq!(app.pane_manager().bound_tabs(), vec![second]);

        // Closing the active tab empties the pane manager.
        assert!(app.close_tab_by_id(second));
        assert_eq!(app.pane_manager().pane_count(), 0);
        assert_eq!(app.pane_manager().active_pane(), None);
        // The earlier tab still exists but its panes were discarded on switch.
        assert!(app.state().tab(first).is_some());
    }

    #[test]
    fn pane_sharing_toggle_is_reflected_in_status() {
        let mut app = PdfxiApp::default();
        app.open_demo_tab();
        app.apply_pane_command(PaneCommand::ToggleSharing);
        assert_eq!(app.pane_manager().sharing(), ViewSharing::MirrorTab);
        assert!(
            app.pane_status().contains("MirrorTab"),
            "{}",
            app.pane_status()
        );
    }
}

/// Window-free tests for the bookmarks, attachments, and links viewer panels.
#[cfg(test)]
mod navigation_viewer_tests {
    use super::*;

    #[test]
    fn demo_tab_installs_navigation_models_and_article_summary() {
        let mut app = PdfxiApp::default();
        assert!(app.bookmark_rows().is_empty());
        assert!(app.attachment_rows().is_empty());
        assert!(app.link_rows(0).is_empty());

        let id = app.open_demo_tab();
        assert!(app.bookmarks().contains(id));
        assert!(app.attachments().contains(id));
        assert!(app.links().contains(id));
        assert_eq!(app.bookmark_rows().len(), 4);
        let articles = app.article_summary();
        assert_eq!(articles.len(), 1);
        assert_eq!(articles[0].0, "art-main");
    }

    #[test]
    fn bookmark_collapse_and_selection_via_ui_helpers() {
        let mut app = PdfxiApp::default();
        app.open_demo_tab();
        // bm-root and bm-intro are open by default: all four nodes visible.
        assert_eq!(app.bookmark_rows().len(), 4);
        // bm-root has children; collapsing it hides bm-intro and bm-body.
        // The helper returns whether the action succeeded; inspect the rows for
        // the resulting expansion state.
        assert!(app.toggle_bookmark_expansion("bm-root"));
        assert_eq!(app.bookmark_rows().len(), 2, "children collapsed");
        assert!(
            !app.bookmarks()
                .state(app.state().active_tab_id().expect("tab"))
                .expect("state")
                .is_expanded("bm-root")
        );
        assert!(app.toggle_bookmark_expansion("bm-root"));
        assert_eq!(app.bookmark_rows().len(), 4, "children re-expanded");
        assert!(app.select_bookmark("bm-refs"));
        assert!(
            app.bookmark_rows()
                .iter()
                .any(|row| row.id() == "bm-refs" && row.selected())
        );
        assert!(!app.select_bookmark("ghost"));
        assert!(app.status().contains("no bookmark"), "{}", app.status());
    }

    #[test]
    fn attachment_rows_order_open_boundary_and_empty_state() {
        let mut app = PdfxiApp::default();
        app.open_demo_tab();
        let rows = app.attachment_rows();
        let names: Vec<&str> = rows.iter().map(|row| row.0.as_str()).collect();
        assert_eq!(names, vec!["data.csv", "notes.txt", "supplement.pdf"]);
        assert!(app.select_attachment("notes.txt"));
        let tab = app.state().active_tab_id().expect("active tab");
        assert_eq!(
            app.attachments()
                .model(tab)
                .and_then(|model| model.get("notes.txt"))
                .map(|attachment| attachment.size()),
            Some(256)
        );
        // Host open boundary: request recorded, no bytes read here.
        assert!(app.open_attachment("notes.txt"));
        assert_eq!(
            app.last_open_request().map(AttachmentOpenRequest::name),
            Some("notes.txt")
        );
        assert!(!app.open_attachment("missing"));
        assert_eq!(app.attachment_rows().len(), 3);
    }

    #[test]
    fn link_rows_resolution_and_hit_test_via_ui_helpers() {
        let mut app = PdfxiApp::default();
        app.open_demo_tab();
        let rows = app.link_rows(0);
        assert_eq!(rows.len(), 2);
        assert_eq!(rows[0].0, "lnk-web");
        assert!(rows[0].2.starts_with("URI"));
        assert_eq!(rows[1].0, "lnk-jump");
        assert_eq!(
            app.resolve_link("lnk-web"),
            Some(ResolvedLink::Uri("https://example.test".into()))
        );
        assert!(matches!(
            app.resolve_link("lnk-named"),
            Some(ResolvedLink::NamedPage { page: Some(0), .. })
        ));
        assert_eq!(app.resolve_link("ghost"), None);
        assert_eq!(app.hit_test_link(0, 80.0, 705.0), Some("lnk-web".into()));
        assert_eq!(app.hit_test_link(0, 230.0, 705.0), Some("lnk-jump".into()));
        assert_eq!(app.hit_test_link(0, 0.0, 0.0), None);
        assert_eq!(
            link_target_label(&LinkTarget::Internal { page: 2 }),
            "Page 3"
        );
    }

    #[test]
    fn navigation_data_is_isolated_per_tab_and_removed_with_tab() {
        let mut app = PdfxiApp::default();
        let a = app.open_demo_tab();
        let b = app.open_demo_tab();
        // The active tab is `b`; selecting there must not touch `a`.
        assert!(app.select_bookmark("bm-refs"));
        assert!(
            app.bookmarks()
                .state(b)
                .expect("tab b")
                .is_expanded("bm-intro")
        );
        assert_eq!(
            app.bookmarks().state(b).expect("tab b").selected(),
            Some("bm-refs")
        );
        assert_eq!(app.bookmarks().state(a).expect("tab a").selected(), None);
        assert!(app.close_tab_by_id(b));
        assert!(!app.bookmarks().contains(b));
        assert!(!app.attachments().contains(b));
        assert!(!app.links().contains(b));
        assert!(app.bookmarks().contains(a));
    }

    #[test]
    fn reinstall_restores_navigation_data_after_clear() {
        let mut app = PdfxiApp::default();
        let id = app.open_demo_tab();
        assert!(app.bookmarks.remove(id));
        assert!(app.bookmark_rows().is_empty());
        assert!(app.install_demo_navigation_data());
        assert!(app.bookmarks().contains(id));
        assert_eq!(app.bookmark_rows().len(), 4);
        let mut empty = PdfxiApp::default();
        assert!(!empty.install_demo_navigation_data());
        assert!(empty.status().contains("No document open"));
    }
}
