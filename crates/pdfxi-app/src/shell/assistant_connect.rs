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
    let assistant = Assistant::connect_with(endpoint, config);
    let info = assistant.check_compatible(cancel).map_err(classify)?;
    let providers = assistant.providers(cancel).map_err(classify)?;
    let models = assistant.models(cancel).map_err(classify)?;
    Ok(ProbeOutcome {
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
    auto_probe_done: bool,
    persist: Option<PersistRequest>,
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
            auto_probe_done: false,
            persist: None,
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
    }

    /// Whether a probe is queued or running.
    pub(crate) fn is_checking(&self) -> bool {
        self.job.is_some() || matches!(self.state, ConnectionState::Checking)
    }

    /// Ask the next poll to start a discovery probe.
    ///
    /// Dispatching a command stays free of I/O; the frame calls [`Self::poll`]
    /// and the worker thread does the network work.
    pub(crate) fn request_probe(&mut self) {
        self.probe_requested = true;
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
    pub(crate) fn disconnect(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel();
        }
        self.state = ConnectionState::Disconnected;
        self.service_url.clear();
        self.provider_labels.clear();
        self.pair_error = None;
        self.persist = None;
        // A manual disconnect must not immediately reconnect on the next frame.
        self.auto_probe_done = true;
    }

    /// Returns the latest settings write, once.
    pub(crate) fn take_persist(&mut self) -> Option<PersistRequest> {
        self.persist.take()
    }

    /// Probe the service once when the panel is first opened.
    pub(crate) fn ensure_auto_probe(&mut self) {
        if self.auto_probe_done {
            return;
        }
        self.auto_probe_done = true;
        if matches!(self.state, ConnectionState::Disconnected) {
            self.request_probe();
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
    /// usable service, so no assistant control is drawn before there is one. The
    /// connection screen itself stays available through
    /// [`Capability::AssistantConnect`].
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

    /// Open the connection screen and queue a probe.
    ///
    /// Called by [`AppCommand::AiAssistant`] from the Tools row, the command bar
    /// and the `--tool=ai` deep link; dispatch itself performs no I/O.
    pub(crate) fn open_assistant_connect(&mut self) {
        self.active_rail = super::capabilities::RailItem::AiAssistant;
        self.assistant.request_probe();
        self.status = "Connect to OpenCode to use the AI assistant".to_owned();
    }

    /// Draw the connection screen inside the left side panel.
    pub(crate) fn assistant_connect_panel_ui(&mut self, ui: &mut egui::Ui, palette: Palette) {
        self.assistant.ensure_auto_probe();

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
                self.dispatch(AppCommand::AiAssistant, Some(ui.ctx()));
            }
            if matches!(&state, ConnectionState::Connected(_)) && ui.button("Disconnect").clicked()
            {
                self.disconnect_assistant();
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
                self.start_assistant_pair();
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
                self.select_assistant_model(label);
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
fn state_color(state: &ConnectionState, palette: Palette) -> egui::Color32 {
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
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::{Arc, Mutex};
    use std::time::Duration;

    use pdfxi_assistant::{Timeouts, redact};

    use crate::settings::{self, Settings};

    const PASSWORD: &str = "test-secret";
    const EXPECTED_AUTH: &str = "Basic b3BlbmNvZGU6dGVzdC1zZWNyZXQ=";

    const INFO: &str = r#"{"version":"2.3.2","pid":17164,"hostname":"test",
        "urls":["http://127.0.0.1:1"],"capabilities":{"persistentPty":true}}"#;
    const INFO_NEWER: &str = r#"{"version":"2.4.0","pid":17164,"hostname":"test",
        "urls":["http://127.0.0.1:1"]}"#;
    const PROVIDERS: &str = r#"{"data":[
        {"id":"opencode-go","name":"OpenCode Go","enabled":true},
        {"id":"opencode","name":"OpenCode","enabled":true},
        {"id":"claude-agent","name":"Claude Agent","enabled":false}]}"#;
    const MODELS: &str = r#"{"data":[
        {"id":"hy4-flash-free","modelID":"hy4-flash-free","providerID":"opencode-go",
         "type":"chat","name":"Hy4 flash Free","status":"active","enabled":true,
         "variants":[{"id":"none"},{"id":"high"}],
         "capabilities":{"tools":true,"input":["text"],"output":["text"]},
         "limit":{"context":1048576}},
        {"id":"deepseek-v4.1-flash","providerID":"opencode-go","type":"chat",
         "name":"DeepSeek V4.1 Flash","enabled":true,
         "limit":{"context":262144}}]}"#;

    #[derive(Debug, Clone, Default)]
    struct Recorded {
        method: String,
        path: String,
        authorization: Option<String>,
        body_len: usize,
    }

    /// An in-process loopback stub for the three metadata routes.
    struct Stub {
        url: String,
        address: std::net::SocketAddr,
        requests: Arc<Mutex<Vec<Recorded>>>,
        stop: Arc<AtomicBool>,
        handle: Option<JoinHandle<()>>,
    }

    impl Stub {
        fn start(info: &'static str) -> Self {
            // A blocking listener: Windows is sensitive to a non-blocking
            // listener's flag leaking into accepted sockets and resetting the
            // connection before the client reads the response.
            let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback stub");
            let address = listener.local_addr().expect("stub address");
            let url = format!("http://{address}");
            let requests = Arc::new(Mutex::new(Vec::new()));
            let recorded = Arc::clone(&requests);
            let stop = Arc::new(AtomicBool::new(false));
            let worker_stop = Arc::clone(&stop);
            let handle = std::thread::spawn(move || {
                while !worker_stop.load(Ordering::SeqCst) {
                    let Ok((mut stream, _)) = listener.accept() else {
                        break;
                    };
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let _ = stream.set_nonblocking(false);
                    let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                    // Stay on this connection until the client closes it. The
                    // response advertises `Connection: close`, so a conforming
                    // client opens a new socket per request, but a keep-alive
                    // client is handled by the same loop.
                    while let Some((head, body)) = read_request(&mut stream) {
                        let request = parse_head(&head, body.len());
                        let authorized = request.authorization.as_deref() == Some(EXPECTED_AUTH);
                        let payload = if !authorized {
                            ""
                        } else if request.path == "/api/info" {
                            info
                        } else if request.path == "/api/provider" {
                            PROVIDERS
                        } else if request.path == "/api/model" {
                            MODELS
                        } else {
                            ""
                        };
                        let status = if authorized {
                            "200 OK"
                        } else {
                            "401 Unauthorized"
                        };
                        recorded.lock().expect("record requests").push(request);
                        let response = format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        );
                        if stream.write_all(response.as_bytes()).is_err() {
                            break;
                        }
                        let _ = stream.flush();
                        if worker_stop.load(Ordering::SeqCst) {
                            break;
                        }
                    }
                }
            });
            Self {
                url,
                address,
                requests,
                stop,
                handle: Some(handle),
            }
        }

        fn requests(&self) -> Vec<Recorded> {
            self.requests.lock().expect("requests").clone()
        }
    }

    impl Drop for Stub {
        fn drop(&mut self) {
            self.stop.store(true, Ordering::SeqCst);
            // Wake the blocking `accept`; the worker sees the stop flag and exits.
            if let Ok(wake) = TcpStream::connect(self.address) {
                drop(wake);
            }
            if let Some(handle) = self.handle.take() {
                let _ = handle.join();
            }
        }
    }

    /// Reads one complete request: all headers and any `Content-Length` body.
    fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
        let mut bytes = Vec::new();
        let mut chunk = [0u8; 1024];
        let header_end = loop {
            if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
                break position + 4;
            }
            let read = read_chunk(stream, &mut chunk)?;
            if read == 0 {
                return None;
            }
            bytes.extend_from_slice(&chunk[..read]);
            if bytes.len() > 64 * 1024 {
                return None;
            }
        };
        let head = String::from_utf8(bytes[..header_end].to_vec()).ok()?;
        let content_length = head.lines().find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        });
        let mut body = bytes[header_end..].to_vec();
        let expected = content_length.unwrap_or(0);
        while body.len() < expected {
            let read = read_chunk(stream, &mut chunk)?;
            if read == 0 {
                return None;
            }
            body.extend_from_slice(&chunk[..read]);
            if body.len() > 1024 * 1024 {
                return None;
            }
        }
        body.truncate(expected);
        Some((head, body))
    }

    /// Reads one chunk, retrying a transient non-blocking result.
    fn read_chunk(stream: &mut TcpStream, chunk: &mut [u8]) -> Option<usize> {
        loop {
            match stream.read(chunk) {
                Ok(read) => return Some(read),
                Err(error)
                    if matches!(
                        error.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                    ) =>
                {
                    std::thread::sleep(Duration::from_millis(1));
                }
                Err(_) => return None,
            }
        }
    }

    fn parse_head(head: &str, body_len: usize) -> Recorded {
        let mut lines = head.lines();
        let request_line = lines.next().unwrap_or_default();
        let mut parts = request_line.split_whitespace();
        let method = parts.next().unwrap_or_default().to_owned();
        let path = parts.next().unwrap_or_default().to_owned();
        let authorization = lines.find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("authorization")
                .then(|| value.trim().to_owned())
        });
        Recorded {
            method,
            path,
            authorization,
            body_len,
        }
    }

    fn test_config() -> ClientConfig {
        ClientConfig {
            timeouts: Timeouts {
                connect: Duration::from_secs(2),
                response: Duration::from_secs(5),
                read: Duration::from_secs(5),
                body: Duration::from_secs(5),
            },
            ..ClientConfig::default()
        }
    }

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
            assert_eq!(request.body_len, 0, "the probe sends bodiless GETs");
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
    fn dispatch_opens_the_connect_panel_without_network_io() {
        let mut app = PdfxiApp::new();
        assert_eq!(
            app.active_rail(),
            super::super::capabilities::RailItem::Home
        );
        assert!(
            app.capabilities.contains(Capability::AssistantConnect),
            "the connection screen is reachable before a service exists"
        );
        assert!(
            !app.capabilities.contains(Capability::AiAssistant),
            "the conversation capability stays off until a usable service exists"
        );

        app.dispatch(AppCommand::AiAssistant, None);

        assert_eq!(
            app.active_rail(),
            super::super::capabilities::RailItem::AiAssistant
        );
        assert!(app.status().contains("OpenCode"));
        assert!(matches!(
            app.assistant.state(),
            ConnectionState::Disconnected
        ));
        assert_eq!(
            app.assistant.selected_model(),
            DEFAULT_ASSISTANT_MODEL,
            "the picker starts on the acceptance model"
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
