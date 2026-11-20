//! Typed shell commands and the single dispatcher every control calls.
//!
//! Widgets and keyboard shortcuts translate user intent into an [`AppCommand`]
//! and call [`crate::ui::PdfxiApp::dispatch`]. The command carries every value
//! the action needs, so the widget itself never mutates application state. This
//! gives later UI tests one stable entry point and a `label` for every action.

use std::path::PathBuf;

use egui::Context;

use crate::settings::{ThemePreference, WindowControlStyle};
use crate::ui::{CaptionAction, UiClosePolicy};
use crate::{TabId, Toolset, ViewMode};

use super::capabilities::RailItem;

/// One user action performed by the viewer shell.
///
/// Parameterless variants are the complete set of actions that need no
/// argument; they are dispatched by the shell's unit tests to prove that an
/// empty application never panics.
#[derive(Debug, Clone, PartialEq)]
pub enum AppCommand {
    /// Show the in-app "Open PDF" path dialog.
    OpenDialog,
    /// Open a typed or pasted path.
    OpenPath(String),
    /// Open a dropped-file batch (one tab per PDF).
    OpenDropped(Vec<PathBuf>),
    /// Close the active tab using the configured close policy.
    CloseActiveTab,
    /// Close a specific tab using the configured close policy.
    CloseTab(TabId),
    /// Close every tab except the active one.
    CloseOtherTabs,
    /// Make a specific tab active.
    ActivateTab(TabId),
    /// Activate the next tab with wraparound.
    ActivateNextTab,
    /// Activate the previous tab with wraparound.
    ActivatePreviousTab,
    /// Move a tab to a new strip position.
    ReorderTab {
        /// Tab to move.
        id: TabId,
        /// Destination index.
        new_index: usize,
    },
    /// Select the next page, clamped at the last page.
    NextPage,
    /// Select the previous page, clamped at the first page.
    PreviousPage,
    /// Select a specific page (also the thumbnail-click action).
    GoToPage(usize),
    /// Increase the active tab's zoom by one step.
    ZoomIn,
    /// Decrease the active tab's zoom by one step.
    ZoomOut,
    /// Fit the whole page in the viewport.
    FitPage,
    /// Fit the page width to the viewport.
    FitWidth,
    /// Show the page at 100% actual size.
    ActualSize,
    /// Rotate the view 90 degrees clockwise.
    RotateClockwise,
    /// Rotate the view 90 degrees counterclockwise.
    RotateCounterclockwise,
    /// Reset the view rotation to zero.
    ResetRotation,
    /// Change the active tab's view mode.
    SetViewMode(ViewMode),
    /// Select the canvas interaction tool.
    SelectTool(Toolset),
    /// Toggle the find bar open or closed.
    ToggleFind,
    /// Show the find bar.
    OpenFind,
    /// Hide the find bar.
    CloseFind,
    /// Move to the next find match.
    FindNext,
    /// Move to the previous find match.
    FindPrevious,
    /// Replace the find query and recompute matches.
    SetFindQuery(String),
    /// Replace the find option checkboxes and recompute matches.
    SetFindOptions {
        /// Match case sensitivity.
        case_sensitive: bool,
        /// Whole-word matching.
        whole_word: bool,
    },
    /// Copy the current selection to the clipboard.
    CopySelection,
    /// Clear the current text selection.
    ClearSelection,
    /// Select a theme preference and persist it.
    SetThemePreference(ThemePreference),
    /// Select a window-control style and persist it.
    SetWindowControls(WindowControlStyle),
    /// Switch between light and dark for the current resolved theme.
    ToggleTheme,
    /// Send a window-caption action.
    Caption(CaptionAction),
    /// Begin an interactive window drag.
    StartWindowDrag,
    /// Dismiss the open-error banner.
    DismissOpenError,
    /// Forget all recorded open errors.
    ClearOpenErrors,
    /// Select a thumbnail page.
    SelectThumbnailPage(usize),
    /// Replace the unsaved-close policy.
    SetClosePolicy(UiClosePolicy),
    /// Open the Edit PDF tool (capability-gated).
    EditDocument,
    /// Open the comment tool (capability-gated).
    Comment,
    /// Start a highlight (capability-gated).
    Highlight,
    /// Open Fill & Sign (capability-gated).
    FillAndSign,
    /// Open the signing flow (capability-gated).
    Sign,
    /// Open Organize Pages (capability-gated).
    Organize,
    /// Open Export (capability-gated).
    Export,
    /// Convert a file to PDF (capability-gated).
    ConvertToPdf,
    /// Combine files (capability-gated).
    CombineFiles,
    /// Open Protect PDF (capability-gated).
    Protect,
    /// Open the AI assistant (capability-gated).
    AiAssistant,
    /// Show or hide the right tools panel.
    ToggleTools,
    /// Toggle the page-thumbnail grid/list layout.
    ToggleGrid,
    /// Toggle a native full-screen viewport.
    ToggleFullscreen,
    /// Set a custom zoom percentage.
    SetZoomPercent(u32),
    /// Validate and submit the page-number box.
    SubmitPageNumber(String),
    /// Switch the left rail and side-panel content.
    ShowRail(RailItem),
    /// Add a page (capability-gated).
    AddPage,
    /// Open the page menu (capability-gated).
    PageMenu,
}

impl AppCommand {
    /// A stable, human-readable identifier for this command.
    ///
    /// The string is part of the test surface: interaction tests match controls
    /// to commands by this label, so it must not change casually. Every
    /// parameterised variant shares its family label with its siblings.
    #[must_use]
    pub fn label(&self) -> &'static str {
        match self {
            Self::OpenDialog => "open-dialog",
            Self::OpenPath(_) => "open-path",
            Self::OpenDropped(_) => "open-dropped",
            Self::CloseActiveTab => "close-active-tab",
            Self::CloseTab(_) => "close-tab",
            Self::CloseOtherTabs => "close-other-tabs",
            Self::ActivateTab(_) => "activate-tab",
            Self::ActivateNextTab => "activate-next-tab",
            Self::ActivatePreviousTab => "activate-previous-tab",
            Self::ReorderTab { .. } => "reorder-tab",
            Self::NextPage => "next-page",
            Self::PreviousPage => "previous-page",
            Self::GoToPage(_) => "goto-page",
            Self::ZoomIn => "zoom-in",
            Self::ZoomOut => "zoom-out",
            Self::FitPage => "fit-page",
            Self::FitWidth => "fit-width",
            Self::ActualSize => "actual-size",
            Self::RotateClockwise => "rotate-clockwise",
            Self::RotateCounterclockwise => "rotate-counterclockwise",
            Self::ResetRotation => "reset-rotation",
            Self::SetViewMode(_) => "set-view-mode",
            Self::SelectTool(_) => "select-tool",
            Self::ToggleFind => "toggle-find",
            Self::OpenFind => "open-find",
            Self::CloseFind => "close-find",
            Self::FindNext => "find-next",
            Self::FindPrevious => "find-previous",
            Self::SetFindQuery(_) => "set-find-query",
            Self::SetFindOptions { .. } => "set-find-options",
            Self::CopySelection => "copy-selection",
            Self::ClearSelection => "clear-selection",
            Self::SetThemePreference(_) => "set-theme-preference",
            Self::SetWindowControls(_) => "set-window-controls",
            Self::ToggleTheme => "toggle-theme",
            Self::Caption(_) => "caption",
            Self::StartWindowDrag => "start-window-drag",
            Self::DismissOpenError => "dismiss-open-error",
            Self::ClearOpenErrors => "clear-open-errors",
            Self::SelectThumbnailPage(_) => "select-thumbnail-page",
            Self::SetClosePolicy(_) => "set-close-policy",
            Self::EditDocument => "edit-document",
            Self::Comment => "comment",
            Self::Highlight => "highlight",
            Self::FillAndSign => "fill-and-sign",
            Self::Sign => "sign",
            Self::Organize => "organize",
            Self::Export => "export",
            Self::ConvertToPdf => "convert-to-pdf",
            Self::CombineFiles => "combine-files",
            Self::Protect => "protect",
            Self::AiAssistant => "ai-assistant",
            Self::ToggleTools => "toggle-tools",
            Self::ToggleGrid => "toggle-grid",
            Self::ToggleFullscreen => "toggle-fullscreen",
            Self::SetZoomPercent(_) => "set-zoom-percent",
            Self::SubmitPageNumber(_) => "submit-page-number",
            Self::ShowRail(_) => "show-rail",
            Self::AddPage => "add-page",
            Self::PageMenu => "page-menu",
        }
    }
}

impl crate::ui::PdfxiApp {
    /// Perform one [`AppCommand`].
    ///
    /// This is the only place a user action mutates application state. A GUI
    /// context is optional so headless tests can dispatch commands that do not
    /// need a window; clipboard and viewport actions are no-ops without one.
    /// Nothing here performs blocking I/O; the open dialog runs the native
    /// picker between frames, never inside a paint callback.
    pub fn dispatch(&mut self, command: AppCommand, ctx: Option<&Context>) {
        match command {
            AppCommand::OpenDialog => self.open_dialog_open = true,
            AppCommand::OpenPath(path) => {
                self.open_path_text(&path);
            }
            AppCommand::OpenDropped(paths) => {
                self.open_dropped(&paths);
            }
            AppCommand::CloseActiveTab => self.close_active_tab(),
            AppCommand::CloseTab(id) => {
                self.close_tab_by_id(id);
            }
            AppCommand::CloseOtherTabs => {
                self.close_other_tabs();
            }
            AppCommand::ActivateTab(id) => self.activate_tab(id),
            AppCommand::ActivateNextTab => {
                self.activate_next_tab();
            }
            AppCommand::ActivatePreviousTab => {
                self.activate_previous_tab();
            }
            AppCommand::ReorderTab { id, new_index } => {
                self.reorder_tab(id, new_index);
            }
            AppCommand::NextPage => self.step_page(1),
            AppCommand::PreviousPage => self.step_page(-1),
            AppCommand::GoToPage(page) => {
                self.select_thumbnail_page(page);
            }
            AppCommand::ZoomIn => self.zoom_in(),
            AppCommand::ZoomOut => self.zoom_out(),
            AppCommand::FitPage => self.fit_page(),
            AppCommand::FitWidth => self.fit_width(),
            AppCommand::ActualSize => self.actual_size(),
            AppCommand::RotateClockwise => self.rotate_clockwise(),
            AppCommand::RotateCounterclockwise => self.rotate_counterclockwise(),
            AppCommand::ResetRotation => self.reset_rotation(),
            AppCommand::SetViewMode(mode) => self.set_view_mode(mode),
            AppCommand::SelectTool(tool) => self.select_tool(tool),
            AppCommand::ToggleFind => {
                if self.find_open {
                    self.close_find();
                } else {
                    self.open_find();
                }
            }
            AppCommand::OpenFind => self.open_find(),
            AppCommand::CloseFind => self.close_find(),
            AppCommand::FindNext => self.find_next(),
            AppCommand::FindPrevious => self.find_previous(),
            AppCommand::SetFindQuery(query) => self.set_find_query(&query),
            AppCommand::SetFindOptions {
                case_sensitive,
                whole_word,
            } => self.set_find_options(case_sensitive, whole_word),
            AppCommand::CopySelection => {
                let text = self.selected_text();
                if !text.is_empty() {
                    if let Some(ctx) = ctx {
                        ctx.copy_text(text);
                    }
                    self.status = "Copied selection to clipboard".to_string();
                }
            }
            AppCommand::ClearSelection => self.clear_selection(),
            AppCommand::SetThemePreference(preference) => self.set_theme_preference(preference),
            AppCommand::SetWindowControls(style) => self.set_window_controls(style),
            AppCommand::ToggleTheme => {
                let preference = match self.theme {
                    crate::design::ThemeMode::Light => ThemePreference::Dark,
                    crate::design::ThemeMode::Dark => ThemePreference::Light,
                };
                self.set_theme_preference(preference);
            }
            AppCommand::Caption(action) => {
                if let Some(ctx) = ctx {
                    let maximized = ctx.input(|input| input.viewport().maximized.unwrap_or(false));
                    ctx.send_viewport_cmd(crate::ui::caption_command(action, maximized));
                }
            }
            AppCommand::StartWindowDrag => {
                if let Some(ctx) = ctx {
                    ctx.send_viewport_cmd(egui::ViewportCommand::StartDrag);
                }
            }
            AppCommand::DismissOpenError => self.dismiss_open_error(),
            AppCommand::ClearOpenErrors => self.clear_open_errors(),
            AppCommand::SelectThumbnailPage(page) => {
                self.select_thumbnail_page(page);
            }
            AppCommand::SetClosePolicy(policy) => self.set_close_policy(policy),
            AppCommand::EditDocument => self.not_available("Edit PDF"),
            AppCommand::Comment => self.not_available("Comment"),
            AppCommand::Highlight => self.not_available("Highlight"),
            AppCommand::FillAndSign => self.not_available("Fill & Sign"),
            AppCommand::Sign => self.not_available("Sign"),
            AppCommand::Organize => self.not_available("Organize Pages"),
            AppCommand::Export => self.not_available("Export"),
            AppCommand::ConvertToPdf => self.not_available("Convert to PDF"),
            AppCommand::CombineFiles => self.not_available("Combine Files"),
            AppCommand::Protect => self.not_available("Protect PDF"),
            AppCommand::AiAssistant => self.not_available("AI Assistant"),
            AppCommand::ToggleTools => self.toggle_tools(),
            AppCommand::ToggleGrid => self.grid_view = !self.grid_view,
            AppCommand::ToggleFullscreen => {
                if let Some(ctx) = ctx {
                    let fullscreen =
                        ctx.input(|input| input.viewport().fullscreen.unwrap_or(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Fullscreen(!fullscreen));
                }
            }
            AppCommand::SetZoomPercent(percent) => self.set_zoom_percent(percent),
            AppCommand::SubmitPageNumber(text) => {
                self.submit_page_number(&text);
            }
            AppCommand::ShowRail(item) => {
                if self.capabilities.contains(item.capability()) {
                    self.active_rail = item;
                }
            }
            AppCommand::AddPage => self.not_available("Add page"),
            AppCommand::PageMenu => self.not_available("Page menu"),
        }
    }
}
