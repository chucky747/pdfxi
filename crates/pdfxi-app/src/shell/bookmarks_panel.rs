//! Bookmarks panel and document-properties dialog (T-0255).
//!
//! The panel lists the active document's `/Outlines` tree with one indentation
//! level per nesting depth. A row with a resolved destination jumps to that
//! page through the same command the thumbnail list uses; a row without one is
//! weak-styled, still selectable (so Rename/Delete can reach it) and explains
//! why it cannot navigate.
//!
//! Every mutation clones the open `Document`, edits the cloned [`Outline`],
//! writes it with [`write_outline`], and commits the clone with
//! `session.edit` followed by `after_session_edit`, exactly like
//! `comments.rs::commit_annotation`. A rejected edit leaves the previous
//! outline in place.
//!
//! The properties dialog reads only real values: `/Info`, the catalog
//! `/Metadata` stream, the document version and the session facts. Absent
//! values read "not set"; a read failure reads "unavailable".

use std::path::Path;

use pdfxi_core::{Document, DocumentInfo};
use pdfxi_edit::geometry::PageIndex;
use pdfxi_edit::navigation::{
    Bookmark, BookmarkPath, Destination, FitType, Outline, OutlineIssue, OutlineIssueReason,
    read_outline, write_outline,
};

use crate::TabId;
use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};

use super::capabilities::Capability;

/// State of the Bookmarks section in the Pages panel.
#[derive(Default)]
pub(crate) struct BookmarksPanelState {
    /// Whether the section is expanded.
    pub(crate) open: bool,
    /// Parsed outline for the active document.
    pub(crate) outline: Option<Outline>,
    /// Outline items whose destination could not be resolved.
    pub(crate) issues: Vec<OutlineIssue>,
    /// Selected child-index path.
    pub(crate) selected: Option<Vec<usize>>,
    /// Title being edited.
    pub(crate) draft_title: String,
    /// Path currently being renamed inline.
    pub(crate) renaming: Option<Vec<usize>>,
    /// Typed error from the last outline operation.
    pub(crate) error: Option<String>,
}

/// State of the Document properties window.
#[derive(Default)]
pub(crate) struct DocumentPropertiesState {
    /// Whether the window is visible.
    pub(crate) open: bool,
    /// Rows captured when the window opened.
    pub(crate) view: Option<PropertiesView>,
}

/// Label/value rows shown by the properties dialog.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct PropertiesView {
    /// Stable label and its real value (or "not set"/"unavailable").
    pub(crate) rows: Vec<(&'static str, String)>,
}

/// One flattened bookmark row.
#[derive(Clone, Debug, PartialEq, Eq)]
struct BookmarkRow {
    /// Child-index path from the root list.
    path: Vec<usize>,
    /// Nesting depth, used for indentation.
    depth: usize,
    /// Position among siblings.
    index: usize,
    /// Number of siblings, for the Move up/down gates.
    siblings: usize,
    /// Bookmark title.
    title: String,
    /// Zero-based destination page, when resolved.
    page: Option<usize>,
    /// Why a destination-less row cannot navigate.
    reason: Option<String>,
}

impl PdfxiApp {
    /// Reload the outline for the active tab.
    ///
    /// A document with no outline yields an empty tree with no error. A
    /// malformed outline yields an empty tree, no issues and a typed error.
    pub(crate) fn refresh_bookmarks(&mut self) {
        self.bookmarks.selected = None;
        self.bookmarks.renaming = None;
        self.bookmarks.draft_title.clear();
        let Some(id) = self.state.active_tab_id() else {
            self.bookmarks.outline = None;
            self.bookmarks.issues.clear();
            self.bookmarks.error = None;
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            self.bookmarks.outline = None;
            self.bookmarks.issues.clear();
            self.bookmarks.error = None;
            return;
        };
        match read_outline(session.document()) {
            Ok(read) => {
                self.bookmarks.outline = Some(read.outline);
                self.bookmarks.issues = read.issues;
                self.bookmarks.error = None;
                self.bookmarks.open = true;
            }
            Err(error) => {
                self.bookmarks.outline = Some(Outline::new());
                self.bookmarks.issues.clear();
                self.bookmarks.error = Some(error.to_string());
            }
        }
    }

    /// Replace the selection with `path`.
    pub(crate) fn select_bookmark(&mut self, path: Vec<usize>) {
        self.bookmarks.selected = Some(path);
    }

    /// Insert a root bookmark at the active tab's current page.
    pub(crate) fn add_bookmark_at_current_page(&mut self) -> Result<(), String> {
        let result = self.add_bookmark_impl();
        self.report_bookmark(result)
    }

    /// Rename the selected bookmark.
    pub(crate) fn rename_selected_bookmark(&mut self, title: &str) -> Result<(), String> {
        let result = self.rename_bookmark_impl(title);
        self.report_bookmark(result)
    }

    /// Delete the selected bookmark.
    pub(crate) fn delete_selected_bookmark(&mut self) -> Result<(), String> {
        let result = self.delete_bookmark_impl();
        self.report_bookmark(result)
    }

    /// Move the selected bookmark one sibling up (`up`) or down.
    pub(crate) fn move_selected_bookmark(&mut self, up: bool) -> Result<(), String> {
        let result = self.move_bookmark_impl(up);
        self.report_bookmark(result)
    }

    /// The rows the panel draws for the current outline.
    fn bookmark_rows(&self) -> Vec<BookmarkRow> {
        match &self.bookmarks.outline {
            Some(outline) => flatten_outline(outline, &self.bookmarks.issues),
            None => Vec::new(),
        }
    }

    /// Draw the collapsible Bookmarks section under the page thumbnails.
    pub(crate) fn bookmarks_section_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        if !self.capabilities.contains(Capability::Bookmarks) {
            return;
        }
        let Some(id) = self.state.active_tab_id() else {
            return;
        };
        let writable = !self.read_only_tabs.contains(&id);
        let rows = self.bookmark_rows();
        let selected = self.bookmarks.selected.clone();
        let selected_row = selected
            .as_ref()
            .and_then(|path| rows.iter().find(|row| &row.path == path))
            .cloned();
        let mut navigate: Option<(usize, Vec<usize>)> = None;
        let mut add = false;
        let mut begin_rename = false;
        let mut commit_rename = false;
        let mut delete = false;
        let mut move_up = false;
        let mut move_down = false;

        ui.separator();
        egui::CollapsingHeader::new("Bookmarks")
            .default_open(self.bookmarks.open)
            .show(ui, |ui| {
                if let Some(error) = &self.bookmarks.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .add_enabled(writable, egui::Button::new("Add bookmark"))
                        .on_hover_text("Add a bookmark at the current page")
                        .clicked()
                    {
                        add = true;
                    }
                    if ui
                        .add_enabled(writable && selected.is_some(), egui::Button::new("Rename"))
                        .clicked()
                    {
                        begin_rename = true;
                    }
                    if ui
                        .add_enabled(writable && selected.is_some(), egui::Button::new("Delete"))
                        .clicked()
                    {
                        delete = true;
                    }
                    let can_move_up =
                        writable && selected_row.as_ref().is_some_and(|row| row.index > 0);
                    if ui
                        .add_enabled(can_move_up, egui::Button::new("Move up"))
                        .clicked()
                    {
                        move_up = true;
                    }
                    let can_move_down = writable
                        && selected_row
                            .as_ref()
                            .is_some_and(|row| row.index + 1 < row.siblings);
                    if ui
                        .add_enabled(can_move_down, egui::Button::new("Move down"))
                        .clicked()
                    {
                        move_down = true;
                    }
                });

                if self.bookmarks.renaming.is_some() {
                    ui.horizontal(|ui| {
                        let response = ui.add(
                            egui::TextEdit::singleline(&mut self.bookmarks.draft_title)
                                .desired_width(180.0),
                        );
                        if ui.button("Save").clicked()
                            || (response.lost_focus()
                                && ui.input(|input| input.key_pressed(egui::Key::Enter)))
                        {
                            commit_rename = true;
                        }
                        if ui.button("Cancel").clicked() {
                            self.bookmarks.renaming = None;
                        }
                    });
                }

                if rows.is_empty() {
                    ui.label(egui::RichText::new("No bookmarks").color(palette.text_muted));
                }
                for row in &rows {
                    let is_selected = selected.as_deref() == Some(row.path.as_slice());
                    let indent = row.depth as f32 * 12.0;
                    ui.horizontal(|ui| {
                        ui.add_space(indent);
                        if let Some(page) = row.page {
                            let response = ui
                                .selectable_label(is_selected, row.title.clone())
                                .on_hover_text(format!("Page {}", page + 1));
                            if response.clicked() {
                                navigate = Some((page, row.path.clone()));
                            }
                        } else {
                            let reason = row
                                .reason
                                .clone()
                                .unwrap_or_else(|| "no destination".to_owned());
                            let response = ui
                                .selectable_label(
                                    is_selected,
                                    egui::RichText::new(row.title.clone()).weak(),
                                )
                                .on_hover_text(reason);
                            if response.clicked() {
                                // Select only: a destination-less item cannot navigate.
                                self.select_bookmark(row.path.clone());
                            }
                        }
                    });
                }
            });

        if let Some((page, path)) = navigate {
            // Same command the thumbnail list dispatches; the destination page
            // is already zero-based and passes through unchanged.
            self.dispatch(AppCommand::SelectThumbnailPage(page), Some(ui.ctx()));
            self.bookmarks.selected = Some(path);
        }
        if add {
            let _ = self.add_bookmark_at_current_page();
        }
        if begin_rename && let Some(row) = &selected_row {
            self.bookmarks.renaming = Some(row.path.clone());
            self.bookmarks.draft_title = row.title.clone();
        }
        if commit_rename {
            let title = self.bookmarks.draft_title.clone();
            let _ = self.rename_selected_bookmark(&title);
        }
        if delete {
            let _ = self.delete_selected_bookmark();
        }
        if move_up {
            let _ = self.move_selected_bookmark(true);
        }
        if move_down {
            let _ = self.move_selected_bookmark(false);
        }
    }

    /// Open the Document properties window for the active tab.
    pub(crate) fn open_document_properties(&mut self) {
        if !self.capabilities.contains(Capability::Documents) {
            self.status = "Document properties are not enabled in this build".to_owned();
            return;
        }
        let Some(id) = self.state.active_tab_id() else {
            self.status = "Open a document to see its properties".to_owned();
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            self.status = "The document has no open session".to_owned();
            return;
        };
        self.properties_state.view = Some(PropertiesView {
            rows: properties_rows(
                session.document(),
                session.path(),
                session.bytes().len(),
                session.decryption_report().is_some(),
            ),
        });
        self.properties_state.open = true;
        self.status = "Document properties".to_owned();
    }

    /// Close the Document properties window.
    pub(crate) fn close_document_properties(&mut self) {
        self.properties_state.open = false;
    }

    /// Whether the Document properties window is visible.
    #[must_use]
    pub fn document_properties_open(&self) -> bool {
        self.properties_state.open
    }

    /// Draw the Document properties window.
    pub(crate) fn document_properties_ui(&mut self, ctx: &egui::Context) {
        if !self.properties_state.open {
            return;
        }
        let mut open = true;
        egui::Window::new("Document properties")
            .open(&mut open)
            .resizable(true)
            .default_width(380.0)
            .show(ctx, |ui| {
                let Some(view) = &self.properties_state.view else {
                    ui.label("Open a document to see its properties");
                    return;
                };
                egui::Grid::new("document_properties_grid")
                    .num_columns(2)
                    .spacing([14.0, 4.0])
                    .show(ui, |ui| {
                        for (label, value) in &view.rows {
                            ui.label(egui::RichText::new(*label).strong());
                            ui.label(value);
                            ui.end_row();
                        }
                    });
            });
        if !open {
            self.close_document_properties();
        }
    }

    /// The app command entry point for [`AppCommand::DocumentProperties`].
    pub(crate) fn handle_document_properties_command(&mut self) {
        self.open_document_properties();
    }

    /// Active writable tab with an open session.
    fn bookmark_target(&self) -> Result<TabId, String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document first".to_owned());
        };
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        if !self.sessions.contains_key(&id) {
            return Err("The document has no open session".to_owned());
        }
        Ok(id)
    }

    /// Store a success or failure on the panel and the status line.
    fn report_bookmark(&mut self, result: Result<(), String>) -> Result<(), String> {
        match result {
            Ok(()) => {
                self.bookmarks.error = None;
                Ok(())
            }
            Err(error) => {
                self.bookmarks.error = Some(error.clone());
                self.status = format!("Bookmarks: {error}");
                Err(error)
            }
        }
    }

    fn add_bookmark_impl(&mut self) -> Result<(), String> {
        let id = self.bookmark_target()?;
        let outline = self
            .bookmarks
            .outline
            .clone()
            .ok_or_else(|| "The document outline is not loaded".to_owned())?;
        let page = self.state.tab(id).map_or(0, |tab| tab.current_page());
        let index = outline.roots.len();
        let title = format!("Bookmark {}", index + 1);
        let bookmark = Bookmark::new(title.clone())
            .map_err(|error| error.to_string())?
            .with_destination(Destination::new(PageIndex::new(page), FitType::Fit));
        let mut next = outline;
        next.insert_bookmark(&BookmarkPath::root(), index, bookmark)
            .map_err(|error| error.to_string())?;
        self.commit_outline(id, next)?;
        self.bookmarks.selected = Some(vec![index]);
        self.bookmarks.draft_title = title;
        self.bookmarks.renaming = None;
        self.status = format!("Bookmark added on page {}", page + 1);
        Ok(())
    }

    fn rename_bookmark_impl(&mut self, title: &str) -> Result<(), String> {
        let id = self.bookmark_target()?;
        let path = self
            .bookmarks
            .selected
            .clone()
            .ok_or_else(|| "Select a bookmark first".to_owned())?;
        let title = title.trim();
        if title.is_empty() {
            return Err("The bookmark title cannot be empty".to_owned());
        }
        let mut next = self
            .bookmarks
            .outline
            .clone()
            .ok_or_else(|| "The document outline is not loaded".to_owned())?;
        next.rename_bookmark(&path_of(&path), title)
            .map_err(|error| error.to_string())?;
        self.commit_outline(id, next)?;
        self.bookmarks.selected = Some(path);
        self.bookmarks.renaming = None;
        self.status = format!("Bookmark renamed to \"{title}\"");
        Ok(())
    }

    fn delete_bookmark_impl(&mut self) -> Result<(), String> {
        let id = self.bookmark_target()?;
        let path = self
            .bookmarks
            .selected
            .clone()
            .ok_or_else(|| "Select a bookmark first".to_owned())?;
        let mut next = self
            .bookmarks
            .outline
            .clone()
            .ok_or_else(|| "The document outline is not loaded".to_owned())?;
        let removed = next
            .remove_bookmark(&path_of(&path))
            .map_err(|error| error.to_string())?;
        self.commit_outline(id, next)?;
        self.bookmarks.selected = None;
        self.bookmarks.renaming = None;
        self.status = format!("Bookmark \"{}\" deleted", removed.title);
        Ok(())
    }

    fn move_bookmark_impl(&mut self, up: bool) -> Result<(), String> {
        let id = self.bookmark_target()?;
        let path = self
            .bookmarks
            .selected
            .clone()
            .ok_or_else(|| "Select a bookmark first".to_owned())?;
        let (parent, index) = path
            .split_last()
            .map(|(last, rest)| (rest.to_vec(), *last))
            .ok_or_else(|| "The outline root has no bookmark to move".to_owned())?;
        let mut next = self
            .bookmarks
            .outline
            .clone()
            .ok_or_else(|| "The document outline is not loaded".to_owned())?;
        let siblings = sibling_count(&next, &parent);
        if up && index == 0 {
            return Err("The bookmark is already first".to_owned());
        }
        if !up && index + 1 >= siblings {
            return Err("The bookmark is already last".to_owned());
        }
        let to_index = if up { index - 1 } else { index + 2 };
        next.move_bookmark(&path_of(&path), &path_of(&parent), to_index)
            .map_err(|error| error.to_string())?;
        let new_index = if up { index - 1 } else { index + 1 };
        let mut new_path = parent;
        new_path.push(new_index);
        self.commit_outline(id, next)?;
        self.bookmarks.selected = Some(new_path.clone());
        self.bookmarks.renaming = None;
        self.status = if up {
            "Bookmark moved up".to_owned()
        } else {
            "Bookmark moved down".to_owned()
        };
        Ok(())
    }

    /// Write `outline` into a cloned document and commit it as one revision.
    fn commit_outline(&mut self, id: TabId, outline: Outline) -> Result<(), String> {
        let mut candidate = self
            .sessions
            .get(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .document()
            .clone();
        write_outline(&mut candidate, &outline).map_err(|error| error.to_string())?;
        let result = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit(|document| {
                *document = candidate;
                Ok(())
            });
        match result {
            Ok(()) => {
                // Reload from the committed bytes and refresh annotations/forms.
                self.after_session_edit(id);
                Ok(())
            }
            Err(error) => Err(error.to_string()),
        }
    }
}

/// Flatten an outline into display rows, resolving issue reasons by title.
fn flatten_outline(outline: &Outline, issues: &[OutlineIssue]) -> Vec<BookmarkRow> {
    fn walk(
        nodes: &[Bookmark],
        depth: usize,
        prefix: &mut Vec<usize>,
        issues: &[OutlineIssue],
        rows: &mut Vec<BookmarkRow>,
    ) {
        let siblings = nodes.len();
        for (index, node) in nodes.iter().enumerate() {
            prefix.push(index);
            rows.push(BookmarkRow {
                path: prefix.clone(),
                depth,
                index,
                siblings,
                title: node.title.clone(),
                page: node
                    .destination
                    .as_ref()
                    .map(|destination| destination.page.get()),
                reason: node
                    .destination
                    .as_ref()
                    .map_or_else(|| Some(issue_reason(issues, &node.title)), |_| None),
            });
            walk(&node.children, depth + 1, prefix, issues, rows);
            prefix.pop();
        }
    }
    let mut rows = Vec::new();
    walk(&outline.roots, 0, &mut Vec::new(), issues, &mut rows);
    rows
}

/// Explain why an item has no destination.
fn issue_reason(issues: &[OutlineIssue], title: &str) -> String {
    let Some(issue) = issues.iter().find(|issue| issue.title == title) else {
        return "no destination".to_owned();
    };
    match &issue.reason {
        OutlineIssueReason::MissingDestination => "no destination".to_owned(),
        OutlineIssueReason::NonGoToAction(action) => {
            format!("not a Go To action ({action})")
        }
        OutlineIssueReason::UnresolvedNamedDestination(name) => {
            format!("unresolved named destination: {name}")
        }
        OutlineIssueReason::InvalidDestination(message) => {
            format!("invalid destination: {message}")
        }
        OutlineIssueReason::DanglingPageReference(id) => {
            format!("page reference does not exist ({id:?})")
        }
    }
}

/// Build a `BookmarkPath` from child indices.
fn path_of(indices: &[usize]) -> BookmarkPath {
    indices
        .iter()
        .fold(BookmarkPath::root(), |path, index| path.child(*index))
}

/// Number of children in the list addressed by `parent`.
fn sibling_count(outline: &Outline, parent: &[usize]) -> usize {
    let mut list = &outline.roots;
    for index in parent {
        let Some(node) = list.get(*index) else {
            return 0;
        };
        list = &node.children;
    }
    list.len()
}

/// Build the property rows from real document and session values only.
fn properties_rows(
    document: &Document,
    path: Option<&Path>,
    file_size: usize,
    encrypted: bool,
) -> Vec<(&'static str, String)> {
    let file_name = path.and_then(Path::file_name).map_or_else(
        || "untitled".to_owned(),
        |name| name.to_string_lossy().into_owned(),
    );
    let pages = document
        .page_count()
        .map_or_else(|_| "unavailable".to_owned(), |count| count.to_string());
    let (info, info_failed) = match document.info() {
        Ok(info) => (info.unwrap_or_default(), false),
        Err(_) => (DocumentInfo::default(), true),
    };
    let text = |value: &Option<String>| {
        if info_failed {
            "unavailable".to_owned()
        } else {
            value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
                .map_or_else(|| "not set".to_owned(), str::to_owned)
        }
    };
    let date = |value: &Option<pdfxi_core::PdfDate>| {
        if info_failed {
            "unavailable".to_owned()
        } else {
            value
                .as_ref()
                .map_or_else(|| "not set".to_owned(), |date| date.raw.clone())
        }
    };
    let xmp = match document.xmp_metadata() {
        Ok(Some(_)) => "Yes".to_owned(),
        Ok(None) => "No".to_owned(),
        Err(_) => "unavailable".to_owned(),
    };
    vec![
        ("File name", file_name),
        ("Pages", pages),
        ("File size", format!("{file_size} bytes")),
        ("PDF version", document.version().to_owned()),
        ("Title", text(&info.title)),
        ("Author", text(&info.author)),
        ("Subject", text(&info.subject)),
        ("Keywords", text(&info.keywords)),
        ("Creator", text(&info.creator)),
        ("Producer", text(&info.producer)),
        ("Creation date", date(&info.creation_date)),
        ("Modification date", date(&info.modification_date)),
        ("Encrypted", if encrypted { "Yes" } else { "No" }.to_owned()),
        ("XMP metadata", xmp),
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld")
            .join(name)
    }

    fn temp_path(tag: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU32, Ordering};
        static COUNTER: AtomicU32 = AtomicU32::new(0);
        let unique = COUNTER.fetch_add(1, Ordering::Relaxed);
        let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/tmp/bookmarks");
        std::fs::create_dir_all(&dir).expect("create the temporary directory");
        dir.join(format!("{tag}-{}-{unique}.pdf", std::process::id()))
    }

    fn open_fixture(name: &str) -> PdfxiApp {
        let mut app = PdfxiApp::new();
        app.open_document(&fixture(name));
        app
    }

    fn read_saved(path: &Path) -> Outline {
        let bytes = std::fs::read(path).expect("read the saved file");
        let document = Document::from_bytes(bytes).expect("reopen the saved file");
        read_outline(&document)
            .expect("read the reopened outline")
            .outline
    }

    #[test]
    fn powerpoint_outline_loads_add_rename_move_persist_then_delete() {
        let mut app = open_fixture("powerpoint-slides.pdf");
        let before = app
            .bookmarks
            .outline
            .clone()
            .expect("the fixture outline loads");
        assert!(
            !before.roots.is_empty(),
            "the PowerPoint fixture carries an outline"
        );
        let count = before.roots.len();
        let last_title = before.roots[count - 1].title.clone();
        let page = app.state.active_tab().map_or(0, |tab| tab.current_page());

        app.add_bookmark_at_current_page()
            .expect("add the bookmark");
        assert_eq!(app.bookmarks.selected, Some(vec![count]));
        let added = app
            .bookmarks
            .outline
            .as_ref()
            .expect("outline")
            .roots
            .get(count)
            .expect("the bookmark is appended");
        assert_eq!(
            added.destination.as_ref().map(|dest| dest.page.get()),
            Some(page)
        );
        assert_eq!(
            added.destination.as_ref().map(|dest| &dest.fit),
            Some(&FitType::Fit)
        );

        app.rename_selected_bookmark("Renamed bookmark")
            .expect("rename the bookmark");
        app.move_selected_bookmark(true)
            .expect("move the bookmark up");
        assert_eq!(app.bookmarks.selected, Some(vec![count - 1]));

        let saved = temp_path("powerpoint");
        app.sessions
            .get_mut(&app.state.active_tab_id().expect("active tab"))
            .expect("session")
            .save_as(&saved)
            .expect("save");
        let artifact = PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../target/tmp/bookmarks/evidence-powerpoint-outline.pdf");
        if let Some(parent) = artifact.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        std::fs::copy(&saved, &artifact).expect("copy the evidence artifact");
        let reopened = read_saved(&saved);
        assert_eq!(reopened.roots.len(), count + 1);
        assert_eq!(reopened.roots[count - 1].title, "Renamed bookmark");
        assert_eq!(
            reopened.roots[count - 1]
                .destination
                .as_ref()
                .map(|d| d.page.get()),
            Some(page)
        );
        assert_eq!(
            reopened.roots[count].title, last_title,
            "the bookmark that followed the added one moves down by one"
        );

        app.select_bookmark(vec![count - 1]);
        app.delete_selected_bookmark().expect("delete the bookmark");
        let deleted = temp_path("powerpoint-deleted");
        app.sessions
            .get_mut(&app.state.active_tab_id().expect("active tab"))
            .expect("session")
            .save_as(&deleted)
            .expect("save after delete");
        let reopened = read_saved(&deleted);
        assert!(
            !reopened
                .roots
                .iter()
                .any(|bookmark| bookmark.title == "Renamed bookmark"),
            "the deleted bookmark is gone after save and reopen"
        );
        let _ = std::fs::remove_file(&saved);
        let _ = std::fs::remove_file(&deleted);
    }

    #[test]
    fn excel_document_gains_an_outline_that_persists() {
        let mut app = open_fixture("microsoft-excel-2019-sample.pdf");
        assert!(
            app.bookmarks
                .outline
                .as_ref()
                .is_some_and(|outline| outline.roots.is_empty()),
            "the Excel fixture has no outline"
        );
        app.add_bookmark_at_current_page()
            .expect("add a bookmark to a document without an outline");
        assert_eq!(
            app.bookmarks.outline.as_ref().expect("outline").roots.len(),
            1
        );
        let saved = temp_path("excel");
        app.sessions
            .get_mut(&app.state.active_tab_id().expect("active tab"))
            .expect("session")
            .save_as(&saved)
            .expect("save");
        let reopened = read_saved(&saved);
        assert_eq!(reopened.roots.len(), 1);
        assert_eq!(reopened.roots[0].title, "Bookmark 1");
        let _ = std::fs::remove_file(&saved);
    }

    #[test]
    fn a_rejected_edit_keeps_the_previous_outline_and_reports_it() {
        let mut app = open_fixture("powerpoint-slides.pdf");
        let before = app.bookmarks.outline.clone().expect("outline");
        app.bookmarks.selected = None;
        let error = app
            .rename_selected_bookmark("No selection")
            .expect_err("renaming without a selection fails");
        assert!(error.contains("Select a bookmark"));
        assert_eq!(app.bookmarks.outline.as_ref(), Some(&before));
        assert!(app.status().starts_with("Bookmarks:"));
        assert!(app.bookmarks.error.is_some());
    }

    #[test]
    fn properties_show_real_info_and_not_set_for_absent_values() {
        let source = std::fs::read(fixture("powerpoint-slides.pdf")).expect("fixture bytes");
        let mut document = Document::from_bytes(source).expect("fixture parses");
        document
            .set_info(&DocumentInfo {
                title: Some("Quarterly Report".to_owned()),
                creator: Some("PDFXI Test".to_owned()),
                ..DocumentInfo::default()
            })
            .expect("write info");
        let expected_pages = document.page_count().expect("page count").to_string();
        let path = temp_path("properties");
        std::fs::write(&path, document.save().expect("save")).expect("write the temp PDF");

        let mut app = PdfxiApp::new();
        app.open_document(&path);
        app.open_document_properties();
        assert!(app.document_properties_open());
        let view = app.properties_state.view.as_ref().expect("properties view");
        let value = |label: &str| {
            view.rows
                .iter()
                .find(|(row_label, _)| *row_label == label)
                .map(|(_, value)| value.as_str())
                .expect("the row exists")
        };
        assert_eq!(value("Title"), "Quarterly Report");
        assert_eq!(value("Creator"), "PDFXI Test");
        assert_eq!(value("Author"), "not set");
        assert_eq!(value("Pages"), expected_pages);
        assert!(!value("PDF version").is_empty());
        assert_eq!(value("Encrypted"), "No");
        let _ = std::fs::remove_file(&path);
    }
}
