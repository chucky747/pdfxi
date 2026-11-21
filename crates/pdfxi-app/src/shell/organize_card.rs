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
//! Inserting a blank page is not offered: [`PageOperation`] has no insert or
//! blank-page variant, and inventing page content in the shell would bypass the
//! library's page-tree validation. The card says so instead of shipping a dead
//! control.
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
        let selection_count = self.organize.selection.len();
        let action_enabled = selection_count > 0;
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());

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
                ui.label(
                    egui::RichText::new(
                        "Insert blank page: not available in this build \
                         (the page planner has no blank-page operation).",
                    )
                    .size(10.0)
                    .weak(),
                );
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
}
