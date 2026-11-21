//! Typed shell commands and the single dispatcher every control calls.
//!
//! Widgets and keyboard shortcuts translate user intent into an [`AppCommand`]
//! and call [`crate::ui::PdfxiApp::dispatch`]. The command carries every value
//! the action needs, so the widget itself never mutates application state. This
//! gives later UI tests one stable entry point and a `label` for every action.
//!
//! Assistant panel actions (T-0253) are commands too, so no panel control calls
//! a mutating method directly: [`AppCommand::AssistantConnect`] (probe),
//! [`AppCommand::AssistantAsk`], [`AppCommand::AssistantStop`],
//! [`AppCommand::AssistantDisconnect`], [`AppCommand::AssistantSelectModel`]
//! (carries the `provider/model` label), [`AppCommand::AssistantPair`],
//! [`AppCommand::AssistantCitation`] (a one-based page cited in an answer) and
//! the two grounded presets [`AppCommand::AssistantSummarize`] and
//! [`AppCommand::AssistantKeyInsights`], with the stable labels
//! `assistant-connect`, `assistant-ask`, `assistant-stop`,
//! `assistant-disconnect`, `assistant-select-model`, `assistant-pair`,
//! `assistant-citation`, `assistant-summarize` and `assistant-key-insights`.
//!
//! `tests/ui_controls.rs` currently records the assistant variants in
//! `PENDING_COVERAGE` with a per-variant reason, not in `all_commands()` or
//! `PROVEN_LABELS`; moving them into the proven set is a separate harness
//! decision, so this module only keeps the labels stable for it.

use std::path::PathBuf;

use egui::Context;

use crate::settings::{ThemePreference, WindowControlStyle};
use crate::ui::{CaptionAction, CloseChoice, UiClosePolicy};
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
    /// Open the typed/drawn/image Fill & Sign placement card.
    PlaceSignature,
    /// Request a signature by email and track it.
    RequestSignature,
    /// Show the document properties dialog.
    DocumentProperties,
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
    /// Open the Redact and sanitize window (capability-gated).
    Redact,
    /// Open the AI assistant (capability-gated).
    AiAssistant,
    /// Open the Connect screen and start the OpenCode metadata probe.
    AssistantConnect,
    /// Send the typed question to the connected OpenCode service.
    AssistantAsk,
    /// Cancel the running assistant turn.
    AssistantStop,
    /// Return the assistant panel to the disconnected state.
    AssistantDisconnect,
    /// Select one model reported by the connected OpenCode service.
    AssistantSelectModel(String),
    /// Start the explicit "pair with a code" connection probe.
    AssistantPair,
    /// Navigate the active document to a one-based page cited in an answer.
    AssistantCitation(u32),
    /// Ask the connected assistant to summarize the open document in one sentence.
    AssistantSummarize,
    /// Ask the connected assistant for short bullets of the open document's key insights.
    AssistantKeyInsights,
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
    /// Open the help window.
    OpenHelp,
    /// Close the help window.
    CloseHelp,
    /// Toggle the local notifications window.
    ToggleNotifications,
    /// Persist a new local display name.
    SetDisplayName(String),
    /// Star or unstar a document path.
    ToggleStar(PathBuf),
    /// Remove a recent entry by path.
    RemoveRecent(PathBuf),
    /// Open the library folder in the file manager.
    OpenStorageFolder,
    /// Reveal a path in the file manager.
    RevealInExplorer(PathBuf),
    /// Open the bundled user-guide folder.
    OpenDocsFolder,
    /// Re-read the library folder and storage total.
    RefreshLibrary,
    /// Save the active document through its session.
    Save,
    /// Save the active document to a new path.
    SaveAs,
    /// Undo the newest session revision.
    Undo,
    /// Redo the most recently undone revision.
    Redo,
    /// Set the active document's Info title through a session edit.
    SetDocumentTitle(String),
    /// Submit the password currently typed in the password modal.
    SubmitPassword(String),
    /// Dismiss the password modal without opening a tab.
    CancelPasswordPrompt,
    /// Answer the unsaved-changes prompt.
    ResolveClose(CloseChoice),
    /// Overwrite a file that changed on disk.
    OverwriteDocument,
    /// Dismiss the external-modification dialog.
    DismissSaveConflict,
    /// Open the pre-print panel for the active document.
    Print,
    /// Confirm the pre-print panel and start the native dialog.
    ConfirmPrint,
    /// Cancel the running print job between pages.
    CancelPrint,
    /// Add a text-note annotation at a PDF user-space point.
    AddNote {
        /// Zero-based page.
        page: usize,
        /// PDF user-space x (points).
        x: f32,
        /// PDF user-space y (points, bottom-left origin).
        y: f32,
        /// Comment text.
        contents: String,
    },
    /// Save the open note draft.
    SaveNoteDraft,
    /// Discard the open note draft.
    CancelNoteDraft,
    /// Add a highlight over the current text selection.
    AddHighlight,
    /// Open an annotation's thread popup and scroll to its page.
    OpenAnnotation {
        /// Zero-based page.
        page: usize,
        /// Annotation `/NM` identifier.
        id: String,
    },
    /// Close the annotation thread popup.
    CloseAnnotationPopup,
    /// Reply in an annotation thread.
    ReplyToAnnotation {
        /// Zero-based page.
        page: usize,
        /// Parent annotation `/NM` identifier.
        id: String,
        /// Reply text.
        contents: String,
    },
    /// Resolve or reopen an annotation.
    SetAnnotationResolved {
        /// Zero-based page.
        page: usize,
        /// Annotation `/NM` identifier.
        id: String,
        /// New resolved state.
        resolved: bool,
    },
    /// Replace an annotation's contents.
    UpdateAnnotation {
        /// Zero-based page.
        page: usize,
        /// Annotation `/NM` identifier.
        id: String,
        /// New text.
        contents: String,
    },
    /// Delete an annotation and its replies.
    DeleteAnnotation {
        /// Zero-based page.
        page: usize,
        /// Annotation `/NM` identifier.
        id: String,
    },
    /// Commit a form text field value through the session.
    SetFormText {
        /// Fully qualified AcroForm field name.
        name: String,
        /// New value.
        value: String,
    },
    /// Toggle a form checkbox through the session.
    ToggleFormCheckbox {
        /// Fully qualified AcroForm field name.
        name: String,
    },
    /// Select a choice field option through the session.
    SetFormChoice {
        /// Fully qualified AcroForm field name.
        name: String,
        /// Selected option, or `None` to clear the field.
        value: Option<String>,
    },
    /// Select one radio button by its on-state through the session.
    SetFormRadio {
        /// Fully qualified AcroForm radio-group name.
        name: String,
        /// Named on-state of the clicked widget.
        on_state: String,
    },
    /// Commit the uncommitted text/choice draft of the focused widget.
    ApplyFormEdit,
    /// Restore every form field to its `/DV` default as one session edit.
    ResetFormFields,
    /// Flatten the form into page content as one session edit.
    FlattenForm,
    /// Focus a form widget and open its choice popup when it is a choice field.
    FocusFormWidget {
        /// Fully qualified AcroForm field name.
        name: String,
        /// Zero-based widget index inside the field.
        widget: usize,
        /// Screen anchor `[x, y]` for the option popup.
        anchor: [f32; 2],
    },
    /// Open the native picker and import XFDF field values.
    ImportXfdfDialog,
    /// Open the native picker and export field values as XFDF.
    ExportXfdfDialog,
    /// Open the native picker and import legacy FDF field values.
    ImportFdfDialog,
    /// Open the native picker and export field values as legacy FDF.
    ExportFdfDialog,
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
            Self::PlaceSignature => "place-signature",
            Self::RequestSignature => "request-signature",
            Self::DocumentProperties => "document-properties",
            Self::Organize => "organize",
            Self::Export => "export",
            Self::ConvertToPdf => "convert-to-pdf",
            Self::CombineFiles => "combine-files",
            Self::Protect => "protect",
            Self::Redact => "redact",
            Self::AiAssistant => "ai-assistant",
            Self::AssistantConnect => "assistant-connect",
            Self::AssistantAsk => "assistant-ask",
            Self::AssistantStop => "assistant-stop",
            Self::AssistantDisconnect => "assistant-disconnect",
            Self::AssistantSelectModel(_) => "assistant-select-model",
            Self::AssistantPair => "assistant-pair",
            Self::AssistantCitation(_) => "assistant-citation",
            Self::AssistantSummarize => "assistant-summarize",
            Self::AssistantKeyInsights => "assistant-key-insights",
            Self::ToggleTools => "toggle-tools",
            Self::ToggleGrid => "toggle-grid",
            Self::ToggleFullscreen => "toggle-fullscreen",
            Self::SetZoomPercent(_) => "set-zoom-percent",
            Self::SubmitPageNumber(_) => "submit-page-number",
            Self::ShowRail(_) => "show-rail",
            Self::AddPage => "add-page",
            Self::PageMenu => "page-menu",
            Self::OpenHelp => "open-help",
            Self::CloseHelp => "close-help",
            Self::ToggleNotifications => "toggle-notifications",
            Self::SetDisplayName(_) => "set-display-name",
            Self::ToggleStar(_) => "toggle-star",
            Self::RemoveRecent(_) => "remove-recent",
            Self::OpenStorageFolder => "open-storage-folder",
            Self::RevealInExplorer(_) => "reveal-in-explorer",
            Self::OpenDocsFolder => "open-docs-folder",
            Self::RefreshLibrary => "refresh-library",
            Self::Save => "save",
            Self::SaveAs => "save-as",
            Self::Undo => "undo",
            Self::Redo => "redo",
            Self::SetDocumentTitle(_) => "set-document-title",
            Self::SubmitPassword(_) => "submit-password",
            Self::CancelPasswordPrompt => "cancel-password-prompt",
            Self::ResolveClose(_) => "resolve-close",
            Self::OverwriteDocument => "overwrite-document",
            Self::DismissSaveConflict => "dismiss-save-conflict",
            Self::Print => "print",
            Self::ConfirmPrint => "confirm-print",
            Self::CancelPrint => "cancel-print",
            Self::AddNote { .. } => "add-note",
            Self::SaveNoteDraft => "save-note-draft",
            Self::CancelNoteDraft => "cancel-note-draft",
            Self::AddHighlight => "add-highlight",
            Self::OpenAnnotation { .. } => "open-annotation",
            Self::CloseAnnotationPopup => "close-annotation-popup",
            Self::ReplyToAnnotation { .. } => "reply-to-annotation",
            Self::SetAnnotationResolved { .. } => "set-annotation-resolved",
            Self::UpdateAnnotation { .. } => "update-annotation",
            Self::DeleteAnnotation { .. } => "delete-annotation",
            Self::SetFormText { .. } => "set-form-text",
            Self::ToggleFormCheckbox { .. } => "toggle-form-checkbox",
            Self::SetFormChoice { .. } => "set-form-choice",
            Self::SetFormRadio { .. } => "set-form-radio",
            Self::ApplyFormEdit => "apply-form-edit",
            Self::ResetFormFields => "reset-form-fields",
            Self::FlattenForm => "flatten-form",
            Self::FocusFormWidget { .. } => "focus-form-widget",
            Self::ImportXfdfDialog => "import-xfdf-dialog",
            Self::ExportXfdfDialog => "export-xfdf-dialog",
            Self::ImportFdfDialog => "import-fdf-dialog",
            Self::ExportFdfDialog => "export-fdf-dialog",
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
                self.request_close_tab(id);
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
            AppCommand::EditDocument => self.handle_edit_document_command(),
            AppCommand::Comment => self.start_note_tool(),
            AppCommand::Highlight => self.highlight_selection(),
            AppCommand::FillAndSign => self.open_fill_and_sign(),
            AppCommand::Sign => self.handle_sign_command(),
            AppCommand::PlaceSignature => self.handle_place_signature_command(),
            AppCommand::RequestSignature => self.handle_request_signature_command(),
            AppCommand::DocumentProperties => self.handle_document_properties_command(),
            AppCommand::Organize => self.not_available("Organize Pages"),
            AppCommand::Export => self.handle_export_command(),
            AppCommand::ConvertToPdf => self.handle_convert_command(),
            AppCommand::CombineFiles => self.not_available("Combine Files"),
            AppCommand::Protect => self.handle_protect_command(),
            AppCommand::Redact => self.handle_redact_command(),
            AppCommand::AiAssistant => self.open_assistant_connect(),
            AppCommand::AssistantConnect => self.begin_assistant_connect(),
            AppCommand::AssistantAsk => {
                self.begin_assistant_ask();
            }
            AppCommand::AssistantStop => self.stop_assistant_turn(),
            AppCommand::AssistantDisconnect => self.disconnect_assistant(),
            AppCommand::AssistantSelectModel(label) => self.select_assistant_model(label),
            AppCommand::AssistantPair => self.start_assistant_pair(),
            AppCommand::AssistantCitation(page) => self.follow_assistant_citation(page),
            AppCommand::AssistantSummarize => {
                self.begin_assistant_preset("Summarize this document in one sentence.");
            }
            AppCommand::AssistantKeyInsights => {
                self.begin_assistant_preset(
                    "List the key insights in this document as short bullets.",
                );
            }
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
            AppCommand::OpenHelp => self.help_open = true,
            AppCommand::CloseHelp => self.help_open = false,
            AppCommand::ToggleNotifications => self.toggle_notifications(),
            AppCommand::SetDisplayName(name) => self.set_display_name(name),
            AppCommand::ToggleStar(path) => {
                let starred = self.library.toggle_star(&path);
                self.status = if starred {
                    format!("Starred {}", path.display())
                } else {
                    format!("Unstarred {}", path.display())
                };
            }
            AppCommand::RemoveRecent(path) => {
                self.library.remove_recent(&path);
                self.status = format!("Removed {} from recent files", path.display());
            }
            AppCommand::OpenStorageFolder => self.open_storage_folder(),
            AppCommand::RevealInExplorer(path) => self.reveal_path(&path),
            AppCommand::OpenDocsFolder => self.open_docs_folder(),
            AppCommand::RefreshLibrary => self.refresh_library(),
            AppCommand::Save => {
                if let Err(error) = self.save_active_document() {
                    // Conflict errors already opened the dialog; other errors
                    // already set the banner.
                    if !crate::ui::save_conflict_offers_save_as(&error) {
                        self.status = error.to_string();
                    }
                }
            }
            AppCommand::SaveAs => self.save_as_dialog(),
            AppCommand::Undo => {
                if let Err(error) = self.undo_document() {
                    self.status = error.to_string();
                }
            }
            AppCommand::Redo => {
                if let Err(error) = self.redo_document() {
                    self.status = error.to_string();
                }
            }
            AppCommand::SetDocumentTitle(title) => {
                if let Err(error) = self.set_active_document_title(&title) {
                    self.status = error.to_string();
                }
            }
            AppCommand::SubmitPassword(password) => self.submit_password(password),
            AppCommand::CancelPasswordPrompt => self.cancel_password_prompt(),
            AppCommand::ResolveClose(choice) => self.resolve_close(choice),
            AppCommand::OverwriteDocument => {
                if let Err(error) = self.save_forced_active() {
                    self.status = error.to_string();
                }
            }
            AppCommand::DismissSaveConflict => self.cancel_save_conflict(),
            AppCommand::Print => self.open_print_panel(),
            AppCommand::ConfirmPrint => self.start_print_job(),
            AppCommand::CancelPrint => self.cancel_print_job(),
            AppCommand::AddNote {
                page,
                x,
                y,
                contents,
            } => match self.add_note(page, x, y, &contents) {
                Ok(()) => {
                    self.comments.error = None;
                    self.comments.hint = None;
                    self.status = "Comment added".to_owned();
                }
                Err(error) => self.comments.error = Some(error),
            },
            AppCommand::SaveNoteDraft => self.save_note_draft(),
            AppCommand::CancelNoteDraft => self.cancel_note_draft(),
            AppCommand::AddHighlight => self.highlight_selection(),
            AppCommand::OpenAnnotation { page, id } => self.open_annotation_popup(page, &id),
            AppCommand::CloseAnnotationPopup => self.close_annotation_popup(),
            AppCommand::ReplyToAnnotation { page, id, contents } => {
                self.reply_to_annotation(page, &id, &contents)
            }
            AppCommand::SetAnnotationResolved { page, id, resolved } => {
                self.set_annotation_resolved(page, &id, resolved)
            }
            AppCommand::UpdateAnnotation { page, id, contents } => {
                self.update_annotation_contents(page, &id, &contents)
            }
            AppCommand::DeleteAnnotation { page, id } => self.delete_annotation(page, &id),
            AppCommand::SetFormText { name, value } => self.set_form_text(&name, &value),
            AppCommand::ToggleFormCheckbox { name } => self.toggle_form_checkbox(&name),
            AppCommand::SetFormChoice { name, value } => {
                self.set_form_choice(&name, value.as_deref())
            }
            AppCommand::SetFormRadio { name, on_state } => self.set_form_radio(&name, &on_state),
            AppCommand::ApplyFormEdit => self.apply_form_edit(),
            AppCommand::ResetFormFields => self.reset_form_fields(),
            AppCommand::FlattenForm => self.flatten_form(),
            AppCommand::FocusFormWidget {
                name,
                widget,
                anchor,
            } => self.focus_form_widget(&name, widget, anchor),
            AppCommand::ImportXfdfDialog => self.import_xfdf_dialog(ctx),
            AppCommand::ExportXfdfDialog => self.export_xfdf_dialog(ctx),
            AppCommand::ImportFdfDialog => self.import_fdf_dialog(ctx),
            AppCommand::ExportFdfDialog => self.export_fdf_dialog(ctx),
        }
    }
}
