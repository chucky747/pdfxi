//! The document workspace: open tabs, the active tab, and tab operations.
//!
//! [`AppState`] is the UI-agnostic model behind the tabbed interface. It owns an
//! ordered set of open [`DocumentTab`]s, tracks which one is active, allocates
//! stable ids that are never reused, and implements every tab operation the
//! shell exposes (open, activate, close, reorder, next/previous, close-others).
//! All operations are deterministic and report failures as typed [`AppError`]
//! values, so they can be driven and asserted without a window.

use std::path::Path;

use crate::canvas::CanvasPoint;
use crate::tabs::{DocumentTab, TabId, normalize_path_key};
use crate::toolset::Toolset;
use crate::view::{DEFAULT_ZOOM, Rotation, ViewMode, Zoom};

/// Errors produced by tab-oriented operations on [`AppState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// No tab with the requested id exists.
    TabNotFound(TabId),
    /// The operation requires an active tab, but none is set.
    NoActiveTab,
    /// A reorder target was outside the open tab range.
    InvalidTabIndex {
        /// The rejected destination index.
        index: usize,
        /// The number of open tabs at the time of the call.
        len: usize,
    },
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TabNotFound(id) => write!(f, "no open tab with id {id}"),
            Self::NoActiveTab => f.write_str("no tab is currently active"),
            Self::InvalidTabIndex { index, len } => {
                write!(f, "tab index {index} is outside the open range of {len}")
            }
        }
    }
}

impl std::error::Error for AppError {}

/// The decision returned by an unsaved-close policy hook.
///
/// A hook is handed the [`DocumentTab`] being closed, including its dirty flag,
/// so a caller can implement "save / discard / cancel" behavior without the
/// model depending on any dialog or UI framework.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CloseDecision {
    /// Proceed with the close, discarding any unsaved changes.
    Close,
    /// Keep the tab open (the user cancelled, or the document must be saved).
    Keep,
}

/// The typed result of a policy-aware close request.
#[derive(Debug, Clone, PartialEq)]
pub enum CloseOutcome {
    /// The policy allowed the close; the removed tab is returned.
    Closed {
        /// The tab that was removed from the workspace.
        tab: DocumentTab,
    },
    /// The policy vetoed the close; the tab is still open.
    Cancelled {
        /// The id of the tab that was kept.
        id: TabId,
    },
}

/// The typed result of opening a document into the workspace.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum OpenOutcome {
    /// A new tab was created and made active.
    Opened {
        /// The id of the newly created tab.
        id: TabId,
    },
    /// A tab with the same path already existed and was focused instead.
    Focused {
        /// The id of the pre-existing tab that was activated.
        id: TabId,
    },
}

/// Which tabs a close-others operation affected.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CloseOthersOutcome {
    /// Ids of tabs that were closed.
    pub closed: Vec<TabId>,
    /// Ids of tabs a policy hook kept open.
    pub kept: Vec<TabId>,
}

/// The top-level, UI-agnostic document workspace of the viewer shell.
///
/// `AppState` owns the set of open [`DocumentTab`]s, which one is active, and
/// the globally selected [`Toolset`]. View settings (layout, zoom, rotation,
/// page, and scroll offset) plus the dirty flag live on each tab, so switching
/// tabs preserves each document's view independently. Ids are allocated
/// monotonically and are never reused while the application runs, even after a
/// tab is closed.
#[derive(Debug)]
pub struct AppState {
    tabs: Vec<DocumentTab>,
    active_tab_id: Option<TabId>,
    next_id: u64,
    active_tool: Toolset,
}

impl Default for AppState {
    fn default() -> Self {
        Self {
            tabs: Vec::new(),
            active_tab_id: None,
            next_id: 1,
            active_tool: Toolset::default(),
        }
    }
}

impl AppState {
    /// Create an empty document workspace.
    pub fn new() -> Self {
        Self::default()
    }

    /// Open a new tab with the given display name and make it active.
    pub fn open_tab(&mut self, display_name: impl Into<String>) -> TabId {
        let id = self.allocate_id();
        self.tabs.push(DocumentTab::new(id, display_name));
        self.active_tab_id = Some(id);
        id
    }

    /// Open a tab for a filesystem path, deriving the display name, and make
    /// it active.
    ///
    /// This always creates a new tab. Use [`Self::open_document`] when a second
    /// open of the same path should focus the existing tab instead.
    pub fn open_path(&mut self, path: &Path) -> TabId {
        let id = self.allocate_id();
        self.tabs.push(DocumentTab::from_path(id, path));
        self.active_tab_id = Some(id);
        id
    }

    /// Open a document from a path, focusing an existing tab when possible.
    ///
    /// Paths are compared using [`normalize_path_key`](crate::tabs::normalize_path_key),
    /// so separator and case differences do not create duplicate tabs. When a
    /// match is found the existing tab is activated and no id is consumed;
    /// otherwise a new tab is allocated. Untitled documents (no path identity)
    /// always get a fresh tab.
    pub fn open_document(&mut self, path: &Path) -> OpenOutcome {
        let key = normalize_path_key(path);
        if !key.is_empty()
            && let Some(existing) = self
                .tabs
                .iter()
                .find(|tab| tab.path_key().as_deref() == Some(key.as_str()))
        {
            let id = existing.id();
            self.active_tab_id = Some(id);
            return OpenOutcome::Focused { id };
        }
        OpenOutcome::Opened {
            id: self.open_path(path),
        }
    }

    /// Close the tab with the given id and return it.
    ///
    /// If the closed tab was active, an adjacent tab becomes active (preferring
    /// the tab that followed it, then the one before it). Closing the last tab
    /// leaves no active tab. This is the unconditional close used by the model;
    /// UI paths should use [`Self::close_tab_with_policy`] so unsaved changes
    /// can be vetoed.
    pub fn close_tab(&mut self, id: TabId) -> Result<DocumentTab, AppError> {
        let index = self.index_of(id).ok_or(AppError::TabNotFound(id))?;
        Ok(self.remove_at(index))
    }

    /// Close a tab after consulting an unsaved-change policy hook.
    ///
    /// The hook is called with a borrow of the tab before any mutation. If it
    /// returns [`CloseDecision::Keep`], the workspace is unchanged and
    /// [`CloseOutcome::Cancelled`] is returned; otherwise the tab is removed,
    /// the active tab is reselected with the usual neighbor rule, and the tab
    /// is returned in [`CloseOutcome::Closed`]. An unknown id produces a typed
    /// [`AppError::TabNotFound`] without calling the hook.
    pub fn close_tab_with_policy<F>(
        &mut self,
        id: TabId,
        policy: F,
    ) -> Result<CloseOutcome, AppError>
    where
        F: FnOnce(&DocumentTab) -> CloseDecision,
    {
        let index = self.index_of(id).ok_or(AppError::TabNotFound(id))?;
        if policy(&self.tabs[index]) == CloseDecision::Keep {
            return Ok(CloseOutcome::Cancelled { id });
        }
        Ok(CloseOutcome::Closed {
            tab: self.remove_at(index),
        })
    }

    /// Make the tab with the given id active.
    pub fn activate_tab(&mut self, id: TabId) -> Result<(), AppError> {
        if self.tabs.iter().any(|tab| tab.id() == id) {
            self.active_tab_id = Some(id);
            Ok(())
        } else {
            Err(AppError::TabNotFound(id))
        }
    }

    /// Activate the next tab in insertion order, wrapping past the end.
    ///
    /// Returns the newly active id. With no active tab (or no tabs at all) a
    /// typed [`AppError::NoActiveTab`] is returned and nothing changes.
    pub fn activate_next(&mut self) -> Result<TabId, AppError> {
        let len = self.tabs.len();
        if len == 0 {
            return Err(AppError::NoActiveTab);
        }
        let current = self.active_index()?;
        let next = (current + 1) % len;
        let id = self.tabs[next].id();
        self.active_tab_id = Some(id);
        Ok(id)
    }

    /// Activate the previous tab in insertion order, wrapping past the start.
    ///
    /// Returns the newly active id. With no active tab (or no tabs at all) a
    /// typed [`AppError::NoActiveTab`] is returned and nothing changes.
    pub fn activate_previous(&mut self) -> Result<TabId, AppError> {
        let len = self.tabs.len();
        if len == 0 {
            return Err(AppError::NoActiveTab);
        }
        let current = self.active_index()?;
        let previous = (current + len - 1) % len;
        let id = self.tabs[previous].id();
        self.active_tab_id = Some(id);
        Ok(id)
    }

    /// Move a tab to a new position in insertion order.
    ///
    /// The active tab is identified by id, so reordering never changes which
    /// document is active. An out-of-range destination yields
    /// [`AppError::InvalidTabIndex`] and leaves the workspace untouched;
    /// moving a tab to its current index is a successful no-op.
    pub fn reorder_tab(&mut self, id: TabId, new_index: usize) -> Result<(), AppError> {
        let from = self.index_of(id).ok_or(AppError::TabNotFound(id))?;
        if new_index >= self.tabs.len() {
            return Err(AppError::InvalidTabIndex {
                index: new_index,
                len: self.tabs.len(),
            });
        }
        if from == new_index {
            return Ok(());
        }
        let tab = self.tabs.remove(from);
        self.tabs.insert(new_index, tab);
        Ok(())
    }

    /// Close every tab except the active one, returning their ids in order.
    ///
    /// When no tab is active the workspace is left untouched (there is no tab
    /// to keep), and an empty vector is returned.
    pub fn close_other_tabs(&mut self) -> Vec<TabId> {
        let Some(active) = self.active_tab_id else {
            return Vec::new();
        };
        let removed: Vec<TabId> = self
            .tabs
            .iter()
            .filter(|tab| tab.id() != active)
            .map(|tab| tab.id())
            .collect();
        self.tabs.retain(|tab| tab.id() == active);
        removed
    }

    /// Close every other tab after consulting a policy hook for each one.
    ///
    /// The active tab is never offered to the hook. Tabs the hook keeps remain
    /// open and are reported in [`CloseOthersOutcome::kept`]; the active id is
    /// preserved throughout. With no active tab this is a no-op.
    pub fn close_other_tabs_with_policy<F>(&mut self, policy: F) -> CloseOthersOutcome
    where
        F: Fn(&DocumentTab) -> CloseDecision,
    {
        let Some(active) = self.active_tab_id else {
            return CloseOthersOutcome::default();
        };
        let mut outcome = CloseOthersOutcome::default();
        let mut retained = Vec::with_capacity(self.tabs.len());
        for tab in self.tabs.drain(..) {
            if tab.id() == active {
                retained.push(tab);
            } else if policy(&tab) == CloseDecision::Close {
                outcome.closed.push(tab.id());
            } else {
                outcome.kept.push(tab.id());
                retained.push(tab);
            }
        }
        self.tabs = retained;
        self.active_tab_id = Some(active);
        outcome
    }

    /// A reference to the active tab, if any.
    pub fn active_tab(&self) -> Option<&DocumentTab> {
        self.active_tab_id.and_then(|id| self.tab(id))
    }

    /// A mutable reference to the active tab, if any.
    pub fn active_tab_mut(&mut self) -> Option<&mut DocumentTab> {
        let id = self.active_tab_id?;
        self.tab_mut(id)
    }

    /// A reference to the tab with the given id, if it exists.
    pub fn tab(&self, id: TabId) -> Option<&DocumentTab> {
        self.tabs.iter().find(|tab| tab.id() == id)
    }

    /// A mutable reference to the tab with the given id, if it exists.
    pub fn tab_mut(&mut self, id: TabId) -> Option<&mut DocumentTab> {
        self.tabs.iter_mut().find(|tab| tab.id() == id)
    }

    /// Iterate over all open tabs in insertion order.
    pub fn tabs(&self) -> impl Iterator<Item = &DocumentTab> {
        self.tabs.iter()
    }

    /// The number of open tabs.
    pub fn tab_count(&self) -> usize {
        self.tabs.len()
    }

    /// Whether there are no open tabs.
    pub fn is_empty(&self) -> bool {
        self.tabs.is_empty()
    }

    /// The id of the active tab, if any.
    pub fn active_tab_id(&self) -> Option<TabId> {
        self.active_tab_id
    }

    /// The currently selected tool.
    pub fn active_tool(&self) -> Toolset {
        self.active_tool
    }

    /// Select the active tool.
    pub fn select_tool(&mut self, tool: Toolset) {
        self.active_tool = tool;
    }

    /// Set the zoom of the active tab.
    pub fn set_zoom(&mut self, zoom: Zoom) -> Result<(), AppError> {
        self.active_tab_mut()
            .ok_or(AppError::NoActiveTab)?
            .set_zoom(zoom);
        Ok(())
    }

    /// Set the rotation of the active tab.
    pub fn set_rotation(&mut self, rotation: Rotation) -> Result<(), AppError> {
        self.active_tab_mut()
            .ok_or(AppError::NoActiveTab)?
            .set_rotation(rotation);
        Ok(())
    }

    /// Set the page layout mode of the active tab.
    pub fn set_view_mode(&mut self, mode: ViewMode) -> Result<(), AppError> {
        self.active_tab_mut()
            .ok_or(AppError::NoActiveTab)?
            .set_view_mode(mode);
        Ok(())
    }

    /// Save the canvas scroll offset of the active tab.
    pub fn set_scroll_offset(&mut self, offset: CanvasPoint) -> Result<(), AppError> {
        self.active_tab_mut()
            .ok_or(AppError::NoActiveTab)?
            .set_scroll_offset(offset);
        Ok(())
    }

    /// Rotate the active tab 90 degrees clockwise, returning the new rotation.
    pub fn rotate_clockwise(&mut self) -> Result<Rotation, AppError> {
        let tab = self.active_tab_mut().ok_or(AppError::NoActiveTab)?;
        tab.rotate_clockwise();
        Ok(tab.rotation())
    }

    /// Rotate the active tab 90 degrees counterclockwise.
    pub fn rotate_counterclockwise(&mut self) -> Result<Rotation, AppError> {
        let tab = self.active_tab_mut().ok_or(AppError::NoActiveTab)?;
        tab.rotate_counterclockwise();
        Ok(tab.rotation())
    }

    /// Reset the active tab's view rotation to zero.
    ///
    /// Only the per-tab *view* rotation is changed; a page's intrinsic `/Rotate`
    /// is never stored here and is never mutated. Returns the new rotation.
    pub fn reset_rotation(&mut self) -> Result<Rotation, AppError> {
        let tab = self.active_tab_mut().ok_or(AppError::NoActiveTab)?;
        tab.set_rotation(Rotation::Deg0);
        Ok(tab.rotation())
    }

    /// Set the active tab's zoom to "fit page".
    pub fn fit_page(&mut self) -> Result<(), AppError> {
        self.set_zoom(Zoom::FitPage)
    }

    /// Set the active tab's zoom to "fit width".
    pub fn fit_width(&mut self) -> Result<(), AppError> {
        self.set_zoom(Zoom::FitWidth)
    }

    /// Set the active tab's zoom to 100% actual size.
    pub fn actual_size(&mut self) -> Result<(), AppError> {
        self.set_zoom(Zoom::custom(DEFAULT_ZOOM))
    }

    /// Compute the deterministic title of a tab, if it exists.
    pub fn tab_title(&self, id: TabId) -> Option<String> {
        self.tab(id).map(|tab| tab.title())
    }

    /// Compute the deterministic title of the active tab, if any.
    pub fn active_tab_title(&self) -> Option<String> {
        self.active_tab().map(|tab| tab.title())
    }

    /// The index of `id` in insertion order, if present.
    fn index_of(&self, id: TabId) -> Option<usize> {
        self.tabs.iter().position(|tab| tab.id() == id)
    }

    /// The index of the active tab, if it is present and open.
    fn active_index(&self) -> Result<usize, AppError> {
        let id = self.active_tab_id.ok_or(AppError::NoActiveTab)?;
        self.index_of(id).ok_or(AppError::NoActiveTab)
    }

    /// Remove the tab at `index` and reselect an adjacent active tab.
    fn remove_at(&mut self, index: usize) -> DocumentTab {
        let removed = self.tabs.remove(index);
        if self.active_tab_id == Some(removed.id()) {
            self.active_tab_id = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.get(index.saturating_sub(1)))
                .map(|tab| tab.id());
        }
        removed
    }

    /// Allocate the next unique tab id.
    ///
    /// Ids are monotonic and are not reused for the lifetime of the workspace.
    fn allocate_id(&mut self) -> TabId {
        let id = TabId::from_u64(self.next_id);
        self.next_id = self.next_id.saturating_add(1);
        id
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tab_open_close_and_activate_lifecycle() {
        let mut state = AppState::new();
        assert!(state.is_empty());
        assert!(state.active_tab().is_none());
        assert_eq!(state.active_tab_title(), None);

        let a = state.open_tab("alpha.pdf");
        let b = state.open_path(Path::new(r"C:\docs\beta.pdf"));
        assert_eq!(state.tab_count(), 2);
        assert_eq!(state.active_tab_id(), Some(b));
        assert_eq!(state.active_tab_title().as_deref(), Some("beta.pdf"));
        assert_ne!(a, b);

        assert!(state.activate_tab(a).is_ok());
        assert_eq!(state.active_tab_id(), Some(a));
        assert_eq!(state.active_tab_title().as_deref(), Some("alpha.pdf"));

        let closed = state.close_tab(a).expect("tab exists");
        assert_eq!(closed.id(), a);
        assert_eq!(state.tab_count(), 1);
        // The remaining tab becomes active.
        assert_eq!(state.active_tab_id(), Some(b));

        assert_eq!(state.close_tab(b).expect("tab exists").id(), b);
        assert!(state.is_empty());
        assert_eq!(state.active_tab_id(), None);
    }

    #[test]
    fn closing_active_tab_prefers_the_following_neighbor() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        let b = state.open_tab("b");
        let c = state.open_tab("c");
        assert_eq!(state.activate_tab(a).expect("tab exists"), ());
        assert_eq!(state.close_tab(a).expect("tab exists").id(), a);
        assert_eq!(state.active_tab_id(), Some(b));
        assert!(state.tab(c).is_some());
    }

    #[test]
    fn unknown_tab_operations_return_errors() {
        let mut state = AppState::new();
        let ghost = TabId::from_u64(999);
        assert_eq!(state.activate_tab(ghost), Err(AppError::TabNotFound(ghost)));
        assert_eq!(
            state.close_tab(ghost).err(),
            Some(AppError::TabNotFound(ghost))
        );
        assert!(state.tab(ghost).is_none());
    }

    #[test]
    fn set_zoom_rotation_and_view_mode_on_active_tab() {
        let mut state = AppState::new();
        let id = state.open_tab("doc.pdf");

        state.set_zoom(Zoom::custom(3.0)).expect("active tab");
        state.set_rotation(Rotation::Deg90).expect("active tab");
        state
            .set_view_mode(ViewMode::TwoPageContinuous)
            .expect("active tab");

        let tab = state.tab(id).expect("tab exists");
        assert_eq!(tab.zoom().scale(), Some(3.0));
        assert_eq!(tab.rotation(), Rotation::Deg90);
        assert_eq!(tab.view_mode(), ViewMode::TwoPageContinuous);
    }

    #[test]
    fn rotation_commands_update_active_tab_only() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        let b = state.open_tab("b");
        assert_eq!(
            state.rotate_clockwise().expect("active tab"),
            Rotation::Deg90
        );
        assert_eq!(
            state.rotate_counterclockwise().expect("active tab"),
            Rotation::Deg0
        );

        assert!(state.activate_tab(a).is_ok());
        assert_eq!(
            state.rotate_clockwise().expect("active tab"),
            Rotation::Deg90
        );
        // The other tab kept its own rotation.
        assert_eq!(state.tab(b).map(|t| t.rotation()), Some(Rotation::Deg0));
    }

    #[test]
    fn active_tab_operations_fail_without_a_tab() {
        let mut state = AppState::new();
        assert_eq!(state.set_zoom(Zoom::FitPage), Err(AppError::NoActiveTab));
        assert_eq!(
            state.set_rotation(Rotation::Deg180),
            Err(AppError::NoActiveTab)
        );
        assert_eq!(
            state.set_view_mode(ViewMode::SinglePage),
            Err(AppError::NoActiveTab)
        );
        assert_eq!(state.rotate_clockwise(), Err(AppError::NoActiveTab));
    }

    #[test]
    fn reset_rotation_and_display_controls_update_only_the_active_tab() {
        let mut state = AppState::new();
        let a = state.open_tab("a.pdf");
        let b = state.open_tab("b.pdf");
        assert!(state.activate_tab(a).is_ok());

        // Drive tab A through a full rotation cycle, then reset it.
        for _ in 0..4 {
            state.rotate_clockwise().expect("active tab");
        }
        assert_eq!(state.tab(a).map(|t| t.rotation()), Some(Rotation::Deg0));
        state.rotate_clockwise().expect("active tab");
        assert_eq!(state.tab(a).map(|t| t.rotation()), Some(Rotation::Deg90));
        assert_eq!(state.reset_rotation(), Ok(Rotation::Deg0));
        assert_eq!(state.tab(a).map(|t| t.rotation()), Some(Rotation::Deg0));

        // Tab B never changed.
        assert_eq!(state.tab(b).map(|t| t.rotation()), Some(Rotation::Deg0));

        // Page display controls set the zoom policy on the active tab only.
        state.fit_page().expect("active tab");
        assert_eq!(state.tab(a).map(|t| t.zoom()), Some(Zoom::FitPage));
        state.fit_width().expect("active tab");
        assert_eq!(state.tab(a).map(|t| t.zoom()), Some(Zoom::FitWidth));
        state.actual_size().expect("active tab");
        assert_eq!(state.tab(a).map(|t| t.zoom()), Some(Zoom::custom(1.0)));
        assert_eq!(state.tab(b).map(|t| t.zoom()), Some(Zoom::default()));

        // No active tab produces a typed error rather than a panic.
        state.close_tab(a).expect("tab a exists");
        state.close_tab(b).expect("tab b exists");
        assert_eq!(state.reset_rotation(), Err(AppError::NoActiveTab));
    }

    #[test]
    fn rotation_state_is_isolated_and_restored_across_tab_switches() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        state.set_rotation(Rotation::Deg270).expect("active tab");
        let b = state.open_tab("b");
        // The new tab starts at the default rotation, not tab A's rotation.
        assert_eq!(state.tab(b).map(|t| t.rotation()), Some(Rotation::Deg0));
        state.set_rotation(Rotation::Deg90).expect("active tab");

        assert!(state.activate_tab(a).is_ok());
        assert_eq!(state.tab(a).map(|t| t.rotation()), Some(Rotation::Deg270));
        assert!(state.activate_tab(b).is_ok());
        assert_eq!(state.tab(b).map(|t| t.rotation()), Some(Rotation::Deg90));
    }

    #[test]
    fn tool_selection_is_global_and_independent_of_tabs() {
        let mut state = AppState::new();
        assert_eq!(state.active_tool(), Toolset::Select);
        state.select_tool(Toolset::Highlight);
        assert_eq!(state.active_tool(), Toolset::Highlight);

        let id = state.open_tab("doc");
        state.select_tool(Toolset::FormFill);
        assert_eq!(state.active_tool(), Toolset::FormFill);
        // Tool selection does not alter tab state.
        assert_eq!(state.tab_title(id).as_deref(), Some("doc"));
    }

    // ---- T-0069 workspace operations ------------------------------------

    #[test]
    fn open_document_focuses_existing_tab_for_the_same_path() {
        let mut state = AppState::new();
        let first = state.open_document(Path::new(r"C:\Docs\Report.pdf"));
        let id = match first {
            OpenOutcome::Opened { id } => id,
            other => panic!("unexpected outcome: {other:?}"),
        };
        assert_eq!(state.tab_count(), 1);

        // Different spelling/case of the same file must reuse the tab.
        let second = state.open_document(Path::new("c:/docs/report.pdf"));
        assert_eq!(second, OpenOutcome::Focused { id });
        assert_eq!(state.tab_count(), 1);
        assert_eq!(state.active_tab_id(), Some(id));
        assert_eq!(
            state.tab(id).map(|tab| tab.display_name()),
            Some("Report.pdf")
        );

        // An untitled document is never deduplicated.
        let other = state.open_document(Path::new(""));
        assert!(matches!(other, OpenOutcome::Opened { .. }));
        assert_eq!(state.tab_count(), 2);
    }

    #[test]
    fn per_tab_view_state_is_restored_when_switching_tabs() {
        let mut state = AppState::new();
        let a = state.open_tab("a.pdf");
        state.set_zoom(Zoom::custom(1.75)).expect("active tab");
        state
            .set_view_mode(ViewMode::TwoPageContinuous)
            .expect("active tab");
        state.set_rotation(Rotation::Deg90).expect("active tab");
        state
            .set_scroll_offset(CanvasPoint::new(40.0, 800.0))
            .expect("active tab");
        if let Some(tab) = state.tab_mut(a) {
            tab.set_page_count(10);
            tab.set_current_page(7);
        }

        let b = state.open_tab("b.pdf");
        state.set_zoom(Zoom::custom(0.5)).expect("active tab");
        state
            .set_view_mode(ViewMode::SinglePage)
            .expect("active tab");
        state
            .set_scroll_offset(CanvasPoint::new(0.0, 120.0))
            .expect("active tab");

        assert!(state.activate_tab(a).is_ok());
        let restored = state.tab(a).expect("tab a exists");
        assert_eq!(restored.zoom().scale(), Some(1.75));
        assert_eq!(restored.view_mode(), ViewMode::TwoPageContinuous);
        assert_eq!(restored.rotation(), Rotation::Deg90);
        assert_eq!(restored.scroll_offset(), CanvasPoint::new(40.0, 800.0));
        assert_eq!(restored.current_page(), 7);

        assert!(state.activate_tab(b).is_ok());
        let restored_b = state.tab(b).expect("tab b exists");
        assert_eq!(restored_b.zoom().scale(), Some(0.5));
        assert_eq!(restored_b.view_mode(), ViewMode::SinglePage);
        assert_eq!(restored_b.scroll_offset(), CanvasPoint::new(0.0, 120.0));
    }

    #[test]
    fn reorder_moves_tab_without_changing_the_active_id() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        let b = state.open_tab("b");
        let c = state.open_tab("c");
        assert!(state.activate_tab(a).is_ok());

        state.reorder_tab(c, 0).expect("valid index");
        let order: Vec<TabId> = state.tabs().map(|tab| tab.id()).collect();
        assert_eq!(order, vec![c, a, b]);
        assert_eq!(state.active_tab_id(), Some(a));

        assert_eq!(
            state.reorder_tab(a, 3),
            Err(AppError::InvalidTabIndex { index: 3, len: 3 })
        );
        // `a` currently sits at index 1; moving it there is a no-op.
        state.reorder_tab(a, 1).expect("same index no-op");
        let order: Vec<TabId> = state.tabs().map(|tab| tab.id()).collect();
        assert_eq!(order, vec![c, a, b]);
    }

    #[test]
    fn close_other_tabs_keeps_only_the_active_tab() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        let b = state.open_tab("b");
        let c = state.open_tab("c");
        assert!(state.activate_tab(b).is_ok());

        assert_eq!(state.close_other_tabs(), vec![a, c]);
        assert_eq!(state.tab_count(), 1);
        assert_eq!(state.active_tab_id(), Some(b));
        assert!(state.tab(a).is_none());
        assert!(state.tab(c).is_none());
    }

    #[test]
    fn next_and_previous_wrap_around_the_tab_strip() {
        let mut state = AppState::new();
        let a = state.open_tab("a");
        let b = state.open_tab("b");
        let c = state.open_tab("c");
        assert_eq!(state.active_tab_id(), Some(c));

        assert_eq!(state.activate_next(), Ok(a));
        assert_eq!(state.activate_previous(), Ok(c));
        assert_eq!(state.activate_previous(), Ok(b));

        assert!(!state.close_other_tabs().is_empty());
        let only = state.active_tab_id().expect("one tab remains");
        assert_eq!(
            state.activate_next(),
            Ok(only),
            "single tab wraps to itself"
        );
        assert_eq!(state.activate_previous(), Ok(only));

        state.close_tab(only).expect("close last tab");
        assert_eq!(state.activate_next(), Err(AppError::NoActiveTab));
        assert_eq!(state.activate_previous(), Err(AppError::NoActiveTab));
    }

    #[test]
    fn unsaved_close_policy_hook_can_veto_a_close() {
        let mut state = AppState::new();
        let a = state.open_tab("dirty.pdf");
        state.open_tab("clean.pdf");
        if let Some(tab) = state.tab_mut(a) {
            tab.set_dirty(true);
        }
        assert!(state.activate_tab(a).is_ok());

        // A hook that refuses to discard dirty documents keeps the tab open.
        let veto = |tab: &DocumentTab| {
            if tab.is_dirty() {
                CloseDecision::Keep
            } else {
                CloseDecision::Close
            }
        };
        let outcome = state.close_tab_with_policy(a, veto).expect("tab exists");
        assert!(matches!(outcome, CloseOutcome::Cancelled { id } if id == a));
        assert_eq!(state.tab_count(), 2);
        assert_eq!(state.active_tab_id(), Some(a));

        // Cleaning the document (or choosing discard) allows the close.
        if let Some(tab) = state.tab_mut(a) {
            tab.set_dirty(false);
        }
        let outcome = state.close_tab_with_policy(a, veto).expect("tab exists");
        match outcome {
            CloseOutcome::Closed { tab } => assert_eq!(tab.id(), a),
            other => panic!("unexpected outcome: {other:?}"),
        }
        assert_eq!(state.tab_count(), 1);

        // An unknown id returns a typed error and never calls the hook.
        let ghost = TabId::from_u64(999);
        assert_eq!(
            state.close_tab_with_policy(ghost, |_| CloseDecision::Close),
            Err(AppError::TabNotFound(ghost))
        );
    }

    #[test]
    fn close_others_policy_hook_keeps_vetoed_tabs() {
        let mut state = AppState::new();
        let keep = state.open_tab("keep.pdf");
        let active = state.open_tab("active.pdf");
        let dirty = state.open_tab("dirty.pdf");
        state.tab_mut(dirty).expect("dirty tab").set_dirty(true);
        assert!(state.activate_tab(active).is_ok());

        let outcome = state.close_other_tabs_with_policy(|tab: &DocumentTab| {
            if tab.is_dirty() {
                CloseDecision::Keep
            } else {
                CloseDecision::Close
            }
        });
        assert_eq!(outcome.closed, vec![keep]);
        assert_eq!(outcome.kept, vec![dirty]);
        assert_eq!(state.active_tab_id(), Some(active));
        assert_eq!(state.tab_count(), 2);
    }

    #[test]
    fn tab_ids_are_stable_and_never_reused() {
        let mut state = AppState::new();
        let first = state.open_tab("first");
        let second = state.open_tab("second");
        assert_eq!(second.as_u64(), first.as_u64() + 1);

        state.close_tab(first).expect("tab exists");
        state.close_tab(second).expect("tab exists");
        assert!(state.is_empty());

        let third = state.open_tab("third");
        assert!(third.as_u64() > second.as_u64());
        assert_ne!(third, first);
        assert_ne!(third, second);
        // Reopening the same path also allocates a fresh id after a full close.
        let reopened = state.open_document(Path::new("third.pdf"));
        match reopened {
            OpenOutcome::Opened { id } => assert!(id.as_u64() > third.as_u64()),
            other => panic!("unexpected outcome: {other:?}"),
        }
    }
}
