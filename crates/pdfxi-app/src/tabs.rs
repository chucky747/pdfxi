//! Document tab identity, state, and deterministic title computation.
//!
//! A [`DocumentTab`] is the unit of per-document view state: its stable id, the
//! source path used for duplicate detection, the unsaved-changes flag, and all
//! view settings (page, zoom, layout, rotation, and canvas scroll offset).
//! Keeping this state on the tab — rather than on the window — is what lets a
//! tabbed workspace restore a document's view exactly when it is reactivated.

use std::path::{Path, PathBuf};

use crate::canvas::CanvasPoint;
use crate::view::{Rotation, ViewMode, Zoom};

/// A stable identifier for an open document tab.
///
/// Ids are assigned monotonically by [`crate::AppState`] and never reused for a
/// different tab during a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct TabId(u64);

impl TabId {
    /// Build an id from a raw numeric value.
    ///
    /// Callers normally obtain ids from [`crate::AppState`] instead of
    /// constructing them directly.
    pub fn from_u64(raw: u64) -> Self {
        Self(raw)
    }

    /// The raw numeric value of this id.
    pub fn as_u64(self) -> u64 {
        self.0
    }
}

impl std::fmt::Display for TabId {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Derive a display name from a filesystem path.
///
/// Only the final non-empty path component is used; both `/` and `\` are
/// treated as separators so the result is deterministic across platforms.
/// An empty or root-only path yields `"Untitled"`.
pub fn display_name_from_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let name = raw
        .rsplit(['/', '\\'])
        .find(|component| !component.is_empty())
        .unwrap_or("Untitled");
    name.to_string()
}

/// Normalize a filesystem path into a comparison key for duplicate detection.
///
/// Backslashes become forward slashes, the Unicode lowercase form is used, and
/// trailing separators are removed. This makes `C:\Docs\a.pdf` and
/// `c:/docs/a.pdf` compare equal on every platform. Empty and root-only paths
/// produce an empty key, which callers should treat as "no path identity" (so
/// two untitled documents are never merged).
pub fn normalize_path_key(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let normalized = raw.replace('\\', "/").to_lowercase();
    normalized.trim_end_matches('/').to_string()
}

/// The full state of one open document in the viewer.
#[derive(Debug, Clone, PartialEq)]
pub struct DocumentTab {
    id: TabId,
    display_name: String,
    path: Option<PathBuf>,
    view_mode: ViewMode,
    zoom: Zoom,
    rotation: Rotation,
    current_page: usize,
    page_count: usize,
    scroll: CanvasPoint,
    dirty: bool,
}

impl DocumentTab {
    /// Create a new tab with default view settings.
    pub fn new(id: TabId, display_name: impl Into<String>) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            path: None,
            view_mode: ViewMode::default(),
            zoom: Zoom::default(),
            rotation: Rotation::default(),
            current_page: 0,
            page_count: 0,
            scroll: CanvasPoint::default(),
            dirty: false,
        }
    }

    /// Create a tab whose display name is derived from a path's file name.
    ///
    /// The path itself is retained so a workspace can detect when the same
    /// document is opened a second time and focus the existing tab instead.
    pub fn from_path(id: TabId, path: &Path) -> Self {
        let mut tab = Self::new(id, display_name_from_path(path));
        tab.path = Some(path.to_path_buf());
        tab
    }

    /// This tab's stable identifier.
    pub fn id(&self) -> TabId {
        self.id
    }

    /// The human-readable document name shown in the tab strip.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }

    /// The source path of this document, when it was opened from a path.
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// The normalized duplicate-detection key for this tab's path.
    ///
    /// Returns `None` for tabs without a path or with an empty/root-only path;
    /// callers must not merge such tabs.
    pub fn path_key(&self) -> Option<String> {
        self.path
            .as_deref()
            .map(normalize_path_key)
            .filter(|key| !key.is_empty())
    }

    /// The page layout mode for this tab.
    pub fn view_mode(&self) -> ViewMode {
        self.view_mode
    }

    /// The zoom policy for this tab.
    pub fn zoom(&self) -> Zoom {
        self.zoom
    }

    /// The page rotation for this tab.
    pub fn rotation(&self) -> Rotation {
        self.rotation
    }

    /// The canvas scroll offset saved for this tab.
    ///
    /// Restoring this value when a tab is reactivated keeps the viewer at the
    /// same position the user left, alongside the page, zoom, layout, and
    /// rotation settings.
    pub fn scroll_offset(&self) -> CanvasPoint {
        self.scroll
    }

    /// Save a canvas scroll offset for this tab.
    ///
    /// Non-finite and negative components are sanitized to zero so a restored
    /// view is always finite and deterministic, matching the canvas clamp
    /// behavior. No viewport is needed here; the canvas clamps to content bounds
    /// at paint time.
    pub fn set_scroll_offset(&mut self, offset: CanvasPoint) {
        let sanitize = |value: f32| {
            if value.is_finite() && value > 0.0 {
                value
            } else {
                0.0
            }
        };
        self.scroll = CanvasPoint::new(sanitize(offset.x), sanitize(offset.y));
    }

    /// The zero-based index of the currently displayed page.
    pub fn current_page(&self) -> usize {
        self.current_page
    }

    /// The number of pages in the document (zero until loaded).
    pub fn page_count(&self) -> usize {
        self.page_count
    }

    /// Whether the tab has unsaved changes.
    pub fn is_dirty(&self) -> bool {
        self.dirty
    }

    /// Set the page layout mode.
    pub fn set_view_mode(&mut self, mode: ViewMode) {
        self.view_mode = mode;
    }

    /// Set the zoom policy, clamping any custom scale.
    pub fn set_zoom(&mut self, zoom: Zoom) {
        self.zoom = zoom.clamped();
    }

    /// Set the page rotation.
    pub fn set_rotation(&mut self, rotation: Rotation) {
        self.rotation = rotation;
    }

    /// Update the page count, keeping the current page in range.
    pub fn set_page_count(&mut self, count: usize) {
        self.page_count = count;
        self.clamp_current_page();
    }

    /// Set the current page, clamped to the loaded page range.
    pub fn set_current_page(&mut self, page: usize) {
        self.current_page = page;
        self.clamp_current_page();
    }

    /// Mark or clear the unsaved-changes flag.
    pub fn set_dirty(&mut self, dirty: bool) {
        self.dirty = dirty;
    }

    /// Rotate the page 90 degrees clockwise.
    pub fn rotate_clockwise(&mut self) {
        self.rotation = self.rotation.clockwise();
    }

    /// Rotate the page 90 degrees counterclockwise.
    pub fn rotate_counterclockwise(&mut self) {
        self.rotation = self.rotation.counterclockwise();
    }

    /// Compute the title displayed in the tab strip.
    ///
    /// The result depends only on the tab's own fields: the trimmed display
    /// name (or `"Untitled"` when blank) plus a trailing `*` when the document
    /// is dirty. No time, randomness, or external state is involved.
    pub fn title(&self) -> String {
        let trimmed = self.display_name.trim();
        let base = if trimmed.is_empty() {
            "Untitled"
        } else {
            trimmed
        };
        if self.dirty {
            format!("{base} *")
        } else {
            base.to_string()
        }
    }

    /// Clamp the current page into `[0, page_count)` (or zero when unloaded).
    fn clamp_current_page(&mut self) {
        self.current_page = if self.page_count == 0 {
            0
        } else {
            self.current_page.min(self.page_count - 1)
        };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn display_name_uses_final_path_component() {
        assert_eq!(
            display_name_from_path(Path::new(r"C:\docs\report.pdf")),
            "report.pdf"
        );
        assert_eq!(
            display_name_from_path(Path::new("/home/u/notes.pdf")),
            "notes.pdf"
        );
        assert_eq!(display_name_from_path(Path::new(r"C:\docs\")), "docs");
        assert_eq!(display_name_from_path(Path::new("")), "Untitled");
    }

    #[test]
    fn tab_title_is_deterministic() {
        let mut tab = DocumentTab::new(TabId::from_u64(1), "  Annual Report  ");
        assert_eq!(tab.title(), "Annual Report");
        tab.set_dirty(true);
        assert_eq!(tab.title(), "Annual Report *");
        tab.set_dirty(false);
        assert_eq!(tab.title(), "Annual Report");

        let blank = DocumentTab::new(TabId::from_u64(2), "   ");
        assert_eq!(blank.title(), "Untitled");
        // Calling twice with unchanged state yields the same string.
        assert_eq!(blank.title(), blank.title());
    }

    #[test]
    fn page_indices_are_clamped_to_loaded_range() {
        let mut tab = DocumentTab::new(TabId::from_u64(3), "doc");
        tab.set_current_page(9);
        assert_eq!(tab.current_page(), 0);
        tab.set_page_count(5);
        tab.set_current_page(9);
        assert_eq!(tab.current_page(), 4);
        tab.set_page_count(2);
        assert_eq!(tab.current_page(), 1);
        tab.set_page_count(0);
        assert_eq!(tab.current_page(), 0);
    }

    #[test]
    fn path_identity_is_normalized_for_duplicate_detection() {
        let windows = Path::new(r"C:\Docs\Report.PDF");
        let unix = Path::new("c:/docs/report.pdf");
        assert_eq!(display_name_from_path(windows), "Report.PDF");
        assert_eq!(normalize_path_key(windows), "c:/docs/report.pdf");
        assert_eq!(normalize_path_key(windows), normalize_path_key(unix));

        let tab = DocumentTab::from_path(TabId::from_u64(1), windows);
        assert_eq!(tab.path(), Some(windows));
        assert_eq!(tab.path_key().as_deref(), Some("c:/docs/report.pdf"));

        let untitled = DocumentTab::new(TabId::from_u64(2), "Untitled");
        assert_eq!(untitled.path(), None);
        assert_eq!(untitled.path_key(), None);
        let root = DocumentTab::from_path(TabId::from_u64(3), Path::new("/"));
        assert_eq!(root.path_key(), None, "root-only paths carry no identity");
    }

    #[test]
    fn rotation_state_is_owned_by_the_tab() {
        // `DocumentTab` owns the per-tab rotation field and mutators;
        // `AppState` only delegates to these methods for the active tab.
        let mut tab = DocumentTab::new(TabId::from_u64(1), "doc");
        assert_eq!(tab.rotation(), Rotation::Deg0);
        tab.rotate_clockwise();
        assert_eq!(tab.rotation(), Rotation::Deg90);
        tab.rotate_counterclockwise();
        assert_eq!(tab.rotation(), Rotation::Deg0);
        tab.set_rotation(Rotation::Deg270);
        assert_eq!(tab.rotation(), Rotation::Deg270);
        // Rotation is independent of the tab's other view settings.
        tab.set_zoom(Zoom::custom(1.5));
        tab.set_view_mode(ViewMode::TwoPage);
        assert_eq!(tab.rotation(), Rotation::Deg270);
    }

    #[test]
    fn scroll_offset_is_sanitized_and_restorable() {
        let mut tab = DocumentTab::new(TabId::from_u64(1), "doc");
        assert_eq!(tab.scroll_offset(), CanvasPoint::new(0.0, 0.0));

        tab.set_scroll_offset(CanvasPoint::new(120.5, 900.0));
        assert_eq!(tab.scroll_offset(), CanvasPoint::new(120.5, 900.0));

        // Non-finite and negative values become zero, never NaN/infinity.
        tab.set_scroll_offset(CanvasPoint::new(f32::NAN, f32::INFINITY));
        assert_eq!(tab.scroll_offset(), CanvasPoint::new(0.0, 0.0));
        tab.set_scroll_offset(CanvasPoint::new(-5.0, -1.0));
        assert_eq!(tab.scroll_offset(), CanvasPoint::new(0.0, 0.0));
    }
}
