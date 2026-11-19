//! Owner of all open document tabs and the active tool.

use std::path::Path;

use crate::tabs::{DocumentTab, TabId};
use crate::toolset::Toolset;
use crate::view::{Rotation, ViewMode, Zoom};

/// Errors produced by tab-oriented operations on [`AppState`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppError {
    /// No tab with the requested id exists.
    TabNotFound(TabId),
    /// The operation requires an active tab, but none is set.
    NoActiveTab,
}

impl std::fmt::Display for AppError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::TabNotFound(id) => write!(f, "no open tab with id {id}"),
            Self::NoActiveTab => f.write_str("no tab is currently active"),
        }
    }
}

impl std::error::Error for AppError {}

/// The top-level, UI-agnostic state of the viewer shell.
///
/// `AppState` owns the set of open [`DocumentTab`]s, which one is active, and
/// the globally selected [`Toolset`]. View settings (layout, zoom, rotation,
/// page, dirty flag) live on each tab, so switching tabs preserves each
/// document's view independently. Ids are allocated monotonically and are not
/// reused while the application runs.
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
    /// Create an empty application state.
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
    pub fn open_path(&mut self, path: &Path) -> TabId {
        let id = self.allocate_id();
        self.tabs.push(DocumentTab::from_path(id, path));
        self.active_tab_id = Some(id);
        id
    }

    /// Close the tab with the given id and return it.
    ///
    /// If the closed tab was active, an adjacent tab becomes active (preferring
    /// the tab that followed it, then the one before it). Closing the last tab
    /// leaves no active tab.
    pub fn close_tab(&mut self, id: TabId) -> Result<DocumentTab, AppError> {
        let index = self
            .tabs
            .iter()
            .position(|tab| tab.id() == id)
            .ok_or(AppError::TabNotFound(id))?;
        let removed = self.tabs.remove(index);

        if self.active_tab_id == Some(id) {
            self.active_tab_id = self
                .tabs
                .get(index)
                .or_else(|| self.tabs.get(index.saturating_sub(1)))
                .map(|tab| tab.id());
        }
        Ok(removed)
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

    /// Iterate over all open tabs in opening order.
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

    /// Compute the deterministic title of a tab, if it exists.
    pub fn tab_title(&self, id: TabId) -> Option<String> {
        self.tab(id).map(|tab| tab.title())
    }

    /// Compute the deterministic title of the active tab, if any.
    pub fn active_tab_title(&self) -> Option<String> {
        self.active_tab().map(|tab| tab.title())
    }

    /// Allocate the next unique tab id.
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
}
