//! The OpenCode connection screen and its read-only connection probe (T-0252).
//!
//! The screen is the app half of ADR 0011: it discovers the user's local
//! OpenCode service through the T-0251 [`ServiceEndpoint`] helper, checks the
//! service version, lists providers and models, and remembers the chosen model.
//! Every HTTP route lives in [`pdfxi_assistant`]; this module only starts the
//! work on a worker thread and renders the result.
//!
//! What the probe sends: `GET /api/info`, `GET /api/provider` and
//! `GET /api/model`. It never creates a session, never prompts, and never sends
//! document text. The service password stays inside the T-0251 `Secret` and is
//! never rendered, logged, stored in application state, or written to settings.

use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::thread::JoinHandle;
use std::time::Duration;

use pdfxi_assistant::{
    Assistant, AssistantError, CancelFlag, ClientConfig, Model, Provider, ServiceEndpoint,
};

use crate::design::Palette;
use crate::ui::{AppCommand, PdfxiApp};

use super::capabilities::Capability;

/// Acceptance model and the picker default from ADR 0011.
pub(crate) const DEFAULT_ASSISTANT_MODEL: &str = "opencode-go/hy4-flash-free";

/// What the connection screen currently knows about the service.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum ConnectionState {
    /// No connection has been attempted, or the user disconnected.
    Disconnected,
    /// A probe is running on a worker thread.
    Checking,
    /// The service answered every metadata call.
    Connected(ConnectedService),
    /// The service answered, but outside the pinned `2.3.x` range.
    Incompatible {
        /// Version reported by `GET /api/info`.
        version: String,
    },
    /// The service could not be reached, or rejected the metadata calls.
    Unreachable {
        /// Safe, redacted error text from the T-0251 client.
        message: String,
    },
}

impl ConnectionState {
    /// A short label for the panel header.
    pub(crate) fn label(&self) -> String {
        match self {
            Self::Disconnected => "Not connected".to_owned(),
            Self::Checking => "Checking the service".to_owned(),
            Self::Connected(service) => format!(
                "Connected - OpenCode {} - {} models",
                service.version,
                service.models.len()
            ),
            Self::Incompatible { version } => format!("Incompatible service {version}"),
            Self::Unreachable { .. } => "Service unreachable".to_owned(),
        }
    }
}

/// The parts of a usable service the panel shows.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct ConnectedService {
    /// Base URL of the service, without credentials.
    pub(crate) url: String,
    /// `/api/info` version.
    pub(crate) version: String,
    /// Enabled provider labels, in service order.
    pub(crate) provider_labels: Vec<String>,
    /// Models from `/api/model`.
    pub(crate) models: Vec<Model>,
}

impl ConnectedService {
    /// Number of models the service reports.
    pub(crate) fn model_count(&self) -> usize {
        self.models.len()
    }
}

/// Result of a successful metadata probe.
#[derive(Debug, Clone)]
pub(crate) struct ProbeOutcome {
    /// Endpoint that answered, including the in-memory credential.
    ///
    /// The credential is never formatted, rendered, logged, or persisted; the
    /// conversation needs it to open a session without asking for the pairing
    /// code again.
    pub(crate) endpoint: ServiceEndpoint,
    /// Base URL of the service, without credentials.
    pub(crate) url: String,
    /// `/api/info` payload.
    pub(crate) info: pdfxi_assistant::ServiceInfo,
    /// `/api/provider` entries.
    pub(crate) providers: Vec<Provider>,
    /// `/api/model` entries.
    pub(crate) models: Vec<Model>,
}

/// Why a probe did not produce a connected service.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ProbeFailure {
    /// The service version is outside the pinned `2.3.x` range.
    Incompatible {
        /// Version reported by the service.
        version: String,
    },
    /// Discovery, transport, authentication or a response shape failed.
    Unreachable {
        /// Safe, redacted error text.
        message: String,
    },
}

/// A target the user selected on the connection screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ConnectTarget {
    /// Discover the local service registration on loopback.
    Discover,
    /// Connect to a user-supplied address with a pairing code as credential.
    ///
    /// The T-0251 client has no OpenCode pairing handshake (`POST /api/pair`,
    /// `GET /auth/connect/{code}`), so this uses its documented explicit
    /// endpoint path and the code as the service credential. The panel states
    /// that limit instead of claiming a handshake that does not exist.
    Explicit {
        /// Service base URL.
        url: String,
        /// Pairing code used as the explicit endpoint credential.
        code: String,
    },
}

/// Calls only the T-0251 metadata routes against an endpoint.
///
/// `/api/info` is checked for `2.3.x` compatibility first, so an incompatible
/// service is refused with a typed [`ProbeFailure::Incompatible`] before the
/// provider or model routes are called. No session or prompt route is used.
pub(crate) fn probe_endpoint(
    endpoint: ServiceEndpoint,
    cancel: &CancelFlag,
) -> Result<ProbeOutcome, ProbeFailure> {
    probe_endpoint_with(endpoint, ClientConfig::default(), cancel)
}

/// Like [`probe_endpoint`] with explicit client timings, for tests.
pub(crate) fn probe_endpoint_with(
    endpoint: ServiceEndpoint,
    config: ClientConfig,
    cancel: &CancelFlag,
) -> Result<ProbeOutcome, ProbeFailure> {
    let assistant = Assistant::connect_with(endpoint.clone(), config);
    let info = assistant.check_compatible(cancel).map_err(classify)?;
    let providers = assistant.providers(cancel).map_err(classify)?;
    let models = assistant.models(cancel).map_err(classify)?;
    Ok(ProbeOutcome {
        endpoint,
        url: assistant.endpoint().url().to_owned(),
        info,
        providers,
        models,
    })
}

/// Splits T-0251 errors into the two states the screen renders.
fn classify(error: AssistantError) -> ProbeFailure {
    match error {
        AssistantError::IncompatibleVersion { version } => ProbeFailure::Incompatible { version },
        other => ProbeFailure::Unreachable {
            // `AssistantError` is non-exhaustive and its Display is redacted and
            // capped by the T-0251 crate; it never contains the password.
            message: other.to_string(),
        },
    }
}

/// One running probe and the channel its result arrives on.
struct ConnectionJob {
    receiver: Receiver<Result<ProbeOutcome, ProbeFailure>>,
    cancel: CancelFlag,
    _handle: JoinHandle<()>,
}

impl ConnectionJob {
    /// Asks the worker transport to stop at its next cancellation point.
    fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Starts a probe on a worker thread so no UI frame blocks on I/O.
fn start_probe(target: ConnectTarget) -> ConnectionJob {
    let (sender, receiver) = mpsc::channel();
    let cancel = CancelFlag::new();
    let worker_cancel = cancel.clone();
    let handle = std::thread::spawn(move || {
        let outcome =
            resolve_target(&target).and_then(|endpoint| probe_endpoint(endpoint, &worker_cancel));
        let _ = sender.send(outcome);
    });
    ConnectionJob {
        receiver,
        cancel,
        _handle: handle,
    }
}

/// Resolves a [`ConnectTarget`] without sending anything.
fn resolve_target(target: &ConnectTarget) -> Result<ServiceEndpoint, ProbeFailure> {
    let endpoint = match target {
        ConnectTarget::Discover => ServiceEndpoint::discover(),
        ConnectTarget::Explicit { url, code } => ServiceEndpoint::explicit(url, code.clone()),
    };
    endpoint.map_err(|error| ProbeFailure::Unreachable {
        message: error.to_string(),
    })
}

/// A settings write requested by a successful probe or a model change.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PersistRequest {
    /// Endpoint that answered, without credentials.
    pub(crate) endpoint: String,
    /// Selected `provider/model` label.
    pub(crate) model: String,
}

/// Window-free state of the connection screen.
pub(crate) struct AssistantConnection {
    state: ConnectionState,
    service_url: String,
    provider_labels: Vec<String>,
    selected_model: String,
    saved_model: String,
    last_endpoint: Option<String>,
    pair_url: String,
    pair_code: String,
    pair_error: Option<String>,
    job: Option<ConnectionJob>,
    probe_requested: bool,
    persist: Option<PersistRequest>,
    /// Endpoint retained in memory for the conversation; cleared on disconnect.
    endpoint: Option<ServiceEndpoint>,
    /// Whether the full Connect screen is showing instead of the short card.
    show_connect: bool,
}

impl Default for AssistantConnection {
    fn default() -> Self {
        Self {
            state: ConnectionState::Disconnected,
            service_url: String::new(),
            provider_labels: Vec::new(),
            selected_model: DEFAULT_ASSISTANT_MODEL.to_owned(),
            saved_model: DEFAULT_ASSISTANT_MODEL.to_owned(),
            last_endpoint: None,
            pair_url: String::new(),
            pair_code: String::new(),
            pair_error: None,
            job: None,
            probe_requested: false,
            persist: None,
            endpoint: None,
            show_connect: false,
        }
    }
}

impl AssistantConnection {
    /// The current connection state.
    pub(crate) fn state(&self) -> &ConnectionState {
        &self.state
    }

    /// The selected `provider/model` label.
    pub(crate) fn selected_model(&self) -> &str {
        &self.selected_model
    }

    /// The last endpoint that answered, without credentials.
    pub(crate) fn last_endpoint(&self) -> Option<&str> {
        self.last_endpoint.as_deref()
    }

    /// The connected endpoint, including the in-memory credential.
    ///
    /// Only the conversation worker reads this; it is never rendered.
    pub(crate) fn endpoint(&self) -> Option<&ServiceEndpoint> {
        self.endpoint.as_ref()
    }

    /// Whether the full Connect screen is showing on the assistant panel.
    pub(crate) fn shows_connect(&self) -> bool {
        self.show_connect
    }

    /// Seeds the picker from persisted settings.
    pub(crate) fn set_saved_model(&mut self, model: String) {
        if !model.trim().is_empty() {
            self.saved_model = model;
            if self.selected_model.is_empty() {
                self.selected_model = self.saved_model.clone();
            }
        }
    }

    /// Seeds the displayed last endpoint from persisted settings.
    pub(crate) fn set_last_endpoint(&mut self, endpoint: Option<String>) {
        self.last_endpoint = endpoint.filter(|value| !value.trim().is_empty());
    }

    /// Whether a connection is usable for assistant controls.
    ///
    /// A service with no models cannot answer a question, so it does not count
    /// as usable even though the metadata calls succeeded.
    pub(crate) fn is_usable(&self) -> bool {
        matches!(&self.state, ConnectionState::Connected(service)
            if !service.models.is_empty())
            && !self.selected_model.is_empty()
            && self.endpoint.is_some()
    }

    /// Whether a probe is queued or running.
    pub(crate) fn is_checking(&self) -> bool {
        self.job.is_some() || matches!(self.state, ConnectionState::Checking)
    }

    /// Ask the next poll to start a discovery probe.
    ///
    /// Dispatching a command stays free of I/O; the frame calls [`Self::poll`]
    /// and the worker thread does the network work. A probe that is already
    /// queued or running makes this a no-op and returns `false`, so two rapid
    /// Connect clicks cannot queue a second probe behind the first one.
    pub(crate) fn request_probe(&mut self) -> bool {
        if self.probe_requested || self.is_checking() {
            return false;
        }
        self.probe_requested = true;
        true
    }

    /// Records the explicit pairing form values. The code stays in memory only.
    #[cfg(test)]
    pub(crate) fn set_pair_inputs(&mut self, url: String, code: String) {
        self.pair_url = url;
        self.pair_code = code;
    }
    /// Starts an explicit-endpoint probe with the pairing code as credential.
    ///
    /// The code is moved into the worker request and cleared from the form, so
    /// it is never rendered after the attempt starts.
    pub(crate) fn begin_explicit(&mut self) {
        let url = self.pair_url.trim().to_owned();
        let code = std::mem::take(&mut self.pair_code);
        if url.is_empty() || code.is_empty() {
            self.pair_error = Some("Enter the service address and the pairing code.".to_owned());
            return;
        }
        self.pair_error = None;
        self.state = ConnectionState::Checking;
        self.service_url = url.clone();
        self.provider_labels.clear();
        // An explicit connection replaces any discovery probe that was only
        // queued and not yet started.
        self.probe_requested = false;
        self.job = Some(start_probe(ConnectTarget::Explicit { url, code }));
    }

    /// Selects a model reported by the connected service.
    pub(crate) fn select_model(&mut self, label: &str) -> bool {
        let ConnectionState::Connected(service) = &self.state else {
            return false;
        };
        if !service
            .models
            .iter()
            .any(|model| model.label() == label && model.is_enabled())
        {
            return false;
        }
        label.clone_into(&mut self.selected_model);
        label.clone_into(&mut self.saved_model);
        true
    }

    /// Returns to the disconnected state and abandons any running probe.
    ///
    /// The pairing code is the credential in this build, so it is erased from
    /// memory and from the visible field here: after Disconnect there is no
    /// service left that the code could be replayed against. The queued-probe
    /// flag is cleared too, so a manual disconnect cannot reconnect on the next
    /// frame.
    pub(crate) fn disconnect(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.state = ConnectionState::Disconnected;
        self.service_url.clear();
        self.provider_labels.clear();
        self.pair_code.clear();
        self.pair_error = None;
        self.probe_requested = false;
        self.persist = None;
        self.endpoint = None;
        self.show_connect = true;
    }

    /// Returns the latest settings write, once.
    pub(crate) fn take_persist(&mut self) -> Option<PersistRequest> {
        self.persist.take()
    }

    /// Open the full Connect screen and queue one discovery probe.
    ///
    /// This is the "Connect to OpenCode" action; opening the assistant panel by
    /// itself never calls it, so opening the panel sends no request. A probe
    /// that is already queued or running is not duplicated.
    pub(crate) fn begin_connect(&mut self) {
        self.show_connect = true;
        let _ = self.request_probe();
    }

    /// Show the short assistant card instead of the full Connect screen.
    pub(crate) fn show_assistant_card(&mut self) {
        if !self.is_usable() {
            self.show_connect = false;
        }
    }

    /// Advances the state machine without blocking.
    ///
    /// Starts a requested probe, then drains one worker result. A pending probe
    /// leaves the state in [`ConnectionState::Checking`].
    pub(crate) fn poll(&mut self) {
        if self.probe_requested && self.job.is_none() {
            self.probe_requested = false;
            self.begin_discovery();
        }

        let Some(job) = self.job.as_ref() else {
            return;
        };
        match job.receiver.try_recv() {
            Ok(Ok(outcome)) => {
                self.job = None;
                self.apply_outcome(outcome);
            }
            Ok(Err(failure)) => {
                self.job = None;
                self.apply_failure(failure);
            }
            Err(TryRecvError::Empty) => {}
            Err(TryRecvError::Disconnected) => {
                self.job = None;
                self.state = ConnectionState::Unreachable {
                    message: "the connection check ended unexpectedly".to_owned(),
                };
            }
        }
    }

    /// Starts a discovery probe and shows the checking state.
    fn begin_discovery(&mut self) {
        self.state = ConnectionState::Checking;
        self.service_url = "Local OpenCode service".to_owned();
        self.provider_labels.clear();
        self.pair_error = None;
        self.job = Some(start_probe(ConnectTarget::Discover));
    }

    /// Records a successful probe and selects a model.
    fn apply_outcome(&mut self, outcome: ProbeOutcome) {
        let provider_labels: Vec<String> = outcome
            .providers
            .iter()
            .filter(|provider| provider.is_enabled())
            .map(|provider| provider.label().to_owned())
            .collect();
        let selected = choose_model(&outcome.models, &self.saved_model);
        self.selected_model = selected.clone();
        self.service_url = outcome.url.clone();
        self.provider_labels = provider_labels.clone();
        self.last_endpoint = Some(outcome.url.clone());
        self.endpoint = Some(outcome.endpoint);
        self.persist = Some(PersistRequest {
            endpoint: outcome.url.clone(),
            model: selected,
        });
        self.state = ConnectionState::Connected(ConnectedService {
            url: outcome.url,
            version: outcome.info.version,
            provider_labels,
            models: outcome.models,
        });
    }

    /// Records a failed probe without losing the last error detail.
    fn apply_failure(&mut self, failure: ProbeFailure) {
        self.provider_labels.clear();
        self.service_url.clear();
        self.endpoint = None;
        self.state = match failure {
            ProbeFailure::Incompatible { version } => ConnectionState::Incompatible { version },
            ProbeFailure::Unreachable { message } => {
                self.pair_error = Some(message.clone());
                ConnectionState::Unreachable { message }
            }
        };
    }
}

/// Picks the remembered model, then the acceptance model, then an enabled chat
/// model, then any enabled model, then the first reported model.
pub(crate) fn choose_model(models: &[Model], preferred: &str) -> String {
    if let Some(model) = models
        .iter()
        .find(|model| model.label() == preferred && model.is_enabled())
    {
        return model.label();
    }
    if let Some(model) = models
        .iter()
        .find(|model| model.label() == DEFAULT_ASSISTANT_MODEL)
    {
        return model.label();
    }
    models
        .iter()
        .find(|model| model.is_enabled() && model.kind.as_deref() == Some("chat"))
        .or_else(|| models.iter().find(|model| model.is_enabled()))
        .or_else(|| models.first())
        .map(Model::label)
        .unwrap_or_default()
}

impl PdfxiApp {
    /// Poll the worker and keep [`Capability::AiAssistant`] honest.
    ///
    /// The conversation capability exists only while the connection screen has a
    /// usable service. The assistant panel reads it to choose the conversation
    /// over the connection screen, so no assistant conversation control is drawn
    /// before there is a service to talk to. The connection screen itself stays
    /// available through [`Capability::AssistantConnect`]. A frame polls before
    /// it draws, so the flag is never a frame behind the panel.
    pub(crate) fn poll_assistant_connection(&mut self, ctx: &egui::Context) {
        self.assistant.poll();
        if let Some(persist) = self.assistant.take_persist() {
            self.settings.assistant_model = persist.model;
            self.settings.assistant_endpoint = Some(persist.endpoint);
            self.persist_settings();
        }
        self.capabilities = if self.assistant.is_usable() {
            self.capabilities.with(Capability::AiAssistant)
        } else {
            self.capabilities.without(Capability::AiAssistant)
        };
        if self.assistant.is_checking() {
            ctx.request_repaint_after(Duration::from_millis(100));
        }
    }

    /// Change the assistant model and remember it in the settings file.
    pub(crate) fn select_assistant_model(&mut self, label: String) {
        if self.assistant.select_model(&label) {
            self.settings.assistant_model = label.clone();
            self.persist_settings();
            self.status = format!("Assistant model: {label}");
        }
    }

    /// Start the explicit "pair with a code" probe from the panel fields.
    pub(crate) fn start_assistant_pair(&mut self) {
        self.assistant.begin_explicit();
        self.status = "Checking the OpenCode service".to_owned();
    }

    /// Return the Connect screen to the disconnected state.
    pub(crate) fn disconnect_assistant(&mut self) {
        self.assistant.disconnect();
        self.status = "Disconnected from OpenCode".to_owned();
    }

    /// Open the assistant panel without sending anything.
    ///
    /// Called by [`AppCommand::AiAssistant`] from the Tools row, the command bar,
    /// the rail and the `--tool=ai` deep link. The panel shows a "Connect to
    /// OpenCode" card when no service is connected; only the explicit connect
    /// action starts the metadata probe.
    pub(crate) fn open_assistant_connect(&mut self) {
        self.active_rail = super::capabilities::RailItem::AiAssistant;
        self.assistant.show_assistant_card();
        self.status = "AI Assistant".to_owned();
    }

    /// Start the explicit connection probe from the assistant panel.
    pub(crate) fn begin_assistant_connect(&mut self) {
        self.active_rail = super::capabilities::RailItem::AiAssistant;
        self.assistant.begin_connect();
        self.status = "Connect to OpenCode to use the AI assistant".to_owned();
    }

    /// Draw the connection states, model picker and pairing fields without a
    /// header, so the conversation can show them in its Connection section.
    pub(crate) fn assistant_connection_body_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        let state = self.assistant.state().clone();
        match &state {
            ConnectionState::Disconnected => {
                ui.label(
                    egui::RichText::new(
                        "Connect to the OpenCode service running on this PC. The check reads \
                         the service version, providers and models only; it sends no document \
                         text.",
                    )
                    .color(palette.text_muted),
                );
                if let Some(endpoint) = self.assistant.last_endpoint() {
                    ui.add_space(4.0);
                    ui.label(
                        egui::RichText::new(format!("Last connected: {endpoint}"))
                            .size(10.0)
                            .color(palette.text_muted),
                    );
                }
            }
            ConnectionState::Checking => {
                ui.horizontal(|ui| {
                    ui.spinner();
                    ui.label(
                        egui::RichText::new("Contacting the OpenCode service...")
                            .color(palette.text_muted),
                    );
                });
            }
            ConnectionState::Connected(service) => {
                ui.label(
                    egui::RichText::new(format!("Service: {}", service.url))
                        .size(10.5)
                        .color(palette.text_muted),
                );
                ui.label(
                    egui::RichText::new(format!(
                        "OpenCode {} | {} | {} models",
                        service.version,
                        provider_summary(&service.provider_labels),
                        service.model_count()
                    ))
                    .size(10.5)
                    .color(palette.text_muted),
                );
            }
            ConnectionState::Incompatible { version } => {
                ui.label(
                    egui::RichText::new(format!(
                        "OpenCode {version} is outside the pinned 2.3.x range. The assistant \
                         refuses this service instead of sending requests it cannot verify."
                    ))
                    .color(palette.text_muted),
                );
            }
            ConnectionState::Unreachable { message } => {
                ui.label(
                    egui::RichText::new(message)
                        .size(10.5)
                        .color(palette.text_muted),
                );
            }
        }

        ui.add_space(8.0);
        ui.horizontal(|ui| {
            let connect_label = match &state {
                ConnectionState::Connected(_) => "Reconnect",
                ConnectionState::Incompatible { .. } | ConnectionState::Unreachable { .. } => {
                    "Try again"
                }
                ConnectionState::Checking => "Checking...",
                ConnectionState::Disconnected => "Connect to OpenCode",
            };
            let enabled = !matches!(&state, ConnectionState::Checking);
            if ui
                .add_enabled(enabled, egui::Button::new(connect_label))
                .clicked()
            {
                self.dispatch(AppCommand::AssistantConnect, Some(ui.ctx()));
            }
            if matches!(&state, ConnectionState::Connected(_)) && ui.button("Disconnect").clicked()
            {
                self.dispatch(AppCommand::AssistantDisconnect, Some(ui.ctx()));
            }
        });

        // Pairing path: an explicit endpoint with the code as credential. The
        // T-0251 client has no `POST /api/pair` handshake, so this is labelled
        // as an explicit connection rather than pretending to pair.
        if !matches!(&state, ConnectionState::Connected(_)) {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("Pair with a code")
                    .size(11.0)
                    .strong()
                    .color(palette.text),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.assistant.pair_url)
                    .hint_text("Service address")
                    .desired_width(f32::INFINITY),
            );
            ui.add(
                egui::TextEdit::singleline(&mut self.assistant.pair_code)
                    .hint_text("Pairing code")
                    .password(true)
                    .desired_width(f32::INFINITY),
            );
            if ui.button("Pair and check").clicked() {
                self.dispatch(AppCommand::AssistantPair, Some(ui.ctx()));
            }
            if let Some(error) = &self.assistant.pair_error {
                ui.label(
                    egui::RichText::new(error)
                        .size(10.0)
                        .color(egui::Color32::from_rgb(200, 80, 70)),
                );
            }
            ui.label(
                egui::RichText::new(
                    "Explicit T-0251 connection: the service has no pairing handshake in \
                     this client yet, so the code is used as the endpoint credential.",
                )
                .size(9.0)
                .color(palette.text_muted),
            );
        }

        if let ConnectionState::Connected(service) = &state {
            ui.add_space(10.0);
            ui.label(
                egui::RichText::new("Model")
                    .size(11.0)
                    .strong()
                    .color(palette.text),
            );
            ui.label(
                egui::RichText::new(format!("Default: {DEFAULT_ASSISTANT_MODEL}"))
                    .size(9.0)
                    .color(palette.text_muted),
            );
            ui.label(
                egui::RichText::new(format!("Selected: {}", self.assistant.selected_model()))
                    .size(10.5)
                    .color(palette.accent),
            );
            ui.add_space(4.0);
            let current = self.assistant.selected_model().to_owned();
            let mut chosen: Option<String> = None;
            egui::ScrollArea::vertical()
                .max_height(160.0)
                .show(ui, |ui| {
                    for model in service.models.iter().filter(|model| model.is_enabled()) {
                        let label = model.label();
                        let selected = label == current;
                        let name = model.name.as_deref().unwrap_or("");
                        let text = if name.is_empty() {
                            label.clone()
                        } else {
                            format!("{label} - {name}")
                        };
                        if ui.selectable_label(selected, text).clicked() {
                            chosen = Some(label);
                        }
                    }
                });
            if let Some(label) = chosen {
                self.dispatch(AppCommand::AssistantSelectModel(label), Some(ui.ctx()));
            }
        }

        ui.add_space(10.0);
        ui.label(
            egui::RichText::new("This screen sends no document text.")
                .size(9.5)
                .color(palette.text_muted),
        );
    }
}

/// Status colour for the panel header.
pub(crate) fn state_color(state: &ConnectionState, palette: Palette) -> egui::Color32 {
    match state {
        ConnectionState::Connected(_) => palette.accent,
        ConnectionState::Incompatible { .. } | ConnectionState::Unreachable { .. } => {
            egui::Color32::from_rgb(200, 80, 70)
        }
        ConnectionState::Disconnected | ConnectionState::Checking => palette.text_muted,
    }
}

/// A short provider summary for the status line.
fn provider_summary(labels: &[String]) -> String {
    match labels {
        [] => "no providers".to_owned(),
        [only] => only.clone(),
        [first, second] => format!("{first}, {second}"),
        [first, ..] => format!("{first} +{}", labels.len() - 1),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::net::TcpListener;
    use std::time::Duration;

    use pdfxi_assistant::redact;

    use crate::settings::{self, Settings};
    use crate::ui::AppCommand;
    use crate::ui::shell::assistant_stub::{
        EXPECTED_AUTH, INFO, INFO_NEWER, PASSWORD, Stub, test_config,
    };
    fn connected_to(stub: &Stub) -> AssistantConnection {
        let endpoint = ServiceEndpoint::explicit(&stub.url, PASSWORD).expect("endpoint");
        let outcome = probe_endpoint_with(endpoint, test_config(), &CancelFlag::new())
            .expect("probe succeeds");
        let mut connection = AssistantConnection::default();
        connection.apply_outcome(outcome);
        connection
    }

    #[test]
    fn connect_lists_models_and_selects_the_acceptance_default() {
        let stub = Stub::start(INFO);
        let connection = connected_to(&stub);

        let ConnectionState::Connected(service) = connection.state() else {
            panic!("expected a connected service, got {:?}", connection.state());
        };
        assert_eq!(service.version, "2.3.2");
        assert_eq!(service.provider_labels, vec!["OpenCode Go", "OpenCode"]);
        assert_eq!(service.model_count(), 2);
        assert_eq!(connection.selected_model(), DEFAULT_ASSISTANT_MODEL);
        assert!(connection.is_usable());

        let requests = stub.requests();
        assert_eq!(requests.len(), 3);
        let paths: Vec<&str> = requests
            .iter()
            .map(|request| request.path.as_str())
            .collect();
        assert_eq!(paths, vec!["/api/info", "/api/provider", "/api/model"]);
        for request in &requests {
            assert_eq!(request.method, "GET", "the probe sends metadata GETs only");
            assert!(request.body.is_empty(), "the probe sends bodiless GETs");
            assert_eq!(
                request.authorization.as_deref(),
                Some(EXPECTED_AUTH),
                "every probe request carries the Basic credential"
            );
        }
        assert!(
            !service.url.contains(PASSWORD),
            "the service URL must never carry the credential"
        );
    }

    #[test]
    fn unreachable_service_is_reported_and_not_usable() {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind free port");
        let address = listener.local_addr().expect("address");
        drop(listener);

        let endpoint =
            ServiceEndpoint::explicit(&format!("http://{address}"), PASSWORD).expect("endpoint");
        let failure = probe_endpoint_with(endpoint, test_config(), &CancelFlag::new())
            .expect_err("a closed port is unreachable");
        let ProbeFailure::Unreachable { message } = failure else {
            panic!("expected unreachable, got {failure:?}");
        };
        assert!(!message.contains(PASSWORD), "error text must be redacted");
        assert_eq!(redact(&message), message, "error text is log-safe");

        let mut connection = AssistantConnection::default();
        connection.apply_failure(ProbeFailure::Unreachable { message });
        assert!(matches!(
            connection.state(),
            ConnectionState::Unreachable { .. }
        ));
        assert!(!connection.is_usable());
    }

    #[test]
    fn incompatible_service_is_refused_before_models_are_requested() {
        let stub = Stub::start(INFO_NEWER);
        let endpoint = ServiceEndpoint::explicit(&stub.url, PASSWORD).expect("endpoint");
        let failure = probe_endpoint_with(endpoint, test_config(), &CancelFlag::new())
            .expect_err("2.4.0 is outside the pinned range");
        assert_eq!(
            failure,
            ProbeFailure::Incompatible {
                version: "2.4.0".to_owned()
            }
        );

        let requests = stub.requests();
        assert_eq!(
            requests.len(),
            1,
            "the version check must run before provider/model calls"
        );
        assert_eq!(requests[0].path, "/api/info");

        let mut connection = AssistantConnection::default();
        connection.apply_failure(failure);
        assert!(matches!(
            connection.state(),
            ConnectionState::Incompatible { version } if version == "2.4.0"
        ));
        assert!(!connection.is_usable());
    }

    #[test]
    fn dispatch_opens_the_assistant_panel_without_network_io() {
        let mut app = PdfxiApp::new();
        assert_eq!(
            app.active_rail(),
            super::super::capabilities::RailItem::Home
        );
        assert!(
            app.capabilities.contains(Capability::AssistantConnect),
            "the assistant panel is reachable before a service exists"
        );
        assert!(
            !app.capabilities.contains(Capability::AiAssistant),
            "the conversation capability stays off until a usable service exists"
        );
        assert!(!app.assistant.shows_connect());

        app.dispatch(AppCommand::AiAssistant, None);

        assert_eq!(
            app.active_rail(),
            super::super::capabilities::RailItem::AiAssistant
        );
        assert_eq!(app.status(), "AI Assistant");
        assert!(
            !app.assistant.shows_connect(),
            "opening the panel shows the card, not the Connect screen"
        );
        assert!(matches!(
            app.assistant.state(),
            ConnectionState::Disconnected
        ));
        assert_eq!(
            app.assistant.selected_model(),
            DEFAULT_ASSISTANT_MODEL,
            "the picker starts on the acceptance model"
        );

        app.begin_assistant_connect();
        assert!(
            app.assistant.shows_connect(),
            "the explicit connect action opens the Connect screen"
        );
    }

    #[test]
    fn model_choice_prefers_the_saved_model_then_the_acceptance_model() {
        let models = vec![
            serde_json::from_str::<Model>(
                r#"{"id":"first","providerID":"other","type":"chat","enabled":false}"#,
            )
            .expect("model"),
            serde_json::from_str::<Model>(
                r#"{"id":"second","providerID":"other","type":"chat","enabled":true}"#,
            )
            .expect("model"),
        ];
        assert_eq!(choose_model(&models, "other/second"), "other/second");
        assert_eq!(choose_model(&models, ""), "other/second");
        assert_eq!(choose_model(&[], "other/second"), "");
    }

    fn wait_for_probe(connection: &mut AssistantConnection) {
        for _ in 0..400 {
            connection.poll();
            if !connection.is_checking() {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("the probe did not finish within four seconds");
    }

    #[test]
    fn pair_with_code_uses_the_explicit_endpoint_and_clears_the_code() {
        let stub = Stub::start(INFO);
        let mut connection = AssistantConnection::default();
        connection.set_pair_inputs(stub.url.clone(), PASSWORD.to_owned());
        connection.begin_explicit();
        assert!(
            connection.pair_code.is_empty(),
            "the code must not stay in the form after the attempt starts"
        );
        wait_for_probe(&mut connection);

        let ConnectionState::Connected(service) = connection.state() else {
            panic!(
                "expected an explicit connection, got {:?}",
                connection.state()
            );
        };
        assert_eq!(service.version, "2.3.2");
        assert_eq!(connection.selected_model(), DEFAULT_ASSISTANT_MODEL);
        let requests = stub.requests();
        assert_eq!(requests.len(), 3);
        assert!(
            requests
                .iter()
                .all(|request| request.authorization.as_deref() == Some(EXPECTED_AUTH))
        );

        let persisted = connection.take_persist().expect("a persist request");
        assert_eq!(persisted.model, DEFAULT_ASSISTANT_MODEL);
        assert!(!persisted.endpoint.contains(PASSWORD));

        let mut empty = AssistantConnection::default();
        empty.set_pair_inputs(stub.url.clone(), String::new());
        empty.begin_explicit();
        assert!(empty.pair_error.is_some());
        assert!(matches!(empty.state(), ConnectionState::Disconnected));
        assert!(!empty.is_checking(), "an empty code must not start a probe");
    }

    #[test]
    fn disconnect_returns_to_disconnected_and_keeps_the_remembered_model() {
        let stub = Stub::start(INFO);
        let mut connection = connected_to(&stub);
        assert!(connection.select_model("opencode-go/deepseek-v4.1-flash"));
        assert_eq!(
            connection.selected_model(),
            "opencode-go/deepseek-v4.1-flash"
        );

        connection.disconnect();

        assert!(matches!(connection.state(), ConnectionState::Disconnected));
        assert!(!connection.is_usable());
        assert_eq!(
            connection.selected_model(),
            "opencode-go/deepseek-v4.1-flash",
            "the remembered model survives a disconnect"
        );
    }

    #[test]
    fn disconnect_clears_the_pairing_code_and_the_queued_probe() {
        let stub = Stub::start(INFO);
        let mut connection = connected_to(&stub);
        connection.set_pair_inputs(stub.url.clone(), PASSWORD.to_owned());
        assert_eq!(
            connection.pair_code, PASSWORD,
            "the form holds the typed code"
        );
        assert!(
            connection.request_probe(),
            "a connected screen may reconnect"
        );

        connection.disconnect();

        assert!(
            connection.pair_code.is_empty(),
            "the credential must not stay in memory after Disconnect"
        );
        assert!(
            !connection.probe_requested,
            "a manual disconnect must not leave a probe queued"
        );
        assert!(matches!(connection.state(), ConnectionState::Disconnected));
    }

    #[test]
    fn duplicate_probe_requests_start_only_one_probe() {
        let stub = Stub::start(INFO);
        let mut connection = AssistantConnection::default();
        connection.set_pair_inputs(stub.url.clone(), PASSWORD.to_owned());
        connection.begin_explicit();
        assert!(
            !connection.request_probe(),
            "a running probe refuses a duplicate request"
        );
        assert!(
            !connection.probe_requested,
            "the refused request queued nothing behind the running probe"
        );

        wait_for_probe(&mut connection);
        connection.poll();
        assert!(
            matches!(connection.state(), ConnectionState::Connected(_)),
            "the single probe connected: {:?}",
            connection.state()
        );
        assert_eq!(
            stub.requests().len(),
            3,
            "one probe sends the three metadata GETs, never six"
        );

        // The same guard coalesces two rapid Connect clicks before the first
        // frame: the second request is refused and the first stays queued.
        let mut queued = AssistantConnection::default();
        assert!(queued.request_probe());
        assert!(
            !queued.request_probe(),
            "a queued probe rejects a second request"
        );
        assert!(queued.probe_requested, "the first request is still queued");
    }

    #[test]
    fn a_usable_service_enables_the_conversation_capability() {
        let stub = Stub::start(INFO);
        let endpoint = ServiceEndpoint::explicit(&stub.url, PASSWORD).expect("endpoint");
        let outcome = probe_endpoint_with(endpoint, test_config(), &CancelFlag::new())
            .expect("probe succeeds");

        let mut app = PdfxiApp::new();
        let context = egui::Context::default();
        app.poll_assistant_connection(&context);
        assert!(
            !app.capabilities.contains(Capability::AiAssistant),
            "no service yet: the panel keeps showing the connection screen"
        );

        app.assistant.apply_outcome(outcome);
        app.poll_assistant_connection(&context);
        assert!(
            app.capabilities.contains(Capability::AiAssistant),
            "a usable service turns the conversation surface on"
        );

        app.assistant.disconnect();
        app.poll_assistant_connection(&context);
        assert!(
            !app.capabilities.contains(Capability::AiAssistant),
            "Disconnect turns the conversation surface off again"
        );
    }

    #[test]
    fn app_persists_the_model_and_endpoint_but_never_the_password() {
        let unique = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or(0);
        let dir = std::env::temp_dir().join(format!(
            "pdfxi-assistant-settings-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&dir).expect("create settings dir");
        let path = dir.join("settings.toml");

        let stub = Stub::start(INFO);
        let endpoint = ServiceEndpoint::explicit(&stub.url, PASSWORD).expect("endpoint");
        let outcome = probe_endpoint_with(endpoint, test_config(), &CancelFlag::new())
            .expect("probe succeeds");

        let mut app = PdfxiApp::new();
        app.settings_path = Some(path.clone());
        app.settings = Settings::default();
        app.assistant.apply_outcome(outcome);
        app.poll_assistant_connection(&egui::Context::default());

        let saved = settings::load(&path);
        assert_eq!(saved.assistant_model, DEFAULT_ASSISTANT_MODEL);
        let expected_endpoint = format!("{}/", stub.url);
        assert_eq!(
            saved.assistant_endpoint.as_deref(),
            Some(expected_endpoint.as_str())
        );

        app.select_assistant_model("opencode-go/deepseek-v4.1-flash".to_owned());
        let saved = settings::load(&path);
        assert_eq!(
            saved.assistant_model, "opencode-go/deepseek-v4.1-flash",
            "the picker change survives a settings round-trip"
        );

        let text = std::fs::read_to_string(&path).expect("read settings");
        let lower = text.to_ascii_lowercase();
        assert!(!lower.contains("password"));
        assert!(!lower.contains("test-secret"));
        assert!(!lower.contains("token"));

        let _ = std::fs::remove_dir_all(&dir);
    }
}
