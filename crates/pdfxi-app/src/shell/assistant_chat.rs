//! Per-document assistant conversation, ask/stop flow and page citations
//! (T-0253, Round A).
//!
//! The panel is the app half of ADR 0011. It owns nothing about HTTP: one turn
//! is handed to a [`TurnRunner`], which by default calls the T-0251 client's
//! session and `prompt`/`ask` routes on a worker thread. The UI thread only
//! drains a channel and repaints, so a slow or cancelled turn never blocks a
//! frame.
//!
//! Round A never reads or sends document text. The prompt is exactly the
//! question the user typed; citations are found in the answer text and only
//! navigate the open document. The locked session, MCP attachment and
//! `BudgetedTools` wrapper are Round B.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use pdfxi_assistant::{
    Assistant, AssistantError, CancelFlag, ModelRef, ServiceEndpoint, SessionRequest,
};

use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};

use super::assistant_connect::state_color;
use super::capabilities::Capability;

/// Hard cap on a prompt typed into the ask box, in characters.
const MAX_QUESTION_CHARS: usize = 4_000;

/// A page citation parsed from an answer.
///
/// Pages are one-based, exactly as the model wrote them, so the UI never has to
/// guess whether "page 2" meant the second page.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(crate) struct Citation {
    /// One-based page number from the answer text.
    pub(crate) page: u32,
}

/// One message in the per-document conversation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ChatMessage {
    /// True for the user's question, false for the assistant answer.
    from_user: bool,
    /// Text shown in the panel.
    text: String,
    /// Page citations parsed from the answer; empty for a question.
    citations: Vec<Citation>,
}

impl ChatMessage {
    /// A question typed by the user. Questions never carry citations.
    fn question(text: impl Into<String>) -> Self {
        Self {
            from_user: true,
            text: text.into(),
            citations: Vec::new(),
        }
    }

    /// An answer returned by the service; citations come from its text only.
    fn answer(text: impl Into<String>) -> Self {
        let text = text.into();
        let citations = parse_citations(&text);
        Self {
            from_user: false,
            text,
            citations,
        }
    }

    /// Whether this message is the user's question.
    pub(crate) fn is_question(&self) -> bool {
        self.from_user
    }

    /// Text to render.
    pub(crate) fn text(&self) -> &str {
        &self.text
    }

    /// Pages cited by an answer, in first-appearance order.
    pub(crate) fn citations(&self) -> &[Citation] {
        &self.citations
    }
}

/// One turn handed to a [`TurnRunner`].
#[derive(Debug, Clone)]
pub(crate) struct TurnRequest {
    /// Existing session for this conversation, when one was created.
    pub(crate) session: Option<String>,
    /// Model the existing session was last set to.
    pub(crate) session_model: Option<ModelRef>,
    /// Model selected in the connection screen.
    pub(crate) model: ModelRef,
    /// The user's question. Round A sends no document text.
    pub(crate) prompt: String,
}

/// A completed turn.
#[derive(Debug, Clone)]
pub(crate) struct TurnOutcome {
    /// Session the answer belongs to.
    pub(crate) session: String,
    /// Model used for the turn.
    pub(crate) model: ModelRef,
    /// Answer text, reasoning excluded by the T-0251 client.
    pub(crate) answer: String,
}

/// Runs one assistant turn off the UI thread.
///
/// The trait is the test seam: a loopback mock exercises the real T-0251 client,
/// while a recording or slow runner can prove Stop and focus behaviour without
/// a network.
pub(crate) trait TurnRunner: Send + Sync {
    /// Send `request` and wait for the turn to finish or be cancelled.
    fn run(
        &self,
        endpoint: &ServiceEndpoint,
        request: TurnRequest,
        cancel: &CancelFlag,
    ) -> Result<TurnOutcome, AssistantError>;
}

/// The shipped runner: one T-0251 session per conversation, then `ask`.
///
/// Round A creates a plain session because the locked-session constructor is
/// deliberately deferred to Round B. No document text and no MCP server are
/// attached, so a document cannot drive a tool call. The panel says so.
pub(crate) struct ServiceTurnRunner;

impl TurnRunner for ServiceTurnRunner {
    fn run(
        &self,
        endpoint: &ServiceEndpoint,
        request: TurnRequest,
        cancel: &CancelFlag,
    ) -> Result<TurnOutcome, AssistantError> {
        let assistant = Assistant::connect(endpoint.clone());
        let session = match request.session.clone() {
            Some(id) => {
                if request.session_model.as_ref() != Some(&request.model) {
                    assistant.set_model(&id, &request.model, cancel)?;
                }
                id
            }
            None => {
                let create = SessionRequest::new(
                    "PDFXI assistant",
                    request.model.clone(),
                    std::env::temp_dir(),
                )?;
                assistant.create_session(&create, cancel)?.id
            }
        };
        let answer = assistant.ask(&session, &request.prompt, cancel)?;
        Ok(TurnOutcome {
            session,
            model: request.model,
            answer: answer.text,
        })
    }
}

/// A running turn and the channel its result arrives on.
struct ChatJob {
    receiver: Receiver<Result<TurnOutcome, AssistantError>>,
    cancel: CancelFlag,
    _handle: JoinHandle<()>,
}

/// Connection-independent conversation state for the active document.
pub(crate) struct AssistantChat {
    document: Option<crate::TabId>,
    messages: Vec<ChatMessage>,
    input: String,
    state: ChatState,
    job: Option<ChatJob>,
    session: Option<String>,
    session_model: Option<ModelRef>,
    runner: Arc<dyn TurnRunner>,
    error: Option<String>,
    /// Set after an answer so the ask box can take keyboard focus for a follow-up.
    focus_input: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ChatState {
    Idle,
    Thinking,
}

impl Default for AssistantChat {
    fn default() -> Self {
        Self {
            document: None,
            messages: Vec::new(),
            input: String::new(),
            state: ChatState::Idle,
            job: None,
            session: None,
            session_model: None,
            runner: Arc::new(ServiceTurnRunner),
            error: None,
            focus_input: false,
        }
    }
}

impl AssistantChat {
    /// Messages for the active document, oldest first.
    pub(crate) fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    /// The question being typed.
    pub(crate) fn input(&self) -> &str {
        &self.input
    }

    /// Replace the question being typed.
    pub(crate) fn set_input(&mut self, text: impl Into<String>) {
        self.input = text.into();
        if self.input.chars().count() > MAX_QUESTION_CHARS {
            self.input = self.input.chars().take(MAX_QUESTION_CHARS).collect();
        }
    }

    /// Take the editable text for one egui frame.
    pub(crate) fn take_input(&mut self) -> String {
        std::mem::take(&mut self.input)
    }

    /// Put the editable text back after the frame.
    pub(crate) fn restore_input(&mut self, text: String) {
        self.input = text;
    }

    /// Whether a turn is running.
    pub(crate) fn is_thinking(&self) -> bool {
        self.state == ChatState::Thinking
    }

    /// The last turn's redacted error text, if any.
    pub(crate) fn error(&self) -> Option<&str> {
        self.error.as_deref()
    }

    /// Whether the ask box should take focus on this frame (after an answer).
    pub(crate) fn take_focus_input(&mut self) -> bool {
        std::mem::take(&mut self.focus_input)
    }

    /// Reset the conversation when the active document changes.
    ///
    /// Opening a document, switching tabs, or closing the last tab all produce a
    /// different [`crate::TabId`], so the previous document's messages and
    /// session never appear under a new document.
    pub(crate) fn sync_document(&mut self, document: Option<crate::TabId>) {
        if self.document == document {
            return;
        }
        self.document = document;
        self.reset();
    }

    /// Clear messages, the session and any running turn.
    pub(crate) fn reset(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.cancel();
        }
        self.messages.clear();
        self.input.clear();
        self.state = ChatState::Idle;
        self.session = None;
        self.session_model = None;
        self.error = None;
        self.focus_input = false;
    }

    /// Send the typed question on a worker thread.
    ///
    /// Returns `false` for an empty question or a turn already in flight, so the
    /// Ask control can stay disabled instead of sending a duplicate.
    pub(crate) fn begin(
        &mut self,
        endpoint: ServiceEndpoint,
        model: ModelRef,
        document: Option<crate::TabId>,
    ) -> bool {
        let prompt = self.input.trim().to_owned();
        if prompt.is_empty() || self.is_thinking() {
            return false;
        }
        self.sync_document(document);
        self.input.clear();
        self.error = None;
        self.focus_input = false;
        self.messages.push(ChatMessage::question(prompt.clone()));
        let request = TurnRequest {
            session: self.session.clone(),
            session_model: self.session_model.clone(),
            model,
            prompt,
        };
        let (sender, receiver) = mpsc::channel();
        let cancel = CancelFlag::new();
        let worker_cancel = cancel.clone();
        let runner = Arc::clone(&self.runner);
        let handle = std::thread::spawn(move || {
            let result = runner.run(&endpoint, request, &worker_cancel);
            let _ = sender.send(result);
        });
        self.job = Some(ChatJob {
            receiver,
            cancel,
            _handle: handle,
        });
        self.state = ChatState::Thinking;
        true
    }

    /// Cancel the running turn and return to idle immediately.
    ///
    /// The worker observes the flag at its next transport or polling boundary;
    /// its eventual result is dropped with the receiver.
    pub(crate) fn stop(&mut self) -> bool {
        let Some(job) = self.job.take() else {
            return false;
        };
        job.cancel.cancel();
        self.state = ChatState::Idle;
        true
    }

    /// Drain one worker result. Returns true when the panel should repaint.
    pub(crate) fn poll(&mut self) -> bool {
        let Some(job) = self.job.as_ref() else {
            return false;
        };
        match job.receiver.try_recv() {
            Ok(Ok(outcome)) => {
                self.job = None;
                self.session = Some(outcome.session);
                self.session_model = Some(outcome.model);
                self.messages.push(ChatMessage::answer(outcome.answer));
                self.state = ChatState::Idle;
                self.focus_input = true;
                true
            }
            Ok(Err(error)) => {
                self.job = None;
                self.state = ChatState::Idle;
                if !error.is_cancelled() {
                    self.error = Some(error.to_string());
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.state = ChatState::Idle;
                self.error = Some("the assistant turn ended unexpectedly".to_owned());
                true
            }
        }
    }

    /// Replace the turn runner. Test-only seam.
    #[cfg(test)]
    pub(crate) fn set_runner(&mut self, runner: Arc<dyn TurnRunner>) {
        self.runner = runner;
    }

    /// The current session id, for tests and status.
    #[cfg(test)]
    pub(crate) fn session(&self) -> Option<&str> {
        self.session.as_deref()
    }
}

/// Parse page citations from free-form answer text.
///
/// Recognised forms are `page 2`, `p. 2`, `[page 2]` and `[p. 2]` (ASCII case
/// insensitive, whitespace or a colon after the keyword). A number must be
/// separated from the keyword, so `page2`, `pages 4` and `homepage 2` do not
/// produce a citation. A bare `p` is deliberately not a keyword: it would make
/// `Appendix P 2` cite page 2, so the abbreviated form needs its period.
/// Results are one-based, deduplicated, in first-appearance order, and bounded
/// to a plausible page number.
pub(crate) fn parse_citations(text: &str) -> Vec<Citation> {
    // `to_ascii_lowercase` only rewrites ASCII letters, so `lower` has the same
    // length, byte offsets and character boundaries as `text`; the scan can
    // index it directly instead of building an offset table.
    let lower = text.to_ascii_lowercase();
    let mut citations: Vec<Citation> = Vec::new();
    for (start, _) in lower.char_indices() {
        if start > 0 {
            let previous = lower[..start].chars().next_back().unwrap_or(' ');
            if previous.is_alphanumeric() || previous == '_' {
                continue;
            }
        }
        let rest = &lower[start..];
        let keyword_len = if rest.starts_with("page") {
            "page".len()
        } else if rest.starts_with("p.") {
            "p.".len()
        } else {
            continue;
        };
        let after = &rest[keyword_len..];
        let mut separator_len = 0usize;
        for character in after.chars() {
            if character == ':' || character.is_whitespace() {
                separator_len += character.len_utf8();
                if separator_len > 8 {
                    break;
                }
            } else {
                break;
            }
        }
        if separator_len == 0 {
            continue;
        }
        // Accumulate the digits without allocating: saturating arithmetic turns
        // an absurd run into a value the range check below rejects.
        let mut page: u64 = 0;
        let mut digits = 0usize;
        for byte in after.as_bytes()[separator_len..].iter() {
            if !byte.is_ascii_digit() {
                break;
            }
            page = page
                .saturating_mul(10)
                .saturating_add(u64::from(byte - b'0'));
            digits += 1;
        }
        if digits == 0 {
            continue;
        }
        if let Ok(page) = u32::try_from(page)
            && (1..=1_000_000).contains(&page)
            && !citations.iter().any(|citation| citation.page == page)
        {
            citations.push(Citation { page });
        }
    }
    citations
}

/// Parse a `provider/id` label into the wire model reference.
pub(crate) fn parse_model_label(label: &str) -> Option<ModelRef> {
    let (provider, id) = label.split_once('/')?;
    if provider.is_empty() || id.is_empty() || id.contains('/') {
        return None;
    }
    Some(ModelRef::new(provider, id))
}

impl PdfxiApp {
    /// Draw the AI Assistant panel: a connection card, the Connect screen, or
    /// the per-document conversation.
    pub(crate) fn assistant_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new("AI Assistant")
                .strong()
                .size(13.0)
                .color(palette.text),
        );
        ui.add(
            egui::Label::new(
                egui::RichText::new(self.assistant.state().label())
                    .size(10.0)
                    .color(state_color(self.assistant.state(), palette)),
            )
            .wrap_mode(egui::TextWrapMode::Wrap),
        );
        ui.add_space(6.0);

        if self.capabilities.contains(Capability::AiAssistant) {
            self.assistant_conversation_ui(ui, palette);
        } else if self.assistant.shows_connect() {
            self.assistant_connection_body_ui(ui, palette);
        } else {
            self.assistant_connect_card_ui(ui, palette);
        }
    }

    /// The disconnected card: one honest sentence and one real connect action.
    fn assistant_connect_card_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        ui.label(
            egui::RichText::new(
                "Ask about the open document through the OpenCode service on this PC. Nothing is \
                 sent until you ask.",
            )
            .color(palette.text_muted),
        );
        ui.add_space(6.0);
        if ui.button("Connect to OpenCode").clicked() {
            self.dispatch(AppCommand::AssistantConnect, Some(ui.ctx()));
        }
        if let Some(endpoint) = self.assistant.last_endpoint() {
            ui.add_space(4.0);
            ui.label(
                egui::RichText::new(format!("Last connected: {endpoint}"))
                    .size(10.0)
                    .color(palette.text_muted),
            );
        }
        ui.add_space(8.0);
        ui.label(
            egui::RichText::new(
                "The assistant uses the providers already configured in OpenCode. PDFXI ships no \
                 model and no provider key.",
            )
            .size(9.5)
            .color(palette.text_muted),
        );
    }

    /// The connected conversation: model, messages, citations, ask and stop.
    fn assistant_conversation_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let model = self.assistant.selected_model().to_owned();
        ui.label(
            egui::RichText::new(format!("Model: {model}"))
                .size(10.5)
                .color(palette.accent),
        );
        ui.add_space(4.0);
        egui::CollapsingHeader::new("Connection and model")
            .id_salt("assistant_connection_section")
            .show(ui, |ui| self.assistant_connection_body_ui(ui, palette));
        ui.add_space(6.0);

        let thinking = self.assistant_chat.is_thinking();
        let page_count = self.state.active_tab().map_or(0, |tab| tab.page_count());
        // The composer is pinned to the bottom of the panel and sized from the
        // widgets it actually draws, so the growing message list can never push
        // the input or the action row over the side panel's storage meter.
        let messages_top = ui.available_rect_before_wrap().min.y;
        let composer = egui::TopBottomPanel::bottom("assistant_composer")
            .frame(egui::Frame::new())
            .show_separator_line(false)
            .show_inside(ui, |ui| {
                if thinking {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label(
                            egui::RichText::new("Thinking...")
                                .size(10.5)
                                .color(palette.text_muted),
                        );
                    });
                }
                ui.add_space(6.0);
                ui.label(
                    egui::RichText::new(
                        "Only your question is sent; this build sends no document text.",
                    )
                    .size(9.0)
                    .color(palette.text_muted),
                );
                let focus = self.assistant_chat.take_focus_input();
                let mut input = self.assistant_chat.take_input();
                let response = ui.add(
                    egui::TextEdit::singleline(&mut input)
                        .hint_text("Ask about this document")
                        .desired_width(f32::INFINITY),
                );
                self.assistant_chat.restore_input(input);
                if focus {
                    response.request_focus();
                }
                let submit =
                    response.has_focus() && ui.input(|input| input.key_pressed(egui::Key::Enter));
                ui.horizontal(|ui| {
                    let can_ask = !self.assistant_chat.input().trim().is_empty() && !thinking;
                    if ui.add_enabled(can_ask, egui::Button::new("Ask")).clicked()
                        || (submit && can_ask)
                    {
                        self.dispatch(AppCommand::AssistantAsk, Some(ui.ctx()));
                    }
                    // The only Stop control; the thinking row above is status only.
                    if thinking && ui.button("Stop").clicked() {
                        self.dispatch(AppCommand::AssistantStop, Some(ui.ctx()));
                    }
                });
            });

        // Exactly the gap the composer left. A positive minimum here would let
        // the list grow back over the composer and the storage meter below it.
        let available = (composer.response.rect.min.y - messages_top).max(0.0);
        // The message list is borrowed for this scroll area only, not cloned:
        // nothing inside it mutates `self`. A clicked citation is recorded and
        // dispatched after the closure, so drawing a frame copies no answer text.
        let mut cited_page: Option<u32> = None;
        {
            let messages = self.assistant_chat.messages();
            let error = self.assistant_chat.error();
            egui::ScrollArea::vertical()
                .id_salt("assistant_messages")
                .max_height(available)
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    if messages.is_empty() {
                        ui.label(
                            egui::RichText::new("Ask a question about the open document.")
                                .color(palette.text_muted),
                        );
                    }
                    for message in messages {
                        if message.is_question() {
                            ui.label(
                                egui::RichText::new("You")
                                    .strong()
                                    .size(10.0)
                                    .color(palette.text_muted),
                            );
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(message.text()).color(palette.text),
                                )
                                .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                        } else {
                            ui.label(
                                egui::RichText::new("OpenCode")
                                    .strong()
                                    .size(10.0)
                                    .color(palette.accent),
                            );
                            ui.add(
                                egui::Label::new(
                                    egui::RichText::new(message.text()).color(palette.text),
                                )
                                .wrap_mode(egui::TextWrapMode::Wrap),
                            );
                            if !message.citations().is_empty() {
                                ui.horizontal_wrapped(|ui| {
                                    for citation in message.citations() {
                                        let target = citation.page as usize;
                                        let clickable = target >= 1 && target <= page_count;
                                        let button = egui::Button::new(format!("Page {target}"))
                                            .small()
                                            .fill(palette.accent_soft);
                                        if ui.add_enabled(clickable, button).clicked() {
                                            cited_page = Some(citation.page);
                                        }
                                    }
                                });
                            }
                        }
                        ui.add_space(8.0);
                    }
                    if let Some(error) = error {
                        ui.label(
                            egui::RichText::new(error)
                                .size(10.0)
                                .color(egui::Color32::from_rgb(200, 80, 70)),
                        );
                    }
                });
        }
        if let Some(page) = cited_page {
            self.dispatch(AppCommand::AssistantCitation(page), Some(ui.ctx()));
        }
    }

    /// Explicit launch/deep-link action: open the assistant, connect, then ask.
    ///
    /// Only the `--assistant-question=` capture path calls this; opening the
    /// panel normally never sends a request.
    pub(crate) fn request_launch_question(&mut self, question: String) {
        self.active_rail = super::capabilities::RailItem::AiAssistant;
        self.pending_assistant_question = Some(question);
        self.begin_assistant_connect();
    }

    /// Send the typed question; no-op without a usable service or a model label.
    pub(crate) fn begin_assistant_ask(&mut self) -> bool {
        if !self.assistant.is_usable() {
            self.status = "Connect to OpenCode before asking".to_owned();
            return false;
        }
        let Some(endpoint) = self.assistant.endpoint().cloned() else {
            self.status = "Connect to OpenCode before asking".to_owned();
            return false;
        };
        let label = self.assistant.selected_model().to_owned();
        let Some(model) = parse_model_label(&label) else {
            self.status = format!("Choose an assistant model in the connection screen ({label})");
            return false;
        };
        let document = self.state.active_tab_id();
        if self.assistant_chat.begin(endpoint, model, document) {
            self.status = "Asking OpenCode".to_owned();
            true
        } else {
            false
        }
    }

    /// Cancel the running turn.
    ///
    /// Reports the outcome either way, so the command always has visible
    /// feedback and a headless dispatch proof can observe it.
    pub(crate) fn stop_assistant_turn(&mut self) {
        self.status = if self.assistant_chat.stop() {
            "Assistant turn stopped".to_owned()
        } else {
            "No assistant turn to stop".to_owned()
        };
    }

    /// Navigate the active document to a one-based citation page.
    ///
    /// The citation button and the tests share this mapping, so the UI cannot
    /// drift from the checked one-based-to-zero-based conversion.
    pub(crate) fn follow_assistant_citation(&mut self, page: u32) {
        if page >= 1 {
            self.dispatch(AppCommand::GoToPage(page as usize - 1), None);
        }
    }

    /// Drain the turn, reset on a document change and keep repainting while busy.
    pub(crate) fn poll_assistant_chat(&mut self, ctx: &egui::Context) {
        self.assistant_chat
            .sync_document(self.state.active_tab_id());
        if self.assistant_chat.poll() {
            self.status = if self.assistant_chat.error().is_some() {
                "The assistant turn failed".to_owned()
            } else {
                "Assistant answered".to_owned()
            };
        }
        if self.assistant_chat.is_thinking() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        if let Some(question) = self.pending_assistant_question.take() {
            if self.assistant.is_usable() {
                self.assistant_chat.set_input(question);
                self.begin_assistant_ask();
            } else if matches!(
                self.assistant.state(),
                super::assistant_connect::ConnectionState::Unreachable { .. }
                    | super::assistant_connect::ConnectionState::Incompatible { .. }
            ) {
                self.status = "The assistant question was not sent: no usable service".to_owned();
            } else {
                // The explicit connect action is still running.
                self.pending_assistant_question = Some(question);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pages_of(text: &str) -> Vec<u32> {
        parse_citations(text)
            .into_iter()
            .map(|citation| citation.page)
            .collect()
    }

    #[test]
    fn citations_parse_the_supported_forms_and_ignore_false_positives() {
        let text = "See page 2 and [p. 3]; page 2 again. P. 12, [page 7], page: 9.";
        let pages: Vec<u32> = parse_citations(text)
            .into_iter()
            .map(|citation| citation.page)
            .collect();
        assert_eq!(pages, vec![2, 3, 12, 7, 9]);

        // Bounds: page 1 is the smallest citation and the parser accepts the
        // cap itself but rejects one past it.
        assert_eq!(pages_of("page 1"), vec![1]);
        assert_eq!(pages_of("page 1000000"), vec![1_000_000]);
        assert!(
            parse_citations("page 1000001").is_empty(),
            "a page number past the cap is not a citation"
        );

        for ignored in [
            "homepage 2",
            "page2",
            "p2",
            // A bare `p` is not a keyword, so this needs its period. Without
            // that rule "Appendix P 2" would cite page 2.
            "p 2",
            "Appendix P 2",
            "pages 4",
            "page 0",
            "page -1",
            "the p. of the page",
        ] {
            assert!(
                parse_citations(ignored).is_empty(),
                "{ignored:?} must not cite a page"
            );
        }
        assert!(parse_citations("The answer cites nothing.").is_empty());
    }

    #[test]
    fn citations_accept_unicode_context_without_splitting_characters() {
        let pages: Vec<u32> = parse_citations("第 2 页 page 3 — página 4, p. 5")
            .into_iter()
            .map(|citation| citation.page)
            .collect();
        assert_eq!(pages, vec![3, 5]);
    }

    #[test]
    fn model_labels_split_into_provider_and_id() {
        let model = parse_model_label("opencode-go/hy4-flash-free").expect("model");
        assert_eq!(model.provider_id, "opencode-go");
        assert_eq!(model.id, "hy4-flash-free");
        assert!(parse_model_label("no-slash").is_none());
        assert!(parse_model_label("/missing-provider").is_none());
        assert!(parse_model_label("provider/").is_none());
        assert!(parse_model_label("a/b/c").is_none());
    }

    #[test]
    fn question_messages_never_invent_citations() {
        let message = ChatMessage::question("What is on page 2?");
        assert!(message.is_question());
        assert!(message.citations().is_empty());
    }

    use std::sync::atomic::{AtomicBool, Ordering};

    use crate::ui::shell::assistant_stub::{INFO, PASSWORD, Stub};

    /// Connect a fresh app to the stub through the real explicit-endpoint probe.
    fn connect(app: &mut PdfxiApp, stub: &Stub) {
        app.assistant
            .set_pair_inputs(stub.url.clone(), PASSWORD.to_owned());
        app.start_assistant_pair();
        let context = egui::Context::default();
        for _ in 0..800 {
            app.poll_assistant_connection(&context);
            if app.assistant.is_usable() {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(1));
        }
        panic!("the stub connection did not become usable");
    }

    fn connected_app(stub: &Stub) -> PdfxiApp {
        let mut app = PdfxiApp::new();
        connect(&mut app, stub);
        app
    }

    /// Poll the chat until an answer or an error replaces the question.
    fn wait_for_answer(app: &mut PdfxiApp) {
        let context = egui::Context::default();
        for _ in 0..1_500 {
            app.poll_assistant_chat(&context);
            if app.assistant_chat.error().is_some() {
                panic!(
                    "the assistant turn failed: {:?}",
                    app.assistant_chat.error()
                );
            }
            if app
                .assistant_chat
                .messages()
                .last()
                .is_some_and(|message| !message.is_question())
            {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("the assistant turn did not finish");
    }

    #[test]
    fn opening_the_panel_sends_nothing() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        stub.clear_requests();

        app.dispatch(AppCommand::AiAssistant, None);
        let context = egui::Context::default();
        for _ in 0..20 {
            app.poll_assistant_connection(&context);
            app.poll_assistant_chat(&context);
        }

        assert!(
            stub.requests().is_empty(),
            "opening the assistant panel must not send a request"
        );
        assert!(app.assistant_chat.messages().is_empty());
        assert!(!app.assistant_chat.is_thinking());
        assert_eq!(app.assistant_chat.session(), None);
    }

    #[test]
    fn ask_sends_only_after_the_click_and_never_document_text() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        app.dispatch(AppCommand::AiAssistant, None);
        stub.clear_requests();
        app.assistant_chat.set_input("What is on page 2?");

        assert!(stub.requests().is_empty(), "typing must send nothing");
        app.begin_assistant_ask();
        assert!(app.assistant_chat.is_thinking());

        wait_for_answer(&mut app);
        let answer = app
            .assistant_chat
            .messages()
            .last()
            .expect("an answer message");
        assert!(!answer.is_question());
        assert_eq!(
            answer
                .citations()
                .iter()
                .map(|citation| citation.page)
                .collect::<Vec<_>>(),
            vec![2]
        );

        let requests = stub.requests();
        let paths: Vec<&str> = requests
            .iter()
            .map(|request| request.path.as_str())
            .collect();
        assert_eq!(requests[0].method, "POST");
        assert_eq!(paths[0], "/api/session");
        assert!(
            paths
                .iter()
                .any(|path| path.starts_with("/api/session/ses_test1/prompt")),
            "the click must send the prompt: {paths:?}"
        );
        assert!(
            paths
                .iter()
                .any(|path| path.starts_with("/api/session/ses_test1/message")),
            "the answer is polled from the message route: {paths:?}"
        );
        let prompt = requests
            .iter()
            .find(|request| request.path.ends_with("/prompt"))
            .expect("a prompt request");
        assert_eq!(
            prompt.body, r#"{"text":"What is on page 2?"}"#,
            "Round A sends the typed question and no document text"
        );
        assert!(!prompt.body.contains("test.pdf"));
        assert!(!prompt.body.contains("The relevant text"));
    }

    #[test]
    fn ask_uses_the_model_remembered_in_settings() {
        let stub = Stub::start(INFO);
        let mut app = PdfxiApp::new();
        app.settings.assistant_model = "opencode-go/deepseek-v4.1-flash".to_owned();
        app.assistant
            .set_saved_model(app.settings.assistant_model.clone());
        connect(&mut app, &stub);
        assert_eq!(
            app.assistant.selected_model(),
            "opencode-go/deepseek-v4.1-flash"
        );

        app.assistant_chat.set_input("Summarize page 2.");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);

        let create = stub
            .requests()
            .into_iter()
            .find(|request| request.path == "/api/session")
            .expect("a session create");
        assert!(create.body.contains(r#""providerID":"opencode-go""#));
        assert!(create.body.contains(r#""id":"deepseek-v4.1-flash""#));
    }

    struct SlowRunner {
        cancelled: Arc<AtomicBool>,
    }

    impl TurnRunner for SlowRunner {
        fn run(
            &self,
            _endpoint: &ServiceEndpoint,
            _request: TurnRequest,
            cancel: &CancelFlag,
        ) -> Result<TurnOutcome, AssistantError> {
            for _ in 0..2_000 {
                if cancel.is_cancelled() {
                    self.cancelled.store(true, Ordering::SeqCst);
                    return Err(AssistantError::Cancelled);
                }
                std::thread::sleep(std::time::Duration::from_millis(1));
            }
            Err(AssistantError::Cancelled)
        }
    }

    #[test]
    fn stop_cancels_a_slow_turn_and_returns_to_idle() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        let cancelled = Arc::new(AtomicBool::new(false));
        app.assistant_chat.set_runner(Arc::new(SlowRunner {
            cancelled: Arc::clone(&cancelled),
        }));

        app.assistant_chat.set_input("Take your time.");
        app.begin_assistant_ask();
        assert!(app.assistant_chat.is_thinking());
        assert_eq!(app.assistant_chat.messages().len(), 1);

        app.stop_assistant_turn();
        assert!(!app.assistant_chat.is_thinking());
        assert!(app.assistant_chat.error().is_none());
        assert_eq!(
            app.assistant_chat.messages().len(),
            1,
            "a stopped turn leaves the question and no invented answer"
        );

        for _ in 0..1_000 {
            if cancelled.load(Ordering::SeqCst) {
                return;
            }
            std::thread::sleep(std::time::Duration::from_millis(2));
        }
        panic!("Stop did not cancel the running turn");
    }

    #[test]
    fn citation_navigation_changes_the_active_page() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        let id = app.state.open_tab("three-pages.pdf");
        app.finish_new_tab(id);
        if let Some(tab) = app.state.tab_mut(id) {
            tab.set_page_count(3);
        }
        app.thumbs.reset(3);
        app.assistant_chat.set_input("What is on page 2?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);
        assert_eq!(app.state.active_tab().expect("tab").current_page(), 0);

        let page = app
            .assistant_chat
            .messages()
            .last()
            .and_then(|message| message.citations().first())
            .map(|citation| citation.page)
            .expect("a citation");
        app.dispatch(AppCommand::AssistantCitation(page), None);

        assert_eq!(
            AppCommand::AssistantCitation(page).label(),
            "assistant-citation"
        );
        assert_eq!(page, 2);
        assert_eq!(
            app.state.active_tab().expect("tab").current_page(),
            1,
            "a one-based page 2 citation navigates to zero-based page 1"
        );
    }

    #[test]
    fn switching_or_opening_a_document_resets_the_conversation() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        let first = app.state.open_tab("first.pdf");
        app.finish_new_tab(first);
        let second = app.state.open_tab("second.pdf");
        app.finish_new_tab(second);

        app.assistant_chat.set_input("What is on page 2?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);
        assert_eq!(app.assistant_chat.messages().len(), 2);
        assert_eq!(app.assistant_chat.session(), Some("ses_test1"));

        app.activate_tab(first);
        assert!(
            app.assistant_chat.messages().is_empty(),
            "a tab switch clears the previous document's conversation"
        );
        assert_eq!(app.assistant_chat.session(), None);

        app.activate_tab(second);
        assert!(
            app.assistant_chat.messages().is_empty(),
            "returning to a tab does not restore an old conversation"
        );

        let third = app.state.open_tab("third.pdf");
        app.finish_new_tab(third);
        assert!(app.assistant_chat.messages().is_empty());
    }
}
