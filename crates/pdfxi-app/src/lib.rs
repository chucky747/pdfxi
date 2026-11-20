//! Desktop application shell, document tabs, toolsets, panels, and canvas.
//!
//! This crate provides a pure-Rust model of the viewer shell: open document
//! tabs, per-tab layout/zoom/rotation state, and the active selection tool. The
//! core model has no `egui`, renderer, or platform dependency so the state
//! machine can be tested deterministically. The [`canvas`] module arranges
//! pages and answers hit-testing/coordinate questions, and [`selection`] maps
//! drag points to ordered text ranges — both without any renderer dependency.
//! The [`raster`] module adds a dependency-light, LRU-bounded cache that feeds
//! [`pdfxi_render::Renderer`] rasters to the canvas; it has no `egui` types. The
//! [`find`] module holds deterministic, bounded in-document search state over
//! caller-supplied page text, also without any GUI dependency.
//! The [`layers`] module models optional content groups (OCGs) from a caller-
//! supplied `/OCProperties` summary: deterministic layer ordering, per-tab
//! visibility state with radio-group exclusivity and locked layers, and a
//! backend-agnostic visibility plan the canvas can consume. It has no GUI or
//! renderer dependency.
//! The [`thumbs`] module adds the window-free state and math for a page-thumbnail
//! navigation pane: visible-row ranges, page/row mapping, active-page tracking,
//! and a bounded prefetch request queue backed by `pdfxi-render`'s cache.
//! The [`commands`] module defines typed view commands (rotation, fit modes,
//! zoom) and a pure keyboard-shortcut map shared by the toolbar and key paths.
//! The [`split`] module adds a window-free split/tile pane model: deterministic
//! pane geometry (rects, dividers, minimum sizes), per-pane scroll/zoom/page
//! state with sharing rules, focus, and close-pane/tab-close cleanup. Panes are
//! logical rectangles inside the single viewer window, never native OS windows.
//! The [`bookmarks`] module models the outline destination tree (titles,
//! page/fit destinations, nesting, expand/collapse, selection, deterministic
//! pre-order) plus an article-thread summary, all without a GUI or PDF backend.
//! The [`attachments`] module models the embedded-file list (name, description,
//! size, page association, selection, deterministic ordering) and exposes an
//! explicit host open-request boundary; it never reads file bytes. The [`links`]
//! module indexes link annotations per page, resolves internal/URI/named
//! destinations, and provides a pure page-space hit-test helper. All three keep
//! mutable state in per-tab registries so documents stay isolated.
//! The [`home`] module holds the home-screen model: bounded, deduplicated
//! recent/favorite/collection state over opaque document keys with deterministic
//! ordering and a bounded, I/O-free text serialization format.
//! The [`page_text`], [`cross_file`], and [`compare`] modules add I/O-free,
//! deterministic workflows over caller-supplied page text: cross-file search
//! with ranking/grouping/result caps and version compare with added/removed/
//! changed hunks and a summary. They depend on no PDF or GUI backend.
//! The [`nav`] module supplies the accessibility navigation model: a
//! deterministic focus ring, toolkit-free navigation shortcuts, WCAG-audited
//! high-contrast color tokens, and bounded reflow layout metrics. The
//! [`read_aloud`] module builds ordered speech chunks with word/sentence
//! boundaries and drives them with a window-free play/pause/step state machine
//! behind an explicit host speech-sink boundary. The [`a11y`] module derives a
//! deterministic accessibility node tree, focus announcements, and a screen-
//! reader host adapter from a caller-supplied state snapshot; with the `ui`
//! feature it registers that tree through `egui`'s AccessKit re-export.
//! [`AppState`] is the tabbed document workspace: it owns an insertion-ordered
//! set of stable-id tabs, each with its own path, dirty flag, and per-tab view
//! state (page, zoom, layout, rotation, and scroll offset). Open, close,
//! reorder, next/previous, and close-others are deterministic operations with
//! typed errors, including an unsaved-close policy hook.
//! An optional native viewer shell built on `egui`/`eframe` is available behind
//! the `ui` feature; it wires [`find`], [`selection`], and [`thumbs`] into a find
//! bar, copy command, and thumbnail side panel, and renders a tab strip with
//! close buttons, an active highlight, and tab keyboard shortcuts.

#![deny(missing_docs)]
#![forbid(unsafe_code)]

pub mod a11y;
pub mod attachments;
pub mod bookmarks;
pub mod canvas;
pub mod commands;
pub mod compare;
pub mod cross_file;
pub mod find;
pub mod home;
pub mod layers;
pub mod links;
pub mod nav;
pub mod open;
pub mod page_text;
pub mod print;
pub mod raster;
pub mod read_aloud;
pub mod selection;
pub mod split;
mod state;
mod tabs;
pub mod thumbs;
mod toolset;
mod view;

/// Native egui/eframe viewer shell, available behind the `ui` feature.
#[cfg(feature = "ui")]
pub mod ui;

pub use a11y::{
    A11yNode, A11yRole, A11yTree, AccessibilityHost, AccessibilitySnapshot, AnnouncementLog,
    FocusAnnouncement, NullAccessibilityHost,
};
pub use attachments::{
    Attachment, AttachmentError, AttachmentInfo, AttachmentModel, AttachmentOpenRequest,
    AttachmentRegistry, AttachmentState, AttachmentSummary, MAX_ATTACHMENT_DESCRIPTION_LEN,
    MAX_ATTACHMENT_NAME_LEN, MAX_ATTACHMENTS,
};
pub use bookmarks::{
    ArticleThread, Bookmark, BookmarkError, BookmarkId, BookmarkInfo, BookmarkModel,
    BookmarkRegistry, BookmarkState, BookmarkSummary, Destination, Fit, MAX_ARTICLE_THREADS,
    MAX_BOOKMARK_TITLE_LEN, MAX_BOOKMARKS, VisibleBookmark,
};
pub use canvas::{
    CanvasHit, CanvasLayout, CanvasPoint, CanvasRect, CanvasSize, PageGeometry, PageSize,
    effective_page_size, rotate_point, unrotate_point, uv_quad_for_rotation,
};
pub use commands::{ShortcutKey, ShortcutModifiers, ViewCommand, map_shortcut};
pub use compare::{
    CompareError, CompareHunk, CompareReport, CompareSummary, HunkKind, MAX_SNAPSHOT_PAGES,
    TextSnapshot, compare_snapshots,
};
pub use cross_file::{
    CrossFileError, CrossFileHit, CrossFileQuery, CrossFileReport, CrossFileSearch, CrossMatch,
    CrossPageGroup, DEFAULT_MAX_PER_FILE, DEFAULT_MAX_RESULTS, FileId, MAX_FILES,
    MAX_PAGES_PER_FILE, SearchFile,
};
pub use find::{DEFAULT_MATCH_CAP, FindMatch, FindOptions, FindState};
pub use home::{
    Collection, DocumentKey, FavoriteEntry, HomeError, HomeModel, HomeSummary,
    MAX_COLLECTION_MEMBERS, MAX_COLLECTIONS, MAX_FAVORITES, MAX_KEY_LEN, MAX_NAME_LEN, MAX_RECENTS,
    MAX_SERIALIZED_LINES, MAX_TITLE_LEN, RecentEntry, escape_field,
};
pub use layers::{
    LayerError, LayerInfo, LayerModel, LayerRegistry, LayerState, OcPropertiesSummary, OcgId,
    OcgInfo, RadioGroup, VisibilityPlan,
};
pub use links::{
    Link, LinkError, LinkId, LinkInfo, LinkModel, LinkPoint, LinkRect, LinkRegistry, LinkSummary,
    LinkTarget, MAX_LINK_TEXT_LEN, MAX_LINKS, MAX_NAMES, ResolvedLink,
};
pub use nav::{
    FocusOrder, FocusTarget, Focusable, HIGH_CONTRAST_TARGET, MAX_REFLOW_SCALE, MIN_CONTRAST_RATIO,
    MIN_REFLOW_SCALE, NavCommand, NavKey, NavModifiers, ReflowMetrics, ReflowRequest, Rgb, Theme,
    ThemeTokens, audited_contrast_pairs, contrast_ratio, map_nav_shortcut, reflow_metrics, tokens,
};
pub use open::{
    DocumentOpenPipeline, DocumentOpener, EncryptionState, MockDocumentOpener, MockResponse,
    OpenBackendKind, OpenError, OpenResult, OpenTabStates, OpenedDocument, RecentFiles,
    TabOpenState,
};
pub use page_text::PageText;
pub use print::{
    DEFAULT_PRINT_DPI, PaperSize, PrintContext, PrintDialogState, PrintModel, PrintModelError,
    PrintPageRange, PrintPlan,
};
pub use raster::{DEFAULT_RASTER_LIMIT, RasterCache, RasterError, RasterKey};
pub use read_aloud::{
    ChunkBoundary, ChunkMode, NullSpeechSink, PlaybackState, ReadAloudPlan, SpeechChunk,
    SpeechRequest, SpeechSink, VoiceSettings, build_chunks,
};
pub use selection::{
    OwnedSpan, OwnedTextPage, SelectableTextPage, Selection, SelectionModel, SpanView, TextAnchor,
};
pub use split::{
    DEFAULT_DIVIDER_THICKNESS, DEFAULT_MINIMUM_PANE, DEFAULT_SPLIT_RATIO, Divider, DividerAxis,
    PaneCommand, PaneKey, PaneLayout, PaneManager, PanePoint, PaneRect, PaneViewState,
    SplitDirection, SplitError, SplitGeometry, SplitLayout, TileGrid, TileLayout, ViewSharing,
    map_pane_shortcut,
};
pub use state::{AppError, AppState, CloseDecision, CloseOthersOutcome, CloseOutcome, OpenOutcome};
pub use tabs::{DocumentTab, TabId, display_name_from_path, normalize_path_key};
pub use thumbs::*;
pub use toolset::Toolset;
pub use view::{DEFAULT_ZOOM, MAX_ZOOM, MIN_ZOOM, Rotation, ViewMode, Zoom};

/// Return the implementation name for diagnostics.
pub fn app_name() -> &'static str {
    "PDFXI"
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_name_is_stable() {
        assert_eq!(app_name(), "PDFXI");
    }
}
