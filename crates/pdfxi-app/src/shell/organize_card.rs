//! Organize Pages and Combine Files (A-13, A-14, A-26, A-29).
//!
//! Both windows are thin UI over the library page planner in
//! [`pdfxi_core::pages`]. A page edit clones the active session's document,
//! applies a [`PagePlan`], and commits the rebuilt document through
//! `DocumentSession::edit_full` plus
//! [`crate::ui::PdfxiApp::after_session_edit`].
//!
//! A page plan rebuilds the page tree with fresh object numbers, so its bytes
//! are a full rewrite rather than an incremental append. `DocumentSession::edit`
//! requires the old bytes to remain a prefix and rejects this shape with
//! `RevisionMismatch`; `edit_full` is the documented rewrite boundary (ADR 0010,
//! point 5). The consequence is honest and visible: a page reorganization marks
//! the tab dirty, and undo does not cross that rewrite boundary.
//!
//! Selection is the multi-select state of the Pages panel while the Organize
//! card is open: a plain click selects one page, `Ctrl` toggles a page, and
//! `Shift` fills the range from the nearest selected page. Outside the card the
//! same thumbnail click still navigates.
//!
//! Inserting a blank page is composed from the existing planner operations: the
//! shell parses a self-written one-page US-Letter template, extracts the pages
//! before and after the insertion point, and merges the template back in with
//! [`PagePlan::merge`]. The template is validated by the library parser on every
//! use, so the shell still never writes page dictionaries by hand.
//!
//! [`PageOperation`]: pdfxi_core::pages::PageOperation

use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

use pdfxi_core::Document;
use pdfxi_core::pages::{PagePlan, RotationTarget};
use pdfxi_security::Permission;
use thiserror::Error;

use crate::TabId;
use crate::ui::PdfxiApp;

/// A minimal, self-written one-page US-Letter PDF used for blank-page insert.
///
/// The template is 612x792 points with an empty content stream, an empty
/// resource dictionary and a correct cross-reference table. It is parsed by
/// [`Document::from_bytes`] at use time and never written to a file on its own,
/// so the shell cannot ship a malformed page dictionary silently.
const BLANK_PAGE_PDF: &[u8] = b"%PDF-1.4\n1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Resources << >> /Contents 4 0 R >>\nendobj\n4 0 obj\n<< /Length 0 >>\nstream\nendstream\nendobj\nxref\n0 5\n0000000000 65535 f \n0000000009 00000 n \n0000000058 00000 n \n0000000115 00000 n \n0000000219 00000 n \ntrailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n267\n%%EOF\n";

/// State of the Organize Pages card and its thumbnail selection.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct OrganizeUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// Zero-based pages selected in the Pages panel.
    pub(crate) selection: BTreeSet<usize>,
    /// The file written by the most recent extract, split or combine.
    pub(crate) last_output: Option<PathBuf>,
    /// Typed failure of the most recent page action.
    pub(crate) error: Option<String>,
}

/// State of the Combine Files card.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct CombineUiState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// PDF sources in combine order.
    pub(crate) sources: Vec<PathBuf>,
    /// Chosen output path.
    pub(crate) output: Option<PathBuf>,
    /// The file written by the most recent combine.
    pub(crate) last_output: Option<PathBuf>,
    /// Typed combine or read error.
    pub(crate) error: Option<String>,
}

/// State of the Pages-panel page menu.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PageMenuUiState {
    /// Whether the small page-actions window is visible.
    pub(crate) open: bool,
    /// Typed failure of the most recent menu action.
    pub(crate) error: Option<String>,
}

/// One action from the Pages-panel page menu.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum PageMenuAction {
    /// Insert a blank page before the current page.
    InsertBefore,
    /// Insert a blank page after the current page.
    InsertAfter,
    /// Rotate the current page one quarter turn counterclockwise.
    RotateLeft,
    /// Rotate the current page one quarter turn clockwise.
    RotateRight,
    /// Delete the current page.
    Delete,
    /// Extract the current page to `<stem>-page-N.pdf` and open it.
    Extract,
}

/// Typed blank-page insertion failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum PageInsertError {
    /// The blank-page template did not parse as a PDF.
    #[error("The blank page template is not a usable PDF: {message}")]
    BlankTemplate {
        /// Parser message.
        message: String,
    },
    /// The insertion point is outside the current page range.
    #[error("Insert position {position} is outside the document (0..={count})")]
    Position {
        /// Requested zero-based insertion point.
        position: usize,
        /// Current page count.
        count: usize,
    },
    /// Extracting the pages around the insertion point failed.
    #[error("{0}")]
    Planner(String),
}

/// Typed Combine Files failures.
#[derive(Debug, Clone, PartialEq, Eq, Error)]
pub(crate) enum CombineError {
    /// Fewer than two sources cannot produce a combined document.
    #[error("Choose at least two PDF files to combine (got {0})")]
    TooFewSources(usize),
    /// A chosen source could not be read.
    #[error("Could not read {path}: {message}")]
    Read {
        /// Source path.
        path: String,
        /// Underlying I/O message.
        message: String,
    },
    /// A chosen source is not a usable PDF.
    #[error("{path} is not a usable PDF: {message}")]
    NotPdf {
        /// Source path.
        path: String,
        /// Parser message.
        message: String,
    },
}

impl PdfxiApp {
    /// Handle [`crate::ui::AppCommand::Organize`]: open the card.
    pub(crate) fn handle_organize_command(&mut self) {
        self.organize.error = None;
        if self.state.active_tab_id().is_none() {
            self.organize.open = false;
            self.organize.selection.clear();
            self.status = "Open a document to organize".to_owned();
            return;
        }
        self.organize.open = true;
        self.organize.selection.clear();
        self.status = "Organize Pages: select pages, then use a page action".to_owned();
    }

    /// Whether the Organize Pages card is open (harness accessor).
    #[must_use]
    pub fn organize_card_open(&self) -> bool {
        self.organize.open
    }

    /// Handle [`crate::ui::AppCommand::AddPage`]: insert a blank page after the
    /// active page.
    ///
    /// With no document the command reports what is missing instead of opening
    /// a card with nothing to edit.
    pub(crate) fn handle_add_page_command(&mut self) {
        let Some(page) = self.state.active_tab().map(|tab| tab.current_page()) else {
            self.status = "Open a document to add a page".to_owned();
            return;
        };
        match self.insert_blank_page_at(page + 1) {
            Ok(_) => {
                self.page_menu.error = None;
                self.status = format!("Inserted a blank page after page {}", page + 1);
            }
            Err(error) => {
                self.page_menu.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Handle [`crate::ui::AppCommand::PageMenu`]: toggle the page-actions
    /// window next to the Pages panel.
    pub(crate) fn handle_page_menu_command(&mut self) {
        if self.state.active_tab_id().is_none() {
            self.page_menu.open = false;
            self.status = "Open a document to manage pages".to_owned();
            return;
        }
        self.page_menu.open = !self.page_menu.open;
        self.page_menu.error = None;
        self.status = if self.page_menu.open {
            "Page menu: choose a page action".to_owned()
        } else {
            "Page menu closed".to_owned()
        };
    }

    /// Whether the page menu is open (harness accessor).
    #[must_use]
    pub fn page_menu_open(&self) -> bool {
        self.page_menu.open
    }

    /// Render the small Pages-panel page menu.
    ///
    /// Every entry performs a real document edit through the same
    /// `edit_full` path as the Organize Pages card; no entry only sets status.
    pub(crate) fn page_menu_ui(&mut self, ctx: &egui::Context) {
        if !self.page_menu.open {
            return;
        }
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        let page = self.state.active_tab().map_or(0, |tab| tab.current_page());
        let mut open = true;
        let mut action = None;

        egui::Window::new("Page")
            .open(&mut open)
            .resizable(false)
            .default_width(220.0)
            .show(ctx, |ui| {
                ui.label(format!("Page {} of {}", page + 1, page_count));
                ui.separator();
                let enabled = page_count > 0;
                if ui
                    .add_enabled(enabled, egui::Button::new("Insert blank page before"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::InsertBefore);
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Insert blank page after"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::InsertAfter);
                }
                ui.separator();
                if ui
                    .add_enabled(enabled, egui::Button::new("Rotate left"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::RotateLeft);
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Rotate right"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::RotateRight);
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Delete page"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::Delete);
                }
                if ui
                    .add_enabled(enabled, egui::Button::new("Extract page"))
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    action = Some(PageMenuAction::Extract);
                }
                if let Some(error) = &self.page_menu.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });

        if let Some(action) = action {
            self.run_page_menu_action(action);
        }
        if !open {
            self.page_menu.open = false;
        }
    }

    /// Run one page-menu action against the active page.
    ///
    /// This is the non-UI entry point used by the window and by the unit tests,
    /// so every menu entry is provable without a GUI context.
    pub(crate) fn run_page_menu_action(&mut self, action: PageMenuAction) {
        let Some(page) = self.state.active_tab().map(|tab| tab.current_page()) else {
            self.status = "Open a document to manage pages".to_owned();
            return;
        };
        let result = match action {
            PageMenuAction::InsertBefore => self
                .insert_blank_page_at(page)
                .map(|_| format!("Inserted a blank page before page {}", page + 1)),
            PageMenuAction::InsertAfter => self
                .insert_blank_page_at(page + 1)
                .map(|_| format!("Inserted a blank page after page {}", page + 1)),
            PageMenuAction::RotateLeft => self.rotate_active_page(page, -1),
            PageMenuAction::RotateRight => self.rotate_active_page(page, 1),
            PageMenuAction::Delete => self.delete_active_page(page),
            PageMenuAction::Extract => self.extract_active_page(page),
        };
        match result {
            Ok(message) => {
                self.page_menu.error = None;
                self.status = message;
            }
            Err(error) => {
                self.page_menu.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Insert the blank template at zero-based `position` and commit it.
    ///
    /// `position` is an insertion point: pages before it, the blank page, then
    /// the remaining pages. The result is committed as one `edit_full` rewrite,
    /// the tab's page count is updated explicitly and the new blank page becomes
    /// current.
    fn insert_blank_page_at(&mut self, position: usize) -> Result<usize, String> {
        let (id, document) = self.page_edit_target()?;
        let count = document.page_count().map_err(|error| error.to_string())?;
        if position > count {
            return Err(PageInsertError::Position { position, count }.to_string());
        }
        let blank = blank_page_document().map_err(|error| error.to_string())?;
        let merged = if position == 0 {
            // The blank page becomes the primary document so `merge` appends
            // every original page after it.
            blank.apply_page_plan(&PagePlan::merge(vec![document]))
        } else if position == count {
            let head =
                extract_page_range(&document, 0, count).map_err(|error| error.to_string())?;
            head.apply_page_plan(&PagePlan::merge(vec![blank]))
        } else {
            let head =
                extract_page_range(&document, 0, position).map_err(|error| error.to_string())?;
            let tail = extract_page_range(&document, position, count)
                .map_err(|error| error.to_string())?;
            head.apply_page_plan(&PagePlan::merge(vec![blank, tail]))
        };
        let bytes =
            merged.map_err(|error| PageInsertError::Planner(error.to_string()).to_string())?;
        let candidate = Document::from_bytes(bytes)
            .map_err(|error| PageInsertError::Planner(error.to_string()).to_string())?;
        let pages = candidate
            .page_count()
            .map_err(|error| PageInsertError::Planner(error.to_string()).to_string())?;
        self.commit_page_candidate(id, candidate)?;
        if let Some(tab) = self.state.tab_mut(id) {
            tab.set_page_count(pages);
            tab.set_current_page(position);
        }
        self.page_box = (position + 1).to_string();
        self.pending_page_scroll = Some(position);
        self.organize.selection.clear();
        Ok(pages)
    }

    /// Rotate one page and report the action.
    fn rotate_active_page(&mut self, page: usize, quarter_turns: i32) -> Result<String, String> {
        let plan = PagePlan::rotate(RotationTarget::Indices(vec![page]), quarter_turns)
            .map_err(|error| error.to_string())?;
        let pages = self.apply_page_plan_edit(&plan)?;
        if let Some(id) = self.state.active_tab_id()
            && let Some(tab) = self.state.tab_mut(id)
        {
            tab.set_page_count(pages);
        }
        let direction = if quarter_turns < 0 { "left" } else { "right" };
        Ok(format!(
            "Rotated page {} {direction}; the document has {pages} page(s)",
            page + 1
        ))
    }

    /// Delete one page, refusing to remove the last page.
    fn delete_active_page(&mut self, page: usize) -> Result<String, String> {
        let count = self
            .active_page_count()
            .ok_or_else(|| "Open a document to manage pages".to_owned())?;
        if count <= 1 {
            return Err("Cannot delete the only page; at least one must remain".to_owned());
        }
        if page >= count {
            return Err(format!(
                "Page {} is outside the document (1..{count})",
                page + 1
            ));
        }
        let keep: Vec<usize> = (0..count).filter(|index| *index != page).collect();
        let plan = PagePlan::extract(&keep).map_err(|error| error.to_string())?;
        let pages = self.apply_page_plan_edit(&plan)?;
        let next = page.min(pages.saturating_sub(1));
        if let Some(id) = self.state.active_tab_id()
            && let Some(tab) = self.state.tab_mut(id)
        {
            tab.set_page_count(pages);
            tab.set_current_page(next);
        }
        self.page_box = (next + 1).to_string();
        Ok(format!("Deleted page {}; {pages} page(s) remain", page + 1))
    }

    /// Extract one page to the document's directory and open the result.
    fn extract_active_page(&mut self, page: usize) -> Result<String, String> {
        let source = self.page_source()?;
        let plan = PagePlan::extract(&[page]).map_err(|error| error.to_string())?;
        let bytes = source
            .apply_page_plan(&plan)
            .map_err(|error| error.to_string())?;
        let path = self.active_page_stem().map_or_else(
            || PathBuf::from(format!("page-{}.pdf", page + 1)),
            |stem| {
                self.active_page_directory()
                    .join(format!("{stem}-page-{}.pdf", page + 1))
            },
        );
        write_pdf(&path, &bytes)?;
        let _ = self.open_document(&path);
        self.notify_real_event("Page extracted", &path.display().to_string());
        Ok(format!("Extracted page {} to {}", page + 1, path.display()))
    }

    /// Apply a thumbnail click to the Organize selection.
    ///
    /// `additive` toggles one page (`Ctrl`); `range` fills from the nearest
    /// selected page to `page` (`Shift`). A plain click replaces the selection.
    pub(crate) fn organize_select_page(&mut self, page: usize, additive: bool, range: bool) {
        if !self.organize.open {
            return;
        }
        if range {
            let anchor = self
                .organize
                .selection
                .iter()
                .min_by_key(|selected| selected.abs_diff(page))
                .copied()
                .unwrap_or(page);
            let (start, end) = if anchor <= page {
                (anchor, page)
            } else {
                (page, anchor)
            };
            for index in start..=end {
                self.organize.selection.insert(index);
            }
            return;
        }
        if additive {
            if !self.organize.selection.remove(&page) {
                self.organize.selection.insert(page);
            }
            return;
        }
        self.organize.selection.clear();
        self.organize.selection.insert(page);
    }

    /// Render the Organize Pages window.
    pub(crate) fn organize_card_ui(&mut self, ctx: &egui::Context) {
        if !self.organize.open {
            return;
        }
        let mut open = true;
        let mut rotate_left = false;
        let mut rotate_right = false;
        let mut delete = false;
        let mut move_up = false;
        let mut move_down = false;
        let mut extract = false;
        let mut split = false;
        let mut insert_before = false;
        let mut insert_after = false;
        let selection_count = self.organize.selection.len();
        let action_enabled = selection_count > 0;
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        // Insert relative to the first selected page, or the active page when
        // the selection is empty.
        let target_page = self
            .organize
            .selection
            .iter()
            .next()
            .copied()
            .or_else(|| self.state.active_tab().map(|tab| tab.current_page()))
            .unwrap_or(0);

        egui::Window::new("Organize Pages")
            .open(&mut open)
            .resizable(true)
            .default_width(360.0)
            .show(ctx, |ui| {
                ui.heading("Organize Pages");
                ui.label(
                    egui::RichText::new(
                        "Click a thumbnail to select it. Ctrl-click toggles a page; \
                         Shift-click selects a range.",
                    )
                    .size(10.0)
                    .weak(),
                );
                ui.label(format!(
                    "{selection_count} of {page_count} page(s) selected"
                ));
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(action_enabled, egui::Button::new("Rotate left"))
                        .on_disabled_hover_text("Select at least one page")
                        .clicked()
                    {
                        rotate_left = true;
                    }
                    if ui
                        .add_enabled(action_enabled, egui::Button::new("Rotate right"))
                        .on_disabled_hover_text("Select at least one page")
                        .clicked()
                    {
                        rotate_right = true;
                    }
                });
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(action_enabled, egui::Button::new("Delete"))
                        .on_disabled_hover_text("Select at least one page")
                        .clicked()
                    {
                        delete = true;
                    }
                    if ui
                        .add_enabled(action_enabled, egui::Button::new("Move up"))
                        .on_disabled_hover_text("Select at least one page")
                        .clicked()
                    {
                        move_up = true;
                    }
                    if ui
                        .add_enabled(action_enabled, egui::Button::new("Move down"))
                        .on_disabled_hover_text("Select at least one page")
                        .clicked()
                    {
                        move_down = true;
                    }
                });
                ui.separator();
                if ui
                    .add_enabled(
                        !self.is_empty_document(),
                        egui::Button::new("Insert blank page before"),
                    )
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    insert_before = true;
                }
                if ui
                    .add_enabled(
                        !self.is_empty_document(),
                        egui::Button::new("Insert blank page after"),
                    )
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    insert_after = true;
                }
                if ui
                    .add_enabled(action_enabled, egui::Button::new("Extract selected"))
                    .on_disabled_hover_text("Select at least one page")
                    .clicked()
                {
                    extract = true;
                }
                if ui
                    .add_enabled(
                        !self.is_empty_document(),
                        egui::Button::new("Split each page"),
                    )
                    .on_disabled_hover_text("Open a document with pages")
                    .clicked()
                {
                    split = true;
                }
                if let Some(path) = &self.organize.last_output {
                    ui.label(format!("Last output: {}", path.display()));
                }
                if let Some(error) = &self.organize.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
            });

        if rotate_left {
            match self.organize_rotate(-1) {
                Ok(()) => {}
                Err(error) => {
                    self.organize.error = Some(error.clone());
                    self.status = error;
                }
            }
        }
        if rotate_right {
            match self.organize_rotate(1) {
                Ok(()) => {}
                Err(error) => {
                    self.organize.error = Some(error.clone());
                    self.status = error;
                }
            }
        }
        if delete {
            self.organize_delete();
        }
        if move_up {
            self.organize_move(true);
        }
        if move_down {
            self.organize_move(false);
        }
        if extract {
            let path = self
                .active_page_stem()
                .map(|stem| {
                    self.active_page_directory()
                        .join(format!("{stem}-extract.pdf"))
                })
                .unwrap_or_else(|| PathBuf::from("extract.pdf"));
            self.organize_extract(&path);
        }
        if split {
            let dir = self.active_page_directory();
            self.organize_split(&dir);
        }
        if insert_before {
            match self.insert_blank_page_at(target_page) {
                Ok(_) => {
                    self.status = format!("Inserted a blank page before page {}", target_page + 1);
                }
                Err(error) => {
                    self.organize.error = Some(error.clone());
                    self.status = error;
                }
            }
        }
        if insert_after {
            match self.insert_blank_page_at(target_page + 1) {
                Ok(_) => {
                    self.status = format!("Inserted a blank page after page {}", target_page + 1);
                }
                Err(error) => {
                    self.organize.error = Some(error.clone());
                    self.status = error;
                }
            }
        }
        if !open {
            self.organize.open = false;
        }
    }

    /// Rotate the selected pages by `quarter_turns` (negative is left).
    ///
    /// The committed plan is `PagePlan::rotate(RotationTarget::Indices(selection),
    /// quarter_turns)`.
    pub(crate) fn organize_rotate(&mut self, quarter_turns: i32) -> Result<(), String> {
        let selection = self.selected_page_indices()?;
        let count = selection.len();
        let plan = PagePlan::rotate(RotationTarget::Indices(selection), quarter_turns)
            .map_err(|error| error.to_string())?;
        let pages = self.apply_page_plan_edit(&plan)?;
        self.organize.error = None;
        let direction = if quarter_turns < 0 { "left" } else { "right" };
        self.status =
            format!("Rotated {count} page(s) {direction}; the document has {pages} page(s)");
        Ok(())
    }

    /// Delete the selected pages, keeping the rest in original order.
    pub(crate) fn organize_delete(&mut self) {
        match self.delete_selected_pages() {
            Ok(deleted) => {
                self.organize.error = None;
                self.status = format!(
                    "Deleted {deleted} page(s); {} remain",
                    self.active_page_count().unwrap_or(0)
                );
            }
            Err(error) => {
                self.organize.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Move the selected pages one position up or down among the full order.
    pub(crate) fn organize_move(&mut self, up: bool) {
        match self.move_selected_pages(up) {
            Ok(0) => {
                self.organize.error = None;
                self.status = format!(
                    "The selected page(s) are already at the {}",
                    if up { "top" } else { "bottom" }
                );
            }
            Ok(moved) => {
                self.organize.error = None;
                self.status = format!("Moved {moved} page(s) {}", if up { "up" } else { "down" });
            }
            Err(error) => {
                self.organize.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Extract the selected pages to `path` and open the result.
    pub(crate) fn organize_extract(&mut self, path: &Path) {
        match self.write_extracted_pages(path) {
            Ok(pages) => {
                self.organize.error = None;
                self.organize.last_output = Some(path.to_path_buf());
                self.status = format!("Extracted {pages} page(s) to {}", path.display());
                self.notify_real_event("Pages extracted", &path.display().to_string());
            }
            Err(error) => {
                self.organize.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Split every page into its own PDF under `dir`.
    pub(crate) fn organize_split(&mut self, dir: &Path) {
        match self.write_split_pages(dir) {
            Ok(count) => {
                self.organize.error = None;
                let last = self.organize.last_output.clone();
                self.status = format!(
                    "Split into {count} one-page PDF(s){}",
                    last.map_or_else(String::new, |path| format!(
                        " in {}",
                        path.parent().map_or_else(
                            || path.display().to_string(),
                            |parent| parent.display().to_string()
                        )
                    ))
                );
                self.notify_real_event("Document split", &dir.display().to_string());
            }
            Err(error) => {
                self.organize.error = Some(error.clone());
                self.status = error;
            }
        }
    }

    /// Combine `sources` into `output`, open it, and return the page count.
    ///
    /// Reads each PDF into a [`Document`], merges every document after the
    /// first into the first with [`PagePlan::merge`], applies that plan to the
    /// first document, writes the bytes and opens the result.
    pub(crate) fn combine_to_path(
        &mut self,
        sources: &[PathBuf],
        output: &Path,
    ) -> Result<usize, String> {
        let result = self.write_combined(sources, output);
        match result {
            Ok(pages) => {
                self.combine.error = None;
                self.combine.last_output = Some(output.to_path_buf());
                self.organize.last_output = Some(output.to_path_buf());
                self.status = format!(
                    "Combined {} files into {pages} page(s) at {}",
                    sources.len(),
                    output.display()
                );
                self.notify_real_event("PDFs combined", &output.display().to_string());
                let _ = self.open_document(output);
                Ok(pages)
            }
            Err(error) => {
                self.combine.error = Some(error.clone());
                self.status = format!("Combine failed: {error}");
                Err(error)
            }
        }
    }

    /// Handle [`crate::ui::AppCommand::CombineFiles`]: open the card.
    pub(crate) fn handle_combine_command(&mut self) {
        self.combine.open = true;
        self.combine.error = None;
        self.status =
            "Combine Files: add at least two PDFs, choose an output, then Combine".to_owned();
    }

    /// Whether the Combine Files card is open (harness accessor).
    #[must_use]
    pub fn combine_card_open(&self) -> bool {
        self.combine.open
    }

    /// Show the native multi-file picker and add the chosen PDFs.
    pub(crate) fn choose_combine_sources(&mut self) {
        let Some(paths) = rfd::FileDialog::new()
            .set_title("Add PDFs to combine")
            .add_filter("PDF documents", &["pdf"])
            .pick_files()
        else {
            return;
        };
        self.add_combine_sources(&paths);
    }

    /// Append `paths` to the combine list, suggesting an output for the first.
    ///
    /// Returns the number of sources now queued.
    pub(crate) fn add_combine_sources(&mut self, paths: &[PathBuf]) -> usize {
        for path in paths {
            self.combine.sources.push(path.clone());
        }
        if self.combine.output.is_none()
            && let Some(first) = paths.first()
        {
            self.combine.output = Some(first.with_file_name("combined.pdf"));
        }
        self.combine.error = None;
        self.combine.sources.len()
    }

    /// Remove the source at `index`.
    pub(crate) fn remove_combine_source(&mut self, index: usize) {
        if index < self.combine.sources.len() {
            self.combine.sources.remove(index);
        }
    }

    /// Show the native save dialog for the combined output.
    pub(crate) fn choose_combine_output(&mut self) {
        let suggested = self
            .combine
            .output
            .clone()
            .and_then(|path| path.file_name().map(|name| name.to_owned()))
            .and_then(|name| name.to_str().map(str::to_owned))
            .unwrap_or_else(|| "combined.pdf".to_owned());
        if let Some(path) = rfd::FileDialog::new()
            .set_title("Save the combined PDF")
            .add_filter("PDF documents", &["pdf"])
            .set_file_name(suggested)
            .save_file()
        {
            self.combine.output = Some(path);
        }
    }

    /// Render the Combine Files window.
    pub(crate) fn combine_card_ui(&mut self, ctx: &egui::Context) {
        if !self.combine.open {
            return;
        }
        let mut open = true;
        let mut add_files = false;
        let mut choose_output = false;
        let mut combine = false;
        let mut remove = None;

        egui::Window::new("Combine Files")
            .open(&mut open)
            .resizable(true)
            .default_width(420.0)
            .show(ctx, |ui| {
                ui.heading("Combine Files");
                ui.label(
                    egui::RichText::new("Add two or more PDFs. Pages keep the order shown here.")
                        .size(10.0)
                        .weak(),
                );
                if ui.button("Add files").clicked() {
                    add_files = true;
                }
                if self.combine.sources.is_empty() {
                    ui.label("No PDFs added");
                }
                for (index, path) in self.combine.sources.iter().enumerate() {
                    ui.horizontal(|ui| {
                        ui.label(path.display().to_string());
                        if ui.button("Remove").clicked() {
                            remove = Some(index);
                        }
                    });
                }
                ui.separator();
                let output = self.combine.output.as_ref().map_or_else(
                    || "No output chosen".to_owned(),
                    |path| path.display().to_string(),
                );
                ui.label(format!("Output: {output}"));
                if ui.button("Choose output").clicked() {
                    choose_output = true;
                }
                if let Some(error) = &self.combine.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.separator();
                let ready = self.combine.sources.len() >= 2 && self.combine.output.is_some();
                if ui
                    .add_enabled(ready, egui::Button::new("Combine"))
                    .on_disabled_hover_text("Add at least two PDFs and choose an output")
                    .clicked()
                {
                    combine = true;
                }
            });

        if add_files {
            self.choose_combine_sources();
        }
        if choose_output {
            self.choose_combine_output();
        }
        if let Some(index) = remove {
            self.remove_combine_source(index);
        }
        if combine && let Some(output) = self.combine.output.clone() {
            let _ = self.combine_to_path(&self.combine.sources.clone(), &output);
        }
        if !open {
            self.combine.open = false;
        }
    }

    /// The selected zero-based pages, or a typed "select a page" message.
    fn selected_page_indices(&self) -> Result<Vec<usize>, String> {
        if self.organize.selection.is_empty() {
            return Err("Select at least one page".to_owned());
        }
        Ok(self.organize.selection.iter().copied().collect())
    }

    /// The active document cloned for a page mutation, with permission checks.
    fn page_edit_target(&self) -> Result<(TabId, Document), String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to organize".to_owned());
        };
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        let Some(session) = self.sessions.get(&id) else {
            return Err("The document has no open session".to_owned());
        };
        if let Some(report) = session.decryption_report()
            && !report.permissions.allows(Permission::MODIFY)
        {
            return Err("The document permissions do not allow changes".to_owned());
        }
        Ok((id, session.document().clone()))
    }

    /// The active document cloned for a read-only page export.
    fn page_source(&self) -> Result<Document, String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document to organize".to_owned());
        };
        self.sessions
            .get(&id)
            .map(|session| session.document().clone())
            .ok_or_else(|| "The document has no open session".to_owned())
    }

    /// Page count of the active session document, if any.
    fn active_page_count(&self) -> Option<usize> {
        let id = self.state.active_tab_id()?;
        self.sessions
            .get(&id)
            .and_then(|session| session.document().page_count().ok())
    }

    /// Whether the active tab has no pages to work on.
    fn is_empty_document(&self) -> bool {
        self.active_page_count().unwrap_or(0) == 0
    }

    /// File stem of the active document, or `document`.
    fn active_page_stem(&self) -> Option<String> {
        self.state
            .active_tab()
            .and_then(|tab| tab.path())
            .and_then(|path| path.file_stem())
            .map(|stem| stem.to_string_lossy().into_owned())
    }

    /// Directory of the active document, or the working directory.
    fn active_page_directory(&self) -> PathBuf {
        self.state
            .active_tab()
            .and_then(|tab| tab.path())
            .and_then(|path| path.parent())
            .filter(|parent| !parent.as_os_str().is_empty())
            .map_or_else(|| PathBuf::from("."), Path::to_path_buf)
    }

    /// Apply `plan` to a clone of the active document and commit the result.
    fn apply_page_plan_edit(&mut self, plan: &PagePlan) -> Result<usize, String> {
        let (id, document) = self.page_edit_target()?;
        let bytes = document
            .apply_page_plan(plan)
            .map_err(|error| error.to_string())?;
        let candidate = Document::from_bytes(bytes).map_err(|error| error.to_string())?;
        let pages = candidate.page_count().map_err(|error| error.to_string())?;
        self.commit_page_candidate(id, candidate)?;
        Ok(pages)
    }

    /// Commit a reparsed page document as one session rewrite.
    ///
    /// A page plan rebuilds the page tree with fresh object numbers, so its
    /// bytes are a full rewrite, not an incremental append. `session.edit`
    /// rejects that shape (`RevisionMismatch`); `edit_full` is the session's
    /// documented rewrite boundary (ADR 0010, point 5), so undo does not cross
    /// a page reorganization and the tab is dirty afterwards.
    fn commit_page_candidate(&mut self, id: TabId, candidate: Document) -> Result<(), String> {
        let result = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit_full(|document| {
                *document = candidate;
                Ok(())
            });
        match result {
            Ok(()) => {
                self.after_session_edit(id);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }

    /// Delete the selection and return how many pages were removed.
    fn delete_selected_pages(&mut self) -> Result<usize, String> {
        let selection = self.selected_page_indices()?;
        let source = self.page_source()?;
        let count = source.page_count().map_err(|error| error.to_string())?;
        let keep: Vec<usize> = (0..count)
            .filter(|index| !self.organize.selection.contains(index))
            .collect();
        if keep.is_empty() {
            return Err("Cannot delete every page; at least one must remain".to_owned());
        }
        let removed = selection.len();
        let plan = PagePlan::extract(&keep).map_err(|error| error.to_string())?;
        self.apply_page_plan_edit(&plan)?;
        self.organize.selection.clear();
        Ok(removed)
    }

    /// Reorder the full page order by one position per selected page.
    fn move_selected_pages(&mut self, up: bool) -> Result<usize, String> {
        self.selected_page_indices()?;
        let source = self.page_source()?;
        let count = source.page_count().map_err(|error| error.to_string())?;
        let selected: BTreeSet<usize> = self
            .organize
            .selection
            .iter()
            .copied()
            .filter(|index| *index < count)
            .collect();
        let mut order: Vec<usize> = (0..count).collect();
        let mut moved = 0usize;
        if up {
            for index in selected.iter().copied() {
                if index == 0 || selected.contains(&(index - 1)) {
                    continue;
                }
                let position = order
                    .iter()
                    .position(|page| *page == index)
                    .ok_or_else(|| format!("Page {index} is missing from the order"))?;
                if position > 0 {
                    order.swap(position, position - 1);
                    moved += 1;
                }
            }
        } else {
            for index in selected.iter().rev().copied() {
                if index + 1 >= count || selected.contains(&(index + 1)) {
                    continue;
                }
                let position = order
                    .iter()
                    .position(|page| *page == index)
                    .ok_or_else(|| format!("Page {index} is missing from the order"))?;
                if position + 1 < order.len() {
                    order.swap(position, position + 1);
                    moved += 1;
                }
            }
        }
        if moved > 0 {
            let new_selection: BTreeSet<usize> = selected
                .iter()
                .filter_map(|page| order.iter().position(|entry| entry == page))
                .collect();
            let plan = PagePlan::reorder(&order).map_err(|error| error.to_string())?;
            self.apply_page_plan_edit(&plan)?;
            self.organize.selection = new_selection;
        }
        Ok(moved)
    }

    /// Apply the extract plan and write/open the result.
    fn write_extracted_pages(&mut self, path: &Path) -> Result<usize, String> {
        let selection = self.selected_page_indices()?;
        let source = self.page_source()?;
        let count = source.page_count().map_err(|error| error.to_string())?;
        if let Some(index) = selection.iter().find(|index| **index >= count) {
            return Err(format!(
                "Selected page {} is outside the document (0..{count})",
                index + 1
            ));
        }
        let plan = PagePlan::extract(&selection).map_err(|error| error.to_string())?;
        let bytes = source
            .apply_page_plan(&plan)
            .map_err(|error| error.to_string())?;
        write_pdf(path, &bytes)?;
        let pages = Document::from_bytes(bytes)
            .and_then(|document| document.page_count())
            .map_err(|error| error.to_string())?;
        let _ = self.open_document(path);
        Ok(pages)
    }

    /// Apply one plan per page and write `<stem>-page-N.pdf` under `dir`.
    fn write_split_pages(&mut self, dir: &Path) -> Result<usize, String> {
        let source = self.page_source()?;
        let count = source.page_count().map_err(|error| error.to_string())?;
        if count == 0 {
            return Err("The document has no pages to split".to_owned());
        }
        let stem = self
            .active_page_stem()
            .unwrap_or_else(|| "document".to_owned());
        let chunks: Vec<Vec<usize>> = (0..count).map(|page| vec![page]).collect();
        let plans = PagePlan::split(&chunks).map_err(|error| error.to_string())?;
        std::fs::create_dir_all(dir)
            .map_err(|error| format!("Could not create {}: {error}", dir.display()))?;
        let mut last = None;
        for (index, plan) in plans.iter().enumerate() {
            let bytes = source
                .apply_page_plan(plan)
                .map_err(|error| error.to_string())?;
            let path = dir.join(format!("{stem}-page-{}.pdf", index + 1));
            write_pdf(&path, &bytes)?;
            last = Some(path);
        }
        self.organize.last_output = last;
        Ok(plans.len())
    }

    /// Read, merge and write `sources`; return the reopened page count.
    fn write_combined(&mut self, sources: &[PathBuf], output: &Path) -> Result<usize, String> {
        if sources.len() < 2 {
            return Err(CombineError::TooFewSources(sources.len()).to_string());
        }
        let mut documents = Vec::with_capacity(sources.len());
        for path in sources {
            let bytes = std::fs::read(path).map_err(|error| {
                CombineError::Read {
                    path: path.display().to_string(),
                    message: error.to_string(),
                }
                .to_string()
            })?;
            let document = Document::from_bytes(bytes).map_err(|error| {
                CombineError::NotPdf {
                    path: path.display().to_string(),
                    message: error.to_string(),
                }
                .to_string()
            })?;
            if document.page_count().map_err(|error| error.to_string())? == 0 {
                return Err(format!("{} has no pages", path.display()));
            }
            documents.push(document);
        }
        let primary = documents.remove(0);
        let plan = PagePlan::merge(documents);
        let bytes = primary
            .apply_page_plan(&plan)
            .map_err(|error| error.to_string())?;
        write_pdf(output, &bytes)?;
        Document::from_bytes(bytes)
            .and_then(|document| document.page_count())
            .map_err(|error| error.to_string())
    }
}

/// Write `bytes` to `path`, creating missing parent directories.
fn write_pdf(path: &Path, bytes: &[u8]) -> Result<(), String> {
    if let Some(parent) = path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("Could not create {}: {error}", parent.display()))?;
    }
    std::fs::write(path, bytes)
        .map_err(|error| format!("Could not write {}: {error}", path.display()))
}

/// Parse the blank-page template into a document.
fn blank_page_document() -> Result<Document, PageInsertError> {
    Document::from_bytes(BLANK_PAGE_PDF.to_vec()).map_err(|error| PageInsertError::BlankTemplate {
        message: error.to_string(),
    })
}

/// Keep pages `start..end` of `document` as a new document.
fn extract_page_range(
    document: &Document,
    start: usize,
    end: usize,
) -> Result<Document, PageInsertError> {
    let indices: Vec<usize> = (start..end).collect();
    let plan =
        PagePlan::extract(&indices).map_err(|error| PageInsertError::Planner(error.to_string()))?;
    let bytes = document
        .apply_page_plan(&plan)
        .map_err(|error| PageInsertError::Planner(error.to_string()))?;
    Document::from_bytes(bytes).map_err(|error| PageInsertError::Planner(error.to_string()))
}

#[cfg(test)]
mod tests {
    use super::*;
    use pdfxi_core::pages::effective_rotation;
    use std::sync::atomic::{AtomicU64, Ordering};

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld")
            .join(name)
    }

    fn temp_dir(tag: &str) -> PathBuf {
        static COUNTER: AtomicU64 = AtomicU64::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/organize")
            .join(format!("{}-{unique}-{tag}", std::process::id()));
        std::fs::create_dir_all(&dir).expect("create the organize directory");
        dir
    }

    fn open_fixture(name: &str) -> PdfxiApp {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture(name));
        app
    }

    fn save_and_reopen(app: &mut PdfxiApp, path: &Path) -> Document {
        app.save_active_as(path).expect("save the edited document");
        let bytes = std::fs::read(path).expect("read the saved document");
        Document::from_bytes(bytes).expect("reopen the saved document")
    }

    fn page_text(document: &Document, index: usize) -> String {
        document
            .page_text(index)
            .unwrap_or_else(|error| panic!("page {index} text: {error}"))
    }

    #[test]
    fn organize_command_needs_a_document_then_opens_the_card() {
        let mut app = PdfxiApp::new();
        app.dispatch(crate::ui::AppCommand::Organize, None);
        assert!(!app.organize_card_open());
        assert_eq!(app.status(), "Open a document to organize");

        let mut app = open_fixture("powerpoint-slides.pdf");
        assert!(!app.organize_card_open());
        app.dispatch(crate::ui::AppCommand::Organize, None);
        assert!(app.organize_card_open());
        assert!(app.organize.selection.is_empty());
    }

    #[test]
    fn thumbnail_click_selects_inside_organize_and_navigates_outside() {
        let mut app = open_fixture("powerpoint-slides.pdf");
        app.dispatch(crate::ui::AppCommand::SelectThumbnailPage(1), None);
        assert_eq!(
            app.state.active_tab().map(|tab| tab.current_page()),
            Some(1)
        );

        app.organize.open = true;
        app.dispatch(crate::ui::AppCommand::SelectThumbnailPage(2), None);
        app.organize_select_page(4, true, false);
        app.organize_select_page(6, false, true);
        assert_eq!(
            app.organize.selection.iter().copied().collect::<Vec<_>>(),
            vec![2, 4, 5, 6]
        );
        assert_eq!(
            app.state.active_tab().map(|tab| tab.current_page()),
            Some(1),
            "thumbnail clicks do not navigate while Organize is open"
        );
    }

    #[test]
    fn rotate_page_zero_by_ninety_degrees_commits_and_reopts() {
        let dir = temp_dir("rotate");
        let mut app = open_fixture("powerpoint-slides.pdf");
        app.organize.open = true;
        app.organize.selection.insert(0);
        app.organize_rotate(1).expect("rotate page 0");

        let path = dir.join("rotated.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert_eq!(effective_rotation(&document, 0).expect("rotation"), 90);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn rotate_left_is_counterclockwise() {
        let dir = temp_dir("rotate-left");
        let mut app = open_fixture("powerpoint-slides.pdf");
        app.organize.open = true;
        app.organize.selection.insert(0);
        app.organize_rotate(-1).expect("rotate page 0 left");

        let path = dir.join("rotated-left.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert_eq!(effective_rotation(&document, 0).expect("rotation"), 270);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn deleting_page_one_removes_its_text_and_page() {
        let dir = temp_dir("delete");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let source_pages = app
            .page_source()
            .expect("source document")
            .page_count()
            .expect("page count");
        let source = app.page_source().expect("source");
        assert!(page_text(&source, 1).contains("harbor slide marker"));

        app.organize.open = true;
        app.organize.selection.insert(1);
        app.organize_delete();

        let path = dir.join("deleted.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert_eq!(document.page_count().expect("page count"), source_pages - 1);
        assert_eq!(
            page_text(&document, 1).trim(),
            page_text(&source, 2).trim(),
            "the old page 2 follows the deleted page"
        );
        let all_text: String = (0..document.page_count().expect("count"))
            .map(|index| page_text(&document, index))
            .collect();
        assert!(
            !all_text.contains("harbor slide marker"),
            "deleted page text survived: {all_text:?}"
        );
        assert!(app.organize.selection.is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reorder_two_zero_one_puts_old_page_two_first() {
        let dir = temp_dir("reorder");
        let mut app = open_fixture("powerpoint-slides.pdf");

        // Move page 2 up twice: [0,1,2] -> [0,2,1] -> [2,0,1].
        app.organize.open = true;
        app.organize.selection.insert(2);
        app.organize_move(true);
        assert_eq!(
            app.organize.selection.iter().copied().collect::<Vec<_>>(),
            vec![1]
        );
        app.organize_move(true);

        let path = dir.join("reordered.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert!(
            page_text(&document, 0).contains("meadow slide marker"),
            "page 0 should carry the old page 2 text: {:?}",
            page_text(&document, 0)
        );
        assert!(page_text(&document, 1).contains("lantern slide marker"));
        assert!(page_text(&document, 2).contains("harbor slide marker"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn moving_down_from_the_bottom_is_a_no_op() {
        let mut app = open_fixture("powerpoint-slides.pdf");
        let source = app.page_source().expect("source");
        let count = source.page_count().expect("page count");
        let last = count - 1;
        let before = page_text(&source, last);

        app.organize.open = true;
        app.organize.selection.insert(last);
        app.organize_move(false);

        assert!(app.status().contains("already at the bottom"));
        let after = page_text(&app.page_source().expect("source"), last);
        assert_eq!(before, after);
    }

    #[test]
    fn extracting_page_one_writes_a_one_page_pdf() {
        let dir = temp_dir("extract");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let source = app.page_source().expect("source");
        assert!(page_text(&source, 1).contains("harbor slide marker"));

        app.organize.open = true;
        app.organize.selection.insert(1);
        let path = dir.join("slides-extract.pdf");
        app.organize_extract(&path);

        assert!(app.organize.error.is_none(), "{:?}", app.organize.error);
        assert_eq!(app.organize.last_output.as_deref(), Some(path.as_path()));
        let bytes = std::fs::read(&path).expect("read extracted PDF");
        let document = Document::from_bytes(bytes).expect("reopen extracted PDF");
        assert_eq!(document.page_count().expect("page count"), 1);
        assert!(page_text(&document, 0).contains("harbor slide marker"));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn splitting_writes_one_file_per_page() {
        let dir = temp_dir("split");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let count = app.active_page_count().expect("the fixture has pages");
        assert_eq!(count, 3, "the PowerPoint fixture has three slides");

        app.organize.open = true;
        app.organize_split(&dir);
        assert!(app.organize.error.is_none(), "{:?}", app.organize.error);

        let mut written: Vec<PathBuf> = std::fs::read_dir(&dir)
            .expect("read the split directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .collect();
        written.sort();
        assert_eq!(written.len(), count);
        for path in &written {
            let bytes = std::fs::read(path).expect("read split page");
            let document = Document::from_bytes(bytes).expect("reopen split page");
            assert_eq!(document.page_count().expect("page count"), 1);
        }
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn combine_sums_pages_and_keeps_text_from_both_files() {
        let dir = temp_dir("combine");
        let slides = fixture("powerpoint-slides.pdf");
        let excel = fixture("microsoft-excel-2019-sample.pdf");
        let slides_pages = Document::from_bytes(std::fs::read(&slides).expect("slides bytes"))
            .expect("slides PDF")
            .page_count()
            .expect("slides count");
        let excel_pages = Document::from_bytes(std::fs::read(&excel).expect("excel bytes"))
            .expect("excel PDF")
            .page_count()
            .expect("excel count");

        let mut app = PdfxiApp::new();
        app.dispatch(crate::ui::AppCommand::CombineFiles, None);
        assert!(app.combine_card_open());
        assert_eq!(app.add_combine_sources(&[slides.clone(), excel.clone()]), 2);

        let output = dir.join("combined.pdf");
        let pages = app
            .combine_to_path(&[slides, excel], &output)
            .expect("combine the PDFs");
        assert_eq!(pages, slides_pages + excel_pages);
        assert!(app.combine.error.is_none(), "{:?}", app.combine.error);

        let bytes = std::fs::read(&output).expect("read the combined PDF");
        let document = Document::from_bytes(bytes).expect("reopen the combined PDF");
        assert_eq!(document.page_count().expect("page count"), pages);
        let all_text: String = (0..pages)
            .map(|index| page_text(&document, index))
            .collect();
        assert!(
            all_text.contains("lantern slide marker"),
            "the slides text is missing from {all_text:?}"
        );
        assert!(
            all_text.contains("amber compass lantern")
                || all_text.contains("PDFXI Real-World Sample"),
            "the Excel text is missing from {all_text:?}"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn combining_one_source_is_a_typed_error_and_writes_nothing() {
        let dir = temp_dir("combine-one");
        let mut app = PdfxiApp::new();
        let output = dir.join("one.pdf");
        let error = app
            .combine_to_path(&[fixture("powerpoint-slides.pdf")], &output)
            .expect_err("one source must fail");
        assert!(error.contains("at least two"), "{error}");
        assert!(!output.exists());
        assert_eq!(app.combine.error.as_deref(), Some(error.as_str()));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn extracted_page_is_opened_and_the_source_stays_unchanged() {
        let dir = temp_dir("extract-open");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let source_pages = app.active_page_count().expect("source pages");
        app.organize.open = true;
        app.organize.selection.insert(0);
        let path = dir.join("first.pdf");
        app.organize_extract(&path);
        assert_eq!(app.active_page_count().expect("open extract"), 1);
        assert_eq!(
            app.state.tabs().count(),
            2,
            "the extracted file opens a second tab"
        );
        assert!(
            app.sessions
                .values()
                .any(|session| session.document().page_count().ok() == Some(source_pages))
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    fn active_bytes(app: &PdfxiApp) -> Vec<u8> {
        let id = app.state.active_tab_id().expect("active tab");
        app.sessions
            .get(&id)
            .expect("active session")
            .bytes()
            .to_vec()
    }

    fn open_temp_copy(tag: &str) -> (PathBuf, PdfxiApp) {
        let dir = temp_dir(tag);
        let path = dir.join("slides.pdf");
        std::fs::copy(fixture("powerpoint-slides.pdf"), &path).expect("copy the fixture");
        let mut app = PdfxiApp::new();
        app.open_document(&path);
        assert_eq!(
            app.active_page_count(),
            Some(3),
            "the temporary three-page copy opens"
        );
        (dir, app)
    }

    #[test]
    fn blank_page_template_is_a_valid_one_page_letter_pdf() {
        let document = blank_page_document().expect("the blank template parses");
        assert_eq!(document.page_count().expect("page count"), 1);
        assert!(
            page_text(&document, 0).trim().is_empty(),
            "the template must have no text"
        );
    }

    #[test]
    fn add_page_inserts_a_blank_page_after_the_current_page() {
        let dir = temp_dir("insert-after");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let before = app
            .sessions
            .values()
            .next()
            .expect("active session")
            .document()
            .clone();
        let old_first = page_text(&before, 0);
        let old_second = page_text(&before, 1);

        app.dispatch(crate::ui::AppCommand::AddPage, None);
        assert_eq!(app.status(), "Inserted a blank page after page 1");
        assert_eq!(
            app.state.active_tab().map(|tab| tab.current_page()),
            Some(1),
            "the new blank page becomes current"
        );
        assert_eq!(app.active_page_count(), Some(4));

        let path = dir.join("insert-after.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert_eq!(document.page_count().expect("page count"), 4);
        assert_eq!(page_text(&document, 0), old_first);
        assert!(
            page_text(&document, 1).trim().is_empty(),
            "page 1 (zero-based) is the inserted blank page"
        );
        assert_eq!(
            page_text(&document, 2),
            old_second,
            "the old second page moved to page 2"
        );
        assert!(!page_text(&document, 3).trim().is_empty());
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn insert_blank_page_before_page_zero_shifts_the_first_text() {
        let dir = temp_dir("insert-before");
        let mut app = open_fixture("powerpoint-slides.pdf");
        let old_first = page_text(
            &app.sessions
                .values()
                .next()
                .expect("active session")
                .document()
                .clone(),
            0,
        );

        app.run_page_menu_action(PageMenuAction::InsertBefore);
        assert_eq!(app.status(), "Inserted a blank page before page 1");
        assert_eq!(
            app.state.active_tab().map(|tab| tab.current_page()),
            Some(0),
            "the new blank page becomes current"
        );
        assert_eq!(app.active_page_count(), Some(4));

        let path = dir.join("insert-before.pdf");
        let document = save_and_reopen(&mut app, &path);
        assert_eq!(document.page_count().expect("page count"), 4);
        assert!(page_text(&document, 0).trim().is_empty());
        assert_eq!(
            page_text(&document, 1),
            old_first,
            "the old first page moved to page 1 (zero-based)"
        );
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn page_menu_dispatch_opens_the_menu_and_every_action_changes_the_document() {
        let (dir, mut app) = open_temp_copy("page-menu-actions");
        assert!(!app.page_menu_open(), "the menu starts closed");
        app.dispatch(crate::ui::AppCommand::PageMenu, None);
        assert!(app.page_menu_open(), "PageMenu opens the page menu");
        assert_eq!(app.status(), "Page menu: choose a page action");

        // Insert before the active page: one more page and different bytes.
        let before = active_bytes(&app);
        let count = app.active_page_count().expect("page count");
        app.run_page_menu_action(PageMenuAction::InsertBefore);
        assert_eq!(app.active_page_count(), Some(count + 1));
        assert_ne!(
            active_bytes(&app),
            before,
            "InsertBefore rewrites the document"
        );

        // Insert after the current (blank) page.
        let before = active_bytes(&app);
        let count = app.active_page_count().expect("page count");
        let current = app.state.active_tab().expect("active tab").current_page();
        app.run_page_menu_action(PageMenuAction::InsertAfter);
        assert_eq!(app.active_page_count(), Some(count + 1));
        assert_ne!(
            active_bytes(&app),
            before,
            "InsertAfter rewrites the document"
        );
        assert_eq!(
            app.state.active_tab().map(|tab| tab.current_page()),
            Some(current + 1)
        );

        // Rotate right and left change the page bytes.
        let before = active_bytes(&app);
        app.run_page_menu_action(PageMenuAction::RotateRight);
        assert_ne!(
            active_bytes(&app),
            before,
            "RotateRight rewrites the document"
        );
        let before = active_bytes(&app);
        app.run_page_menu_action(PageMenuAction::RotateLeft);
        assert_ne!(
            active_bytes(&app),
            before,
            "RotateLeft rewrites the document"
        );

        // Delete removes one page.
        let count = app.active_page_count().expect("page count");
        app.run_page_menu_action(PageMenuAction::Delete);
        assert_eq!(app.active_page_count(), Some(count - 1));

        // Extract writes a one-page PDF and opens it in a second tab.
        let tabs = app.state.tabs().count();
        let page = app.state.active_tab().expect("active tab").current_page();
        let expected = dir.join(format!("slides-page-{}.pdf", page + 1));
        app.run_page_menu_action(PageMenuAction::Extract);
        assert!(expected.exists(), "Extract writes {}", expected.display());
        assert_eq!(app.state.tabs().count(), tabs + 1);
        assert_eq!(app.active_page_count(), Some(1));
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn add_page_and_page_menu_report_a_missing_document() {
        let mut app = PdfxiApp::new();
        app.dispatch(crate::ui::AppCommand::AddPage, None);
        assert_eq!(app.status(), "Open a document to add a page");
        app.dispatch(crate::ui::AppCommand::PageMenu, None);
        assert_eq!(app.status(), "Open a document to manage pages");
        assert!(!app.page_menu_open());
    }
}
