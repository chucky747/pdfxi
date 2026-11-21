//! Per-document assistant conversation, ask/stop flow and page citations
//! (T-0253).
//!
//! The panel is the app half of ADR 0011. It owns nothing about HTTP: one turn
//! is handed to a [`TurnRunner`], which by default calls the T-0251 client's
//! locked-session and `prompt`/`ask` routes on a worker thread. The UI thread
//! only drains a channel and repaints, so a slow or cancelled turn never blocks
//! a frame.
//!
//! A turn is grounded by sending a bounded, fenced excerpt of the document in
//! the prompt (T-0253 fallback decision). The client-MCP route cannot expose
//! document tools to the model in OpenCode 2.3.2, so the app does not attach a
//! tool server at all:
//!
//! 1. [`PdfxiApp::begin_assistant_ask`] captures
//!    [`super::assistant_tools::DocumentSnapshot`] on the UI thread and turns it
//!    into `excerpt + "\n\nQuestion: " + question`;
//! 2. the first turn creates the session with
//!    [`pdfxi_assistant::Assistant::create_locked_session`], which checks the
//!    service version, sends the deny-`*` ruleset and verifies the read-back;
//! 3. only then is `/prompt` sent, with no tool available to the model.
//!
//! The locked session is the security boundary: the excerpt is untrusted data
//! and the session has no shell, file or network tool. Nothing is sent before
//! the user presses Ask; opening the panel only draws state.
//!
//! The snapshot is captured on the UI thread per turn. A document change resets
//! the conversation and cancels the turn.

use std::sync::Arc;
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;

use pdfxi_assistant::{
    Assistant, AssistantError, CancelFlag, LockedSession, ModelRef, ServiceEndpoint, SessionRequest,
};

use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};

use super::assistant_connect::state_color;
use super::assistant_tools::MAX_EXCERPT_CHARS;
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
#[derive(Clone)]
pub(crate) struct TurnRequest {
    /// Verified locked session for this conversation, when one was created.
    pub(crate) session: Option<LockedSession>,
    /// Model the existing session was last set to.
    pub(crate) session_model: Option<ModelRef>,
    /// Model selected in the connection screen.
    pub(crate) model: ModelRef,
    /// The bounded document excerpt followed by `Question: <question>`.
    pub(crate) prompt: String,
}

/// A completed turn.
#[derive(Debug, Clone)]
pub(crate) struct TurnOutcome {
    /// Verified locked session the answer belongs to.
    pub(crate) session: LockedSession,
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

/// The shipped runner: one locked session per conversation, then `ask`.
///
/// The session is created with [`Assistant::create_locked_session`] (version
/// gate, deny-all ruleset, read-back verification) and no tool server is
/// attached, so the model can only read the excerpt in the prompt.
#[derive(Default)]
pub(crate) struct ServiceTurnRunner;

impl TurnRunner for ServiceTurnRunner {
    fn run(
        &self,
        endpoint: &ServiceEndpoint,
        request: TurnRequest,
        cancel: &CancelFlag,
    ) -> Result<TurnOutcome, AssistantError> {
        let assistant = Assistant::connect(endpoint.clone());
        let locked = match request.session.clone() {
            Some(locked) => {
                if request.session_model.as_ref() != Some(&request.model) {
                    assistant.set_model(locked.id(), &request.model, cancel)?;
                }
                locked
            }
            None => {
                let create = SessionRequest::new(
                    "PDFXI assistant",
                    request.model.clone(),
                    std::env::temp_dir(),
                )?;
                // Version gate, deny-all ruleset and read-back verification all
                // happen inside this call; a failure returns before any prompt.
                assistant.create_locked_session(&create, cancel)?
            }
        };
        let answer = assistant.ask(locked.id(), &request.prompt, cancel);
        Ok(TurnOutcome {
            session: locked,
            model: request.model,
            answer: answer?.text,
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
    locked: Option<LockedSession>,
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
            locked: None,
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
        self.locked = None;
        self.session_model = None;
        self.error = None;
        self.focus_input = false;
    }

    /// Send the typed question on a worker thread.
    ///
    /// Returns `false` for an empty question or a turn already in flight, so the
    /// Ask control can stay disabled instead of sending a duplicate. `prompt` is
    /// the complete turn text (the fenced document excerpt followed by
    /// `Question: <question>`); the panel message keeps only the typed question.
    pub(crate) fn begin(
        &mut self,
        endpoint: ServiceEndpoint,
        model: ModelRef,
        document: Option<crate::TabId>,
        prompt: String,
    ) -> bool {
        let question = self.input.trim().to_owned();
        if question.is_empty() || self.is_thinking() {
            return false;
        }
        self.sync_document(document);
        self.input.clear();
        self.error = None;
        self.focus_input = false;
        self.messages.push(ChatMessage::question(question));
        let request = TurnRequest {
            session: self.locked.clone(),
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
                let (session, model) = (outcome.session, outcome.model);
                job.cancel.cancel();
                self.job = None;
                self.locked = Some(session);
                self.session_model = Some(model);
                self.messages.push(ChatMessage::answer(outcome.answer));
                self.state = ChatState::Idle;
                self.focus_input = true;
                true
            }
            Ok(Err(error)) => {
                job.cancel.cancel();
                self.job = None;
                self.state = ChatState::Idle;
                if !error.is_cancelled() {
                    self.error = Some(error.to_string());
                }
                true
            }
            Err(TryRecvError::Empty) => false,
            Err(TryRecvError::Disconnected) => {
                job.cancel.cancel();
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
        self.locked.as_ref().map(LockedSession::id)
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
        // Presets are document-grounded: they stay disabled until there is a
        // document to read, a usable service to answer and no turn in flight.
        let presets_enabled =
            self.state.active_tab_id().is_some() && self.assistant.is_usable() && !thinking;
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
                        "When you ask, your question and a bounded excerpt of this document's \
                         text are sent to the local OpenCode service. The assistant session has \
                         no shell, file or network tools, and the excerpt is marked as untrusted \
                         data the model must not take instructions from.",
                    )
                    .size(9.0)
                    .color(palette.text_muted),
                );
                ui.horizontal(|ui| {
                    if ui
                        .add_enabled(presets_enabled, egui::Button::new("Summarize").small())
                        .clicked()
                    {
                        self.dispatch(AppCommand::AssistantSummarize, Some(ui.ctx()));
                    }
                    if ui
                        .add_enabled(presets_enabled, egui::Button::new("Key insights").small())
                        .clicked()
                    {
                        self.dispatch(AppCommand::AssistantKeyInsights, Some(ui.ctx()));
                    }
                });
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
    ///
    /// The prompt is `excerpt + "\n\nQuestion: <question>"`, built only here, so
    /// nothing about the document leaves the app before the user presses Ask.
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
        // Capture on the UI thread, then fence the excerpt for the prompt. The
        // session itself receives no tools; the excerpt is untrusted data.
        let question = self.assistant_chat.input().trim().to_owned();
        let snapshot = self.assistant_document_snapshot();
        let prompt = format!(
            "{}\n\nQuestion: {question}",
            snapshot.prompt_excerpt(MAX_EXCERPT_CHARS)
        );
        if self.assistant_chat.begin(endpoint, model, document, prompt) {
            self.status = "Asking OpenCode".to_owned();
            true
        } else {
            false
        }
    }

    /// Fill the ask box with a grounded preset and send it through the ask path.
    ///
    /// Both presets are about the open document, so they stay inert without one
    /// instead of sending an empty excerpt. Everything else — the service gate,
    /// the locked session, the fenced excerpt — is the normal
    /// [`Self::begin_assistant_ask`] path; this helper only supplies the text and
    /// the document guard.
    pub(crate) fn begin_assistant_preset(&mut self, question: &str) -> bool {
        if self.state.active_tab_id().is_none() {
            self.status = "Open a document before using an assistant preset".to_owned();
            return false;
        }
        self.assistant_chat.set_input(question);
        self.begin_assistant_ask()
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

    use pdfxi_document::DocumentSession;

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

    /// Open the real two-page fixture in `app` under a real session.
    fn open_fixture(app: &mut PdfxiApp) {
        let path = std::path::PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("../../fixtures/realworld/microsoft-excel-2019-sample.pdf");
        let id = app.state.open_tab("microsoft-excel-2019-sample.pdf");
        app.sessions
            .insert(id, DocumentSession::open(&path).expect("the fixture opens"));
        app.finish_new_tab(id);
    }

    /// Poll the chat until an answer or an error replaces the question.
    fn wait_for_answer(app: &mut PdfxiApp) {
        let context = egui::Context::default();
        for _ in 0..2_000 {
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

    fn request_paths(stub: &Stub) -> Vec<String> {
        stub.requests()
            .into_iter()
            .map(|request| request.path)
            .collect()
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
    fn ask_sends_the_excerpt_and_question_only_after_the_click() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        open_fixture(&mut app);
        app.dispatch(AppCommand::AiAssistant, None);
        stub.clear_requests();
        app.assistant_chat
            .set_input("What is the verification sentence on page 1?");

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
        // The locked path checks the service version before it creates a
        // session, then reads the ruleset back before any prompt is sent.
        assert_eq!(paths[0], "/api/info", "paths: {paths:?}");
        assert!(
            paths.contains(&"/api/session"),
            "the conversation must create a session: {paths:?}"
        );
        assert!(
            requests
                .iter()
                .any(|request| request.method == "GET" && request.path == "/api/session/ses_test1"),
            "the ruleset must be read back: {paths:?}"
        );
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
        assert!(
            !paths.iter().any(|path| path.contains("mcp")),
            "no tool server is attached any more: {paths:?}"
        );

        let prompt = requests
            .iter()
            .find(|request| request.path.ends_with("/prompt"))
            .expect("a prompt request");
        assert!(
            prompt.body.contains("amber compass lantern"),
            "the prompt must carry the real page 1 text: {:?}",
            prompt.body
        );
        assert_eq!(
            prompt.body.matches("<untrusted-document-text").count(),
            1,
            "one open fence: {:?}",
            prompt.body
        );
        assert_eq!(
            prompt.body.matches("</untrusted-document-text>").count(),
            1,
            "one close fence: {:?}",
            prompt.body
        );
        assert!(
            prompt
                .body
                .contains("Question: What is the verification sentence on page 1?"),
            "the typed question follows the excerpt: {:?}",
            prompt.body
        );
        assert!(prompt.body.contains("--- page 1 ---"));
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

    #[test]
    fn the_session_is_created_locked_with_the_selected_model() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        app.assistant_chat.set_input("What is on page 2?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);

        let create = stub
            .requests()
            .into_iter()
            .find(|request| request.method == "POST" && request.path == "/api/session")
            .expect("a session create");
        let body: serde_json::Value =
            serde_json::from_str(&create.body).expect("the create body is JSON");

        let permissions = body["permissions"]
            .as_array()
            .expect("the create body carries a ruleset");
        assert_eq!(permissions.len(), 6);
        assert_eq!(
            permissions[0],
            serde_json::json!({"action": "*", "resource": "*", "effect": "deny"}),
            "the wildcard deny must come first"
        );
        let allowed: Vec<&str> = permissions[1..]
            .iter()
            .map(|rule| rule["action"].as_str().expect("an action"))
            .collect();
        assert_eq!(
            allowed,
            vec![
                "pdfxi_document_info",
                "pdfxi_page_text",
                "pdfxi_search",
                "pdfxi_selection",
                "pdfxi_outline"
            ]
        );
        assert!(
            permissions[1..]
                .iter()
                .all(|rule| rule["resource"] == "*" && rule["effect"] == "allow")
        );
        assert_eq!(body["model"]["providerID"], "opencode-go");
        assert_eq!(body["model"]["id"], "hy4-flash-free");
    }

    #[test]
    fn ask_without_a_document_sends_an_honest_empty_excerpt() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        app.assistant_chat.set_input("What does this say?");

        assert!(app.begin_assistant_ask());
        wait_for_answer(&mut app);

        let prompt = stub
            .requests()
            .into_iter()
            .find(|request| request.path.ends_with("/prompt"))
            .expect("a prompt request");
        assert!(prompt.body.contains("No document is open."));
        assert!(prompt.body.contains("[No document is open.]"));
        assert!(prompt.body.contains("Question: What does this say?"));
        assert_eq!(prompt.body.matches("</untrusted-document-text>").count(), 1);
    }

    #[test]
    fn a_second_ask_reuses_the_locked_session() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);

        app.assistant_chat.set_input("What is on page 2?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);
        let first_session = app.assistant_chat.session().map(str::to_owned);

        app.assistant_chat.set_input("And page 3?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);

        assert_eq!(app.assistant_chat.session(), first_session.as_deref());
        let paths = request_paths(&stub);
        assert_eq!(
            paths.iter().filter(|path| *path == "/api/session").count(),
            1,
            "a second ask must not create another session: {paths:?}"
        );
        assert_eq!(
            paths
                .iter()
                .filter(|path| path.ends_with("/prompt"))
                .count(),
            2,
            "both questions reach the session: {paths:?}"
        );
    }

    #[test]
    fn a_model_change_is_posted_before_the_prompt() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);

        app.assistant_chat.set_input("What is on page 2?");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);
        assert_eq!(app.assistant_chat.session(), Some("ses_test1"));

        assert!(
            app.assistant
                .select_model("opencode-go/deepseek-v4.1-flash"),
            "the model list must offer the second model"
        );
        stub.clear_requests();
        app.assistant_chat.set_input("Summarize page 1.");
        app.begin_assistant_ask();
        wait_for_answer(&mut app);

        let requests = stub.requests();
        let model_index = requests
            .iter()
            .position(|request| {
                request.method == "POST" && request.path == "/api/session/ses_test1/model"
            })
            .expect("a model switch");
        let prompt_index = requests
            .iter()
            .position(|request| request.path.ends_with("/prompt"))
            .expect("a prompt");
        assert!(
            model_index < prompt_index,
            "the model switch must precede the prompt: {requests:?}"
        );
        assert!(
            requests[model_index].body.contains("deepseek-v4.1-flash"),
            "the model body was {:?}",
            requests[model_index].body
        );
        assert_eq!(
            requests
                .iter()
                .filter(|request| request.path == "/api/session")
                .count(),
            0,
            "a model change must not create a new session"
        );
    }

    #[test]
    fn summarize_preset_goes_through_the_normal_ask_path() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        open_fixture(&mut app);
        app.dispatch(AppCommand::AiAssistant, None);
        stub.clear_requests();

        app.dispatch(AppCommand::AssistantSummarize, None);

        assert_eq!(
            AppCommand::AssistantSummarize.label(),
            "assistant-summarize"
        );
        assert!(app.assistant_chat.is_thinking());
        let asked = app.assistant_chat.messages().first().expect("a question");
        assert!(asked.is_question());
        assert_eq!(asked.text(), "Summarize this document in one sentence.");

        wait_for_answer(&mut app);
        let prompt = stub
            .requests()
            .into_iter()
            .find(|request| request.path.ends_with("/prompt"))
            .expect("a prompt request");
        assert!(
            prompt
                .body
                .contains("Question: Summarize this document in one sentence."),
            "the preset question must reach the prompt: {:?}",
            prompt.body
        );
        assert!(
            prompt.body.contains("amber compass lantern"),
            "the preset must still carry the grounded excerpt: {:?}",
            prompt.body
        );
    }

    #[test]
    fn key_insights_preset_goes_through_the_normal_ask_path() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        open_fixture(&mut app);
        app.dispatch(AppCommand::AiAssistant, None);
        stub.clear_requests();

        app.dispatch(AppCommand::AssistantKeyInsights, None);

        assert_eq!(
            AppCommand::AssistantKeyInsights.label(),
            "assistant-key-insights"
        );
        assert!(app.assistant_chat.is_thinking());
        let asked = app.assistant_chat.messages().first().expect("a question");
        assert!(asked.is_question());
        assert_eq!(
            asked.text(),
            "List the key insights in this document as short bullets."
        );

        wait_for_answer(&mut app);
        let prompt = stub
            .requests()
            .into_iter()
            .find(|request| request.path.ends_with("/prompt"))
            .expect("a prompt request");
        assert!(
            prompt
                .body
                .contains("Question: List the key insights in this document as short bullets."),
            "the preset question must reach the prompt: {:?}",
            prompt.body
        );
        assert!(
            prompt.body.contains("amber compass lantern"),
            "the preset must still carry the grounded excerpt: {:?}",
            prompt.body
        );
    }

    #[test]
    fn a_preset_without_a_document_does_not_send() {
        let stub = Stub::start(INFO);
        let mut app = connected_app(&stub);
        app.dispatch(AppCommand::AiAssistant, None);
        stub.clear_requests();

        app.dispatch(AppCommand::AssistantSummarize, None);
        app.dispatch(AppCommand::AssistantKeyInsights, None);

        assert!(!app.assistant_chat.is_thinking());
        assert!(
            app.assistant_chat.messages().is_empty(),
            "a preset with no document must not enter the conversation"
        );
        assert!(
            stub.requests().is_empty(),
            "a preset with no document must not send anything"
        );
    }
}
