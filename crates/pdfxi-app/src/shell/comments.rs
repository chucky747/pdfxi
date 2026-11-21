//! Comments and markup: rail badge, side panel, page popup, note and highlight.
//!
//! All PDF mutation goes through `pdfxi_edit::document_annotations` on a clone
//! of the active session's document, which is then committed as one
//! `DocumentSession::edit`. That makes every action undoable and saved with the
//! document, and it keeps the shell free of annotation semantics.

use pdfxi_edit::{
    AnnotationState, AnnotationSubtype, AnnotationView, Point, Rect, add_highlight, add_text_note,
    delete_annotation, list_annotations, reply_to, set_state, update_contents,
};
use pdfxi_security::Permission;

use time::{Date, Month, OffsetDateTime, PrimitiveDateTime, Time, UtcOffset};

use crate::canvas::{CanvasLayout, CanvasRect};
use crate::design::{Icon, Palette, icon};
use crate::ui::{AppCommand, PdfxiApp};

/// One annotation in the open document, with its zero-based page.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnotationEntry {
    /// Zero-based page that owns the annotation.
    pub page: usize,
    /// The annotation as the library reads it.
    pub view: AnnotationView,
}

/// A pending note placement typed by the user.
#[derive(Clone, Debug, Default, PartialEq)]
pub(crate) struct NoteDraft {
    /// Page the note will land on.
    pub page: usize,
    /// PDF user-space point (bottom-left origin).
    pub x: f32,
    /// PDF user-space point (bottom-left origin).
    pub y: f32,
    /// Comment text being typed.
    pub text: String,
}

/// Comments state owned by the shell.
#[derive(Default)]
pub(crate) struct CommentsUiState {
    pub(crate) entries: Vec<AnnotationEntry>,
    pub(crate) selected: Option<(usize, String)>,
    pub(crate) popup_open: bool,
    pub(crate) reply_text: String,
    pub(crate) editing: bool,
    pub(crate) edit_text: String,
    pub(crate) note_draft: Option<NoteDraft>,
    pub(crate) note_tool: bool,
    pub(crate) hint: Option<String>,
    pub(crate) error: Option<String>,
    next_id: u64,
}

impl CommentsUiState {
    /// Clear everything tied to the previous document.
    fn reset_document(&mut self) {
        self.entries.clear();
        self.selected = None;
        self.popup_open = false;
        self.reply_text.clear();
        self.editing = false;
        self.edit_text.clear();
        self.note_draft = None;
        self.note_tool = false;
    }
}

fn is_resolved(view: &AnnotationView) -> bool {
    matches!(
        view.state.as_deref(),
        Some("Accepted" | "Completed" | "Marked")
    )
}

fn subtype_label(subtype: &AnnotationSubtype) -> &str {
    match subtype {
        AnnotationSubtype::Text => "Note",
        AnnotationSubtype::Highlight => "Highlight",
        AnnotationSubtype::Other(name) => name.as_str(),
    }
}

fn subtype_icon(subtype: &AnnotationSubtype) -> Icon {
    match subtype {
        AnnotationSubtype::Highlight => Icon::Highlight,
        _ => Icon::Comment,
    }
}

fn snippet(text: &str) -> String {
    let flat = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if flat.chars().count() <= 42 {
        return flat;
    }
    let mut trimmed: String = flat.chars().take(41).collect();
    trimmed.push('…');
    trimmed
}

/// A parsed `/M` value: zoned when the PDF carries an offset, wall time when it
/// does not.
enum PdfDate {
    Zoned {
        date: Date,
        time: Time,
        offset_seconds: i32,
    },
    Wall {
        date: Date,
        time: Time,
    },
}

/// Format a PDF date string for people.
///
/// `D:YYYYMMDDHHmmSS` followed by `Z`, `+HH'mm'`, `-HH'mm'` or nothing is
/// accepted; zoned values are converted to local time (falling back to the
/// recorded offset when the platform cannot report one). A string that cannot
/// be parsed is returned unchanged, so an unusual producer never hides its
/// date.
fn display_date(raw: &str) -> String {
    const FORMAT: &[time::format_description::FormatItem<'static>] =
        time::macros::format_description!("[year]-[month]-[day] [hour]:[minute]");
    match parse_pdf_date(raw) {
        Some(PdfDate::Zoned {
            date,
            time,
            offset_seconds,
        }) => {
            let offset = UtcOffset::from_whole_seconds(offset_seconds).unwrap_or(UtcOffset::UTC);
            let local = UtcOffset::current_local_offset().unwrap_or(offset);
            OffsetDateTime::new_in_offset(date, time, offset)
                .to_offset(local)
                .format(FORMAT)
                .unwrap_or_else(|_| raw.to_owned())
        }
        Some(PdfDate::Wall { date, time }) => PrimitiveDateTime::new(date, time)
            .format(FORMAT)
            .unwrap_or_else(|_| raw.to_owned()),
        None => raw.to_owned(),
    }
}

fn parse_pdf_date(raw: &str) -> Option<PdfDate> {
    let trimmed = raw.trim();
    let body = trimmed.strip_prefix("D:").unwrap_or(trimmed);
    let year: i32 = body.get(0..4)?.parse().ok()?;
    let month: u8 = body.get(4..6)?.parse().ok()?;
    let day: u8 = body.get(6..8)?.parse().ok()?;
    let hour: u8 = body
        .get(8..10)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let minute: u8 = body
        .get(10..12)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let second: u8 = body
        .get(12..14)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    let date = Date::from_calendar_date(year, Month::try_from(month).ok()?, day).ok()?;
    let time = Time::from_hms(hour, minute, second).ok()?;
    match parse_pdf_offset(body.get(14..).unwrap_or("")) {
        Some(offset_seconds) => Some(PdfDate::Zoned {
            date,
            time,
            offset_seconds,
        }),
        None => Some(PdfDate::Wall { date, time }),
    }
}

fn parse_pdf_offset(rest: &str) -> Option<i32> {
    let rest = rest.trim();
    if rest.is_empty() {
        return None;
    }
    if rest.starts_with(['Z', 'z']) {
        return Some(0);
    }
    let sign = match rest.as_bytes().first() {
        Some(b'+') => 1,
        Some(b'-') => -1,
        _ => return None,
    };
    let digits: String = rest
        .get(1..)
        .unwrap_or("")
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    let hours: i32 = digits.get(0..2)?.parse().ok()?;
    let minutes: i32 = digits
        .get(2..4)
        .and_then(|text| text.parse().ok())
        .unwrap_or(0);
    if hours > 23 || minutes > 59 {
        return None;
    }
    Some(sign * (hours * 3600 + minutes * 60))
}

impl PdfxiApp {
    /// The number of times the canvas renderer has been reloaded.
    ///
    /// A change means the canvas source was rebuilt from the session's current
    /// bytes, so every raster and texture from the previous generation is
    /// stale. Tests use it to prove an annotation edit reaches the canvas
    /// without a save or reopen.
    #[must_use]
    pub fn render_generation(&self) -> u64 {
        self.render_generation
    }

    /// Number of annotations in the open document (the rail badge).
    #[must_use]
    pub fn annotation_count(&self) -> usize {
        self.comments.entries.len()
    }

    /// Every annotation in the open document, in page then document order.
    #[must_use]
    pub fn annotations(&self) -> &[AnnotationEntry] {
        &self.comments.entries
    }

    /// The annotation whose popup is open, if any.
    #[must_use]
    pub fn selected_annotation(&self) -> Option<(usize, &AnnotationView)> {
        let (page, id) = self.comments.selected.as_ref()?;
        self.comments
            .entries
            .iter()
            .find(|entry| entry.page == *page && entry.view.id == *id)
            .map(|entry| (entry.page, &entry.view))
    }

    /// Whether the note-placement tool is armed.
    #[must_use]
    pub fn note_tool_active(&self) -> bool {
        self.comments.note_tool
    }

    /// Whether a note draft is waiting for Save or Cancel.
    #[must_use]
    pub fn note_draft_open(&self) -> bool {
        self.comments.note_draft.is_some()
    }

    /// The last hint shown by a markup action.
    #[must_use]
    pub fn comments_hint(&self) -> Option<&str> {
        self.comments.hint.as_deref()
    }

    /// The last annotation error, if any.
    #[must_use]
    pub fn comments_error(&self) -> Option<&str> {
        self.comments.error.as_deref()
    }

    /// Re-read every page's annotations from the active session.
    ///
    /// Called after open, tab switch, undo/redo and every annotation change, so
    /// the badge and panel never show state the document no longer has.
    pub fn refresh_annotations(&mut self) {
        let Some(id) = self.state.active_tab_id() else {
            self.comments.reset_document();
            return;
        };
        let Some(session) = self.sessions.get(&id) else {
            self.comments.reset_document();
            return;
        };
        let page_count = session.document().page_count().unwrap_or(0);
        let mut entries = Vec::new();
        for page in 0..page_count {
            match list_annotations(session.document(), page) {
                Ok(views) => {
                    entries.extend(views.into_iter().map(|view| AnnotationEntry { page, view }))
                }
                Err(error) => {
                    self.comments.error = Some(error.to_string());
                }
            }
        }
        self.comments.entries = entries;
        if let Some((page, id)) = self.comments.selected.clone()
            && !self
                .comments
                .entries
                .iter()
                .any(|entry| entry.page == page && entry.view.id == id)
        {
            self.comments.selected = None;
            self.comments.popup_open = false;
        }
    }

    /// Arm the note tool; the next canvas click opens the note draft.
    pub(crate) fn start_note_tool(&mut self) {
        if self.state.active_tab().is_none() {
            self.comments.hint = Some("Open a document before adding a comment.".to_owned());
            self.status = "Open a document before adding a comment".to_owned();
            return;
        }
        self.comments.note_tool = true;
        self.comments.hint = Some("Click a page to place a comment.".to_owned());
        self.status = "Comment: click a page to place a note".to_owned();
    }

    /// Begin a note draft at a page-local point (bottom-left PDF origin).
    ///
    /// This is the public entry point the canvas uses after a click and that a
    /// harness can call directly; it creates no annotation until
    /// `SaveNoteDraft` runs.
    pub fn begin_note_draft_at(&mut self, page: usize, x: f32, y: f32) {
        self.comments.note_draft = Some(NoteDraft {
            page,
            x,
            y,
            text: String::new(),
        });
        self.comments.note_tool = false;
        self.comments.hint = None;
    }

    /// Save the open note draft as an annotation.
    pub(crate) fn save_note_draft(&mut self) {
        let Some(draft) = self.comments.note_draft.take() else {
            return;
        };
        let contents = draft.text.trim().to_owned();
        if contents.is_empty() {
            self.comments.error = Some("A comment needs some text.".to_owned());
            self.comments.note_draft = Some(draft);
            return;
        }
        match self.add_note(draft.page, draft.x, draft.y, &contents) {
            Ok(()) => {
                self.comments.error = None;
                self.comments.hint = None;
                self.status = "Comment added".to_owned();
            }
            Err(error) => {
                self.comments.error = Some(error);
                self.comments.note_draft = Some(draft);
            }
        }
    }

    /// Replace the draft text (the note window binds this field directly).
    pub fn set_note_draft_text(&mut self, text: String) {
        if let Some(draft) = &mut self.comments.note_draft {
            draft.text = text;
        }
    }

    /// The note draft text, or `None` when no draft is open.
    #[must_use]
    pub fn note_draft_text(&self) -> Option<&str> {
        self.comments
            .note_draft
            .as_ref()
            .map(|draft| draft.text.as_str())
    }

    /// Discard the note draft.
    pub(crate) fn cancel_note_draft(&mut self) {
        self.comments.note_draft = None;
        self.comments.note_tool = false;
    }

    /// Add a highlight over the current text selection.
    ///
    /// Without a selection nothing is created and a short hint explains how to
    /// make one.
    pub(crate) fn highlight_selection(&mut self) {
        let groups = self.selection_quads();
        if groups.is_empty() {
            self.comments.hint =
                Some("Select text on the page first, then choose Highlight.".to_owned());
            self.status = "Select text before highlighting".to_owned();
            return;
        }
        let author = self.settings.display_name.clone();
        let result = (|| -> Result<usize, String> {
            let (id, mut candidate) = self.annotation_target()?;
            let mut added = 0usize;
            for (page, quads) in &groups {
                add_highlight(&mut candidate, *page, quads, "", &author)
                    .map_err(|error| error.to_string())?;
                added += 1;
            }
            self.commit_annotation(id, candidate, added)
        })();
        match result {
            Ok(added) => {
                self.comments.error = None;
                self.comments.hint = None;
                self.selection.clear();
                self.status = format!("Highlighted {added} page(s)");
            }
            Err(error) => self.comments.error = Some(error),
        }
    }

    /// Scroll to and open the popup for one annotation.
    pub(crate) fn open_annotation_popup(&mut self, page: usize, id: &str) {
        self.comments.selected = Some((page, id.to_owned()));
        self.comments.popup_open = true;
        self.comments.editing = false;
        self.comments.reply_text.clear();
        self.pending_page_scroll = Some(page);
    }

    /// Close the annotation popup.
    pub(crate) fn close_annotation_popup(&mut self) {
        self.comments.popup_open = false;
        self.comments.selected = None;
        self.comments.editing = false;
    }

    /// Reply in the open thread.
    pub(crate) fn reply_to_annotation(&mut self, page: usize, id: &str, contents: &str) {
        let contents = contents.trim();
        if contents.is_empty() {
            return;
        }
        let author = self.settings.display_name.clone();
        let result = (|| -> Result<(), String> {
            let (tab, mut candidate) = self.annotation_target()?;
            reply_to(&mut candidate, page, id, contents, &author)
                .map_err(|error| error.to_string())?;
            self.commit_annotation(tab, candidate, ())
        })();
        match result {
            Ok(()) => {
                self.comments.reply_text.clear();
                self.comments.error = None;
                self.status = "Reply added".to_owned();
            }
            Err(error) => self.comments.error = Some(error),
        }
    }

    /// Resolve or reopen a comment.
    pub(crate) fn set_annotation_resolved(&mut self, page: usize, id: &str, resolved: bool) {
        let state = if resolved {
            AnnotationState::Accepted
        } else {
            AnnotationState::Unmarked
        };
        let result = (|| -> Result<(), String> {
            let (tab, mut candidate) = self.annotation_target()?;
            set_state(&mut candidate, page, id, state).map_err(|error| error.to_string())?;
            self.commit_annotation(tab, candidate, ())
        })();
        match result {
            Ok(()) => {
                self.comments.error = None;
                self.status = if resolved {
                    "Comment resolved".to_owned()
                } else {
                    "Comment reopened".to_owned()
                };
            }
            Err(error) => self.comments.error = Some(error),
        }
    }

    /// Replace a comment's text.
    pub(crate) fn update_annotation_contents(&mut self, page: usize, id: &str, contents: &str) {
        let result = (|| -> Result<(), String> {
            let (tab, mut candidate) = self.annotation_target()?;
            update_contents(&mut candidate, page, id, contents)
                .map_err(|error| error.to_string())?;
            self.commit_annotation(tab, candidate, ())
        })();
        match result {
            Ok(()) => {
                self.comments.editing = false;
                self.comments.error = None;
                self.status = "Comment updated".to_owned();
            }
            Err(error) => self.comments.error = Some(error),
        }
    }

    /// Delete an annotation and its replies.
    pub(crate) fn delete_annotation(&mut self, page: usize, id: &str) {
        let replies: Vec<String> = self
            .comments
            .entries
            .iter()
            .filter(|entry| entry.page == page)
            .filter(|entry| entry.view.parent_id.as_deref() == Some(id))
            .map(|entry| entry.view.id.clone())
            .collect();
        let result = (|| -> Result<(), String> {
            let (tab, mut candidate) = self.annotation_target()?;
            delete_annotation(&mut candidate, page, id).map_err(|error| error.to_string())?;
            for reply in &replies {
                delete_annotation(&mut candidate, page, reply)
                    .map_err(|error| error.to_string())?;
            }
            self.commit_annotation(tab, candidate, ())
        })();
        match result {
            Ok(()) => {
                self.comments.error = None;
                self.comments.popup_open = false;
                self.comments.selected = None;
                self.status = "Comment deleted".to_owned();
            }
            Err(error) => self.comments.error = Some(error),
        }
    }

    /// Clone the active document for a library mutation.
    fn annotation_target(&self) -> Result<(crate::TabId, pdfxi_core::Document), String> {
        let Some(id) = self.state.active_tab_id() else {
            return Err("Open a document first".to_owned());
        };
        if self.read_only_tabs.contains(&id) {
            return Err("The document permissions do not allow changes".to_owned());
        }
        let Some(session) = self.sessions.get(&id) else {
            return Err("The document has no open session".to_owned());
        };
        if let Some(report) = session.decryption_report()
            && !report.permissions.allows(Permission::ANNOTATE)
        {
            return Err("The document permissions do not allow comments".to_owned());
        }
        Ok((id, session.document().clone()))
    }

    /// Commit the mutated clone as one undoable session edit.
    fn commit_annotation<T>(
        &mut self,
        id: crate::TabId,
        candidate: pdfxi_core::Document,
        value: T,
    ) -> Result<T, String> {
        let result = self
            .sessions
            .get_mut(&id)
            .ok_or_else(|| "The document has no open session".to_owned())?
            .edit(|document| {
                *document = candidate;
                Ok(value)
            });
        match result {
            Ok(value) => {
                self.after_session_edit(id);
                Ok(value)
            }
            Err(error) => Err(error.to_string()),
        }
    }

    fn next_annotation_id(&mut self, prefix: &str) -> String {
        self.comments.next_id = self.comments.next_id.wrapping_add(1);
        format!("pdfxi-{prefix}-{}", self.comments.next_id)
    }

    pub(crate) fn add_note(
        &mut self,
        page: usize,
        x: f32,
        y: f32,
        contents: &str,
    ) -> Result<(), String> {
        let author = self.settings.display_name.clone();
        let note_id = self.next_annotation_id("note");
        let (id, mut candidate) = self.annotation_target()?;
        add_text_note(
            &mut candidate,
            page,
            Point::new(x, y),
            contents,
            &author,
            &note_id,
        )
        .map_err(|error| error.to_string())?;
        self.commit_annotation(id, candidate, ())
    }

    /// Selected spans converted to page-local PDF rectangles, grouped by page.
    pub(crate) fn selection_quads(&self) -> Vec<(usize, Vec<Rect>)> {
        let Some(selection) = self.selection.selection() else {
            return Vec::new();
        };
        if selection.is_empty() {
            return Vec::new();
        }
        let page_height = |page: usize| {
            self.page_sizes
                .get(page)
                .map_or(792.0, |size| size.height_pt)
        };
        let mut groups = Vec::new();
        for page_index in selection.start.page..=selection.end.page {
            let Some(page) = self
                .text_pages
                .iter()
                .find(|page| page.page_number() == page_index)
            else {
                continue;
            };
            let start = if page_index == selection.start.page {
                selection.start.offset
            } else {
                0
            };
            let end = if page_index == selection.end.page {
                selection.end.offset
            } else {
                usize::MAX
            };
            let height = page_height(page_index);
            let mut cursor = 0usize;
            let mut quads = Vec::new();
            for span in page.spans() {
                let span_start = cursor;
                let span_end = cursor + span.text.chars().count();
                cursor = span_end;
                if span_end <= start || span_start >= end || span.width <= 0.0 || span.height <= 0.0
                {
                    continue;
                }
                let x0 = span.x;
                let x1 = span.x + span.width;
                let y0 = height - (span.y + span.height);
                let y1 = height - span.y;
                if let Ok(rect) = Rect::new(x0, y0, x1, y1) {
                    quads.push(rect);
                }
            }
            if !quads.is_empty() {
                groups.push((page_index, quads));
            }
        }
        groups
    }

    /// The Comments rail badge: the real annotation count.
    pub(crate) fn comments_badge(&self) -> Option<usize> {
        let count = self.annotation_count();
        (count > 0).then_some(count)
    }

    /// Side-panel list, grouped by page.
    pub(crate) fn comments_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        // Keep the list inside the fixed panel width: a long comment must not
        // widen the side panel and push the canvas off-screen.
        ui.set_max_width(ui.available_width());
        ui.horizontal(|ui| {
            ui.label(egui::RichText::new("Comments").size(15.0).strong());
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                ui.label(
                    egui::RichText::new(format!("{}", self.annotation_count()))
                        .color(palette.text_muted),
                );
            });
        });
        if let Some(hint) = self.comments.hint.clone() {
            ui.label(egui::RichText::new(hint).color(palette.text_muted));
        }
        if let Some(error) = self.comments.error.clone() {
            ui.colored_label(ui.visuals().error_fg_color, error);
        }
        if self.state.active_tab().is_none() {
            ui.add_space(8.0);
            ui.label(egui::RichText::new("Open a document to review comments.").weak());
            return;
        }
        let entries = self.comments.entries.clone();
        if entries.is_empty() {
            ui.add_space(8.0);
            ui.label(
                egui::RichText::new(
                    "No comments in this document. Choose Comment in the command bar, then click a page.",
                )
                .color(palette.text_muted),
            );
            return;
        }
        let mut last_page = None;
        for entry in &entries {
            if last_page != Some(entry.page) {
                last_page = Some(entry.page);
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(format!("Page {}", entry.page + 1))
                        .size(11.0)
                        .color(palette.text_muted),
                );
            }
            let selected = self
                .comments
                .selected
                .as_ref()
                .is_some_and(|(page, id)| *page == entry.page && *id == entry.view.id);
            ui.horizontal(|ui| {
                let glyph = subtype_icon(&entry.view.subtype);
                let (rect, _) =
                    ui.allocate_exact_size(egui::vec2(14.0, 14.0), egui::Sense::hover());
                icon(ui.painter(), rect, glyph, palette.text_muted);
                let label = format!(
                    "{}  {}",
                    subtype_label(&entry.view.subtype),
                    snippet(&entry.view.contents)
                );
                let mut text = egui::RichText::new(label);
                if selected {
                    text = text.background_color(palette.accent_soft);
                }
                let response = ui.add(
                    egui::Label::new(text)
                        .truncate()
                        .sense(egui::Sense::click()),
                );
                if response.clicked() {
                    self.dispatch(
                        AppCommand::OpenAnnotation {
                            page: entry.page,
                            id: entry.view.id.clone(),
                        },
                        Some(ui.ctx()),
                    );
                }
            });
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {}",
                        entry.view.author,
                        entry
                            .view
                            .date
                            .as_deref()
                            .map(display_date)
                            .unwrap_or_default()
                    ))
                    .size(10.0)
                    .color(palette.text_muted),
                );
            });
            ui.horizontal(|ui| {
                ui.add_space(18.0);
                let mut resolved = is_resolved(&entry.view);
                if ui.checkbox(&mut resolved, "Resolved").changed() {
                    self.dispatch(
                        AppCommand::SetAnnotationResolved {
                            page: entry.page,
                            id: entry.view.id.clone(),
                            resolved,
                        },
                        Some(ui.ctx()),
                    );
                }
                if ui.small_button("Delete").clicked() {
                    self.dispatch(
                        AppCommand::DeleteAnnotation {
                            page: entry.page,
                            id: entry.view.id.clone(),
                        },
                        Some(ui.ctx()),
                    );
                }
            });
            ui.add_space(4.0);
        }
    }

    /// Note draft window.
    pub(crate) fn note_draft_ui(&mut self, ctx: &egui::Context) {
        let Some(mut draft) = self.comments.note_draft.take() else {
            return;
        };
        let mut save = false;
        let mut cancel = false;
        egui::Window::new("Add comment")
            .collapsible(false)
            .resizable(true)
            .default_width(300.0)
            .anchor(egui::Align2::CENTER_CENTER, egui::vec2(0.0, 0.0))
            .show(ctx, |ui| {
                ui.label(format!("Page {}", draft.page + 1));
                ui.add(
                    egui::TextEdit::multiline(&mut draft.text)
                        .hint_text("Write a comment")
                        .desired_rows(4)
                        .desired_width(280.0),
                );
                if let Some(error) = &self.comments.error {
                    ui.colored_label(ui.visuals().error_fg_color, error);
                }
                ui.horizontal(|ui| {
                    if ui.button("Save").clicked() {
                        save = true;
                    }
                    if ui.button("Cancel").clicked() {
                        cancel = true;
                    }
                });
            });
        if save {
            self.comments.note_draft = Some(draft);
            self.dispatch(AppCommand::SaveNoteDraft, Some(ctx));
        } else if cancel {
            self.dispatch(AppCommand::CancelNoteDraft, Some(ctx));
        } else {
            self.comments.note_draft = Some(draft);
        }
    }

    /// Comment thread popup anchored near the annotation on the canvas.
    pub(crate) fn annotation_popup_ui(
        &mut self,
        ui: &mut egui::Ui,
        layout: &CanvasLayout,
        origin: egui::Pos2,
        pad_x: f32,
    ) {
        if !self.comments.popup_open {
            return;
        }
        let ctx = ui.ctx().clone();
        if ctx.input(|input| input.key_pressed(egui::Key::Escape)) {
            self.dispatch(AppCommand::CloseAnnotationPopup, Some(&ctx));
            return;
        }
        let Some((page, view)) = self
            .selected_annotation()
            .map(|(page, view)| (page, view.clone()))
        else {
            return;
        };
        let height = self
            .page_sizes
            .get(page)
            .map_or(792.0, |size| size.height_pt);
        let local = CanvasRect::new(
            view.rect.x0(),
            height - view.rect.y1(),
            view.rect.x1() - view.rect.x0(),
            view.rect.y1() - view.rect.y0(),
        );
        let Some(mapped) = layout.page_rect_to_canvas(page, local) else {
            return;
        };
        let anchor = egui::pos2(
            origin.x + mapped.x - self.scroll_x + pad_x + mapped.width + 8.0,
            origin.y + mapped.y - self.scroll_y,
        );
        let screen = ctx.content_rect();
        let mut position = anchor;
        if position.x + 368.0 > screen.right() {
            position.x = (screen.right() - 368.0).max(screen.left() + 8.0);
        }
        position.y = position.y.clamp(
            screen.top() + 8.0,
            (screen.bottom() - 160.0).max(screen.top() + 8.0),
        );
        let replies: Vec<AnnotationView> = self
            .comments
            .entries
            .iter()
            .filter(|entry| entry.page == page)
            .filter(|entry| entry.view.parent_id.as_deref() == Some(view.id.as_str()))
            .map(|entry| entry.view.clone())
            .collect();
        let mut open = true;
        egui::Window::new(format!("Comment - {}", view.author))
            .fixed_pos(position)
            .max_width(360.0)
            .max_height((screen.height() - 32.0).max(160.0))
            .resizable(false)
            .open(&mut open)
            .show(&ctx, |ui| {
                ui.set_max_width(360.0);
                ui.label(egui::RichText::new(&view.contents).strong());
                ui.label(
                    egui::RichText::new(format!(
                        "{}  {}",
                        view.author,
                        view.date.as_deref().map(display_date).unwrap_or_default()
                    ))
                    .size(10.0)
                    .weak(),
                );
                ui.separator();
                for reply in &replies {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(egui::RichText::new(&reply.author).strong());
                        ui.label(egui::RichText::new(&reply.contents));
                    });
                }
                ui.add(
                    egui::TextEdit::singleline(&mut self.comments.reply_text)
                        .hint_text("Reply")
                        .desired_width(ui.available_width()),
                );
                ui.horizontal(|ui| {
                    if ui.button("Reply").clicked() {
                        self.dispatch(
                            AppCommand::ReplyToAnnotation {
                                page,
                                id: view.id.clone(),
                                contents: self.comments.reply_text.clone(),
                            },
                            Some(ui.ctx()),
                        );
                    }
                    let resolved = is_resolved(&view);
                    if ui
                        .button(if resolved { "Reopen" } else { "Resolve" })
                        .clicked()
                    {
                        self.dispatch(
                            AppCommand::SetAnnotationResolved {
                                page,
                                id: view.id.clone(),
                                resolved: !resolved,
                            },
                            Some(ui.ctx()),
                        );
                    }
                    if ui.button("Edit").clicked() {
                        self.comments.editing = true;
                        self.comments.edit_text = view.contents.clone();
                    }
                    if ui.button("Delete").clicked() {
                        self.dispatch(
                            AppCommand::DeleteAnnotation {
                                page,
                                id: view.id.clone(),
                            },
                            Some(ui.ctx()),
                        );
                    }
                });
                if self.comments.editing {
                    ui.add(
                        egui::TextEdit::multiline(&mut self.comments.edit_text)
                            .desired_rows(3)
                            .desired_width(ui.available_width()),
                    );
                    if ui.button("Save edit").clicked() {
                        self.dispatch(
                            AppCommand::UpdateAnnotation {
                                page,
                                id: view.id.clone(),
                                contents: self.comments.edit_text.clone(),
                            },
                            Some(ui.ctx()),
                        );
                    }
                }
            });
        if !open {
            self.dispatch(AppCommand::CloseAnnotationPopup, Some(&ctx));
        }
    }
}

/// Map a clicked page point (top-left, y-down) to a PDF user-space note point.
#[must_use]
pub fn note_point(page_height_pt: f32, x_pt: f32, y_down_pt: f32) -> Point {
    Point::new(x_pt, page_height_pt - y_down_pt)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn note_point_flips_the_page_axis() {
        let point = note_point(792.0, 72.0, 100.0);
        assert_eq!(point.x, 72.0);
        assert_eq!(point.y, 692.0);
    }

    #[test]
    fn pdf_dates_format_humanly_and_keep_unparsable_text() {
        // No timezone: keep the recorded wall time.
        assert_eq!(display_date("D:20261120222352"), "2026-11-20 22:23");
        // Zoned values convert to local time; compare against an independent
        // conversion so the test is not tied to this machine's zone.
        let expected = |offset_seconds: i32| {
            let date =
                time::Date::from_calendar_date(2026, time::Month::November, 20).expect("date");
            let clock = time::Time::from_hms(22, 23, 52).expect("time");
            let offset = time::UtcOffset::from_whole_seconds(offset_seconds).expect("offset");
            let local = time::UtcOffset::current_local_offset().unwrap_or(offset);
            time::OffsetDateTime::new_in_offset(date, clock, offset)
                .to_offset(local)
                .format(&time::macros::format_description!(
                    "[year]-[month]-[day] [hour]:[minute]"
                ))
                .expect("format")
        };
        assert_eq!(display_date("D:20261120222352Z"), expected(0));
        assert_eq!(display_date("D:20261120222352+02'00'"), expected(2 * 3600));
        // Malformed values are shown unchanged, never hidden.
        assert_eq!(display_date("D:2026"), "D:2026");
        assert_eq!(display_date("not a date"), "not a date");
    }

    #[test]
    fn snippets_collapse_whitespace_and_truncate() {
        assert_eq!(snippet("  a\n b  "), "a b");
        let long = "word ".repeat(20);
        let short = snippet(&long);
        assert!(short.ends_with('…'));
        assert_eq!(short.chars().count(), 42);
    }
}

#[cfg(all(test, feature = "pdfium"))]
mod integration_tests {
    use super::*;
    use crate::selection::TextAnchor;
    use std::path::PathBuf;

    fn fixture() -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf")
    }

    fn temp_dir(tag: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("pdfxi-t0242-{}-{tag}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).expect("temp dir");
        dir
    }

    fn open_excel() -> PdfxiApp {
        let mut app = PdfxiApp::new();
        assert!(matches!(
            app.open_document(&fixture()),
            crate::OpenOutcome::Opened { .. }
        ));
        app
    }

    #[test]
    fn note_is_created_listed_saved_and_reopened() {
        let dir = temp_dir("note");
        let saved = dir.join("annotated.pdf");
        let mut app = open_excel();
        assert_eq!(app.annotation_count(), 0);

        app.dispatch(
            AppCommand::AddNote {
                page: 0,
                x: 72.0,
                y: 600.0,
                contents: "Please check this figure".to_owned(),
            },
            None,
        );
        assert_eq!(app.annotation_count(), 1);
        let entry = app.annotations().first().expect("entry");
        assert_eq!(entry.page, 0);
        assert_eq!(entry.view.contents, "Please check this figure");
        assert_eq!(entry.view.author, app.display_name());
        assert_eq!(entry.view.subtype, AnnotationSubtype::Text);
        assert!(app.state().active_tab().is_some_and(|tab| tab.is_dirty()));

        app.save_active_as(&saved).expect("save");
        let reopened = pdfxi_document::DocumentSession::open(&saved).expect("reopen");
        let listed = list_annotations(reopened.document(), 0).expect("list");
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].contents, "Please check this figure");

        let mut viewer = PdfxiApp::new();
        assert!(matches!(
            viewer.open_document(&saved),
            crate::OpenOutcome::Opened { .. }
        ));
        assert_eq!(viewer.annotation_count(), 1);
        assert_eq!(
            viewer.annotations()[0].view.contents,
            "Please check this figure"
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn reply_resolve_and_delete_follow_the_badge() {
        let mut app = open_excel();
        app.dispatch(
            AppCommand::AddNote {
                page: 0,
                x: 72.0,
                y: 500.0,
                contents: "Root comment".to_owned(),
            },
            None,
        );
        let root = app.annotations()[0].view.id.clone();
        assert_eq!(app.annotation_count(), 1);

        app.dispatch(
            AppCommand::ReplyToAnnotation {
                page: 0,
                id: root.clone(),
                contents: "Agreed".to_owned(),
            },
            None,
        );
        assert_eq!(app.annotation_count(), 2);
        let reply = app
            .annotations()
            .iter()
            .find(|entry| entry.view.parent_id.as_deref() == Some(root.as_str()))
            .expect("reply");
        assert_eq!(reply.view.contents, "Agreed");

        app.dispatch(
            AppCommand::SetAnnotationResolved {
                page: 0,
                id: root.clone(),
                resolved: true,
            },
            None,
        );
        let resolved = app
            .annotations()
            .iter()
            .find(|entry| entry.view.id == root)
            .expect("root");
        assert_eq!(resolved.view.state.as_deref(), Some("Accepted"));
        assert_eq!(app.annotation_count(), 2);

        app.dispatch(AppCommand::DeleteAnnotation { page: 0, id: root }, None);
        assert_eq!(app.annotation_count(), 0, "delete removes the thread");
    }

    #[test]
    fn highlight_uses_the_selection_quads() {
        let mut app = open_excel();
        assert!(!app.text_pages.is_empty(), "fixture text must be extracted");
        let spans = app.text_pages[0].spans().len();
        assert!(spans > 0);
        app.selection
            .drag(TextAnchor::new(0, 0), TextAnchor::new(0, 12));
        assert!(!app.selection_quads().is_empty());

        let generation = app.render_generation();
        app.dispatch(AppCommand::AddHighlight, None);
        assert_eq!(app.annotation_count(), 1);
        assert!(
            app.render_generation() > generation,
            "a highlight edit must reload the canvas source"
        );
        assert!(
            app.textures.is_empty(),
            "old page textures must be dropped after an edit"
        );
        assert!(
            app.raster_cache.is_empty(),
            "old rasters must be dropped after an edit"
        );
        let renderer = app.pdfium_renderer.as_ref().expect("canvas renderer");
        let raster = pdfxi_render::Renderer::render_page(
            renderer,
            pdfxi_render::PageIndex::new(0),
            1.0,
            &pdfxi_render::CancellationToken::new(),
            &mut |_| {},
        )
        .expect("rasterize the edited page");
        assert!(
            raster
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| { pixel[0] > 200 && pixel[1] > 200 && pixel[2] < 150 }),
            "the new highlight must be in the canvas raster, not only the thumbnail"
        );
        let entry = &app.annotations()[0];
        assert_eq!(entry.page, 0);
        assert_eq!(entry.view.subtype, AnnotationSubtype::Highlight);
        assert!(app.selection.selection().is_none_or(|s| s.is_empty()));

        let mut app_without_selection = open_excel();
        app_without_selection.dispatch(AppCommand::AddHighlight, None);
        assert_eq!(app_without_selection.annotation_count(), 0);
        assert!(app_without_selection.comments_hint().is_some());
    }

    #[test]
    fn undo_removes_a_note_from_the_panel_and_from_saved_bytes() {
        let dir = temp_dir("undo");
        let saved = dir.join("undone.pdf");
        let mut app = open_excel();
        app.dispatch(
            AppCommand::AddNote {
                page: 0,
                x: 100.0,
                y: 400.0,
                contents: "Temporary".to_owned(),
            },
            None,
        );
        assert_eq!(app.annotation_count(), 1);

        app.undo_document().expect("undo");
        assert_eq!(app.annotation_count(), 0);
        app.save_active_as(&saved).expect("save");
        let reopened = pdfxi_document::DocumentSession::open(&saved).expect("reopen");
        assert!(
            list_annotations(reopened.document(), 0)
                .expect("list")
                .is_empty()
        );

        let _ = std::fs::remove_dir_all(&dir);
    }

    /// The whole draft flow is reachable without a canvas click: begin, type,
    /// save (one new annotation), then begin a second draft and cancel it.
    #[test]
    fn note_draft_can_be_driven_without_a_click() {
        let mut app = open_excel();
        app.begin_note_draft_at(0, 96.0, 480.0);
        assert!(app.note_draft_open());
        assert_eq!(app.note_draft_text(), Some(""));

        app.set_note_draft_text("Draft written by the harness".to_owned());
        assert_eq!(app.note_draft_text(), Some("Draft written by the harness"));
        app.dispatch(AppCommand::SaveNoteDraft, None);
        assert!(!app.note_draft_open());
        assert_eq!(app.annotation_count(), 1);
        assert_eq!(
            app.annotations()[0].view.contents,
            "Draft written by the harness"
        );

        app.begin_note_draft_at(0, 120.0, 420.0);
        assert!(app.note_draft_open());
        app.dispatch(AppCommand::CancelNoteDraft, None);
        assert!(!app.note_draft_open());
        assert_eq!(app.annotation_count(), 1, "cancel creates nothing");
    }

    /// Produce the annotated document the evidence captures open.
    ///
    /// The note, reply and highlight are created through the same public
    /// commands the UI uses; no demo flag or canned launch content exists.
    #[test]
    fn writes_annotated_document_for_captures() {
        let workspace = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
        let out_dir = workspace.join("target/lanes/app/t0242-capture");
        std::fs::create_dir_all(&out_dir).expect("capture folder");
        let output = out_dir.join("commented.pdf");
        let _ = std::fs::remove_file(&output);

        let mut app = open_excel();
        app.dispatch(
            AppCommand::AddNote {
                page: 0,
                x: 96.0,
                y: 600.0,
                contents: "Please confirm the totals on this page.".to_owned(),
            },
            None,
        );
        let root = app.annotations()[0].view.id.clone();
        app.dispatch(
            AppCommand::ReplyToAnnotation {
                page: 0,
                id: root,
                contents: "Checked - the totals match.".to_owned(),
            },
            None,
        );
        app.selection
            .drag(TextAnchor::new(0, 0), TextAnchor::new(0, 18));
        app.dispatch(AppCommand::AddHighlight, None);
        assert_eq!(app.annotation_count(), 3);
        app.save_active_as(&output).expect("save capture document");
        assert!(output.is_file(), "capture document must exist");
        let reopened = pdfxi_document::DocumentSession::open(&output).expect("reopen");
        assert_eq!(
            list_annotations(reopened.document(), 0)
                .expect("list")
                .len(),
            3
        );

        // Opening the generated document is not an edit: no dirty marker and no
        // undo entry. The capture shows the tab exactly as a user receives it.
        let mut viewer = PdfxiApp::new();
        assert!(matches!(
            viewer.open_document(&output),
            crate::OpenOutcome::Opened { .. }
        ));
        assert_eq!(viewer.annotation_count(), 3);
        assert!(!viewer.can_undo(), "opening must not create a revision");
        assert!(
            !viewer
                .state()
                .active_tab()
                .is_some_and(|tab| tab.is_dirty())
        );

        // The startup deep link only opens UI state.
        let entry = viewer.annotations()[0].clone();
        viewer.dispatch(
            AppCommand::OpenAnnotation {
                page: entry.page,
                id: entry.view.id,
            },
            None,
        );
        assert!(!viewer.can_undo(), "opening a thread must not edit the PDF");

        // The saved appearance must be in the raster PDFium produces for the
        // canvas at every scale the viewer uses, not just in the thumbnail.
        let renderer =
            pdfxi_render::PdfiumRenderer::from_bytes(std::fs::read(&output).expect("bytes"))
                .expect("render saved file");
        for scale in [0.2_f32, 0.5, 1.0, 1.5] {
            let raster = pdfxi_render::Renderer::render_page(
                &renderer,
                pdfxi_render::PageIndex::new(0),
                scale,
                &pdfxi_render::CancellationToken::new(),
                &mut |_| {},
            )
            .expect("rasterize saved page");
            let has_highlight = raster
                .pixels()
                .as_chunks::<4>()
                .0
                .iter()
                .any(|pixel| pixel[0] > 200 && pixel[1] > 200 && pixel[2] < 150);
            assert!(
                has_highlight,
                "saved highlight missing from the {scale} scale raster"
            );
        }
    }
}
