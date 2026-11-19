//! Document tab identity, state, and deterministic title computation.

use std::path::Path;

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

/// The full state of one open document in the viewer.
#[derive(Debug, Clone)]
pub struct DocumentTab {
    id: TabId,
    display_name: String,
    view_mode: ViewMode,
    zoom: Zoom,
    rotation: Rotation,
    current_page: usize,
    page_count: usize,
    dirty: bool,
}

impl DocumentTab {
    /// Create a new tab with default view settings.
    pub fn new(id: TabId, display_name: impl Into<String>) -> Self {
        Self {
            id,
            display_name: display_name.into(),
            view_mode: ViewMode::default(),
            zoom: Zoom::default(),
            rotation: Rotation::default(),
            current_page: 0,
            page_count: 0,
            dirty: false,
        }
    }

    /// Create a tab whose display name is derived from a path's file name.
    pub fn from_path(id: TabId, path: &Path) -> Self {
        Self::new(id, display_name_from_path(path))
    }

    /// This tab's stable identifier.
    pub fn id(&self) -> TabId {
        self.id
    }

    /// The human-readable document name shown in the tab strip.
    pub fn display_name(&self) -> &str {
        &self.display_name
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
}
