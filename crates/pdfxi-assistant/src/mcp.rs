//! Client-MCP attach: PDFXI hosts a read-only MCP server on the service socket.
//!
//! The OpenCode service upgrades `GET /api/experimental/client-mcp?name=<name>`
//! to a WebSocket and speaks one JSON-RPC message per text frame. PDFXI answers
//! `initialize`, sends `notifications/initialized`, answers `tools/list` from a
//! [`ToolProvider`] and dispatches `tools/call`. The tool set is read-only
//! document data: no shell, file, edit or network tool is ever advertised.
//!
//! Protocol constants follow MCP revision `2025-11-25`, the version the service
//! sent in the T-0250 initialize frame.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tungstenite::stream::MaybeTlsStream;
use url::Url;

use crate::cancel::CancelFlag;
use crate::config::Timeouts;
use crate::error::{AssistantError, is_timeout, redact};

/// MCP revision this server answers with.
pub const MCP_PROTOCOL_VERSION: &str = "2025-11-25";

/// Server name advertised in `initialize`.
pub const MCP_SERVER_NAME: &str = "pdfxi";

/// Maximum characters returned by one tool call, so document text cannot be
/// used to exhaust the model context or PDFXI memory.
pub const MAX_TOOL_TEXT_CHARS: usize = 64 * 1024;

/// Read-only document tools exposed to the assistant.
///
/// Every method returns text content for the model. Implementations must not
/// read the filesystem, the network, the shell or the process environment:
/// document text is attacker-controlled input, and these tools are the only
/// capability the assistant session gets.
pub trait ToolProvider: Send + Sync {
    /// Document identity and page count, for example a JSON object or a short
    /// human-readable summary.
    fn document_info(&self) -> Result<String, String>;
    /// Text of one 1-based page.
    fn page_text(&self, page: u32) -> Result<String, String>;
    /// Full-document search.
    fn search(&self, query: &str) -> Result<String, String>;
    /// Text of the current selection.
    fn selection(&self) -> Result<String, String>;
    /// The document outline.
    fn outline(&self) -> Result<String, String>;

    /// The `tools/list` payload. The default is the fixed read-only set; an
    /// implementation may narrow it but should not widen it.
    fn tools(&self) -> Vec<Value> {
        tool_definitions()
    }
}

/// The fixed read-only tool definitions advertised by PDFXI.
pub fn tool_definitions() -> Vec<Value> {
    vec![
        json!({
            "name": "document_info",
            "description": "Identity and page count of the open PDF.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "page_text",
            "description": "Extracted text of one 1-based page.",
            "inputSchema": {
                "type": "object",
                "properties": {"page": {"type": "integer", "minimum": 1}},
                "required": ["page"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "search",
            "description": "Search the whole document and return matches with page numbers.",
            "inputSchema": {
                "type": "object",
                "properties": {"query": {"type": "string", "minLength": 1}},
                "required": ["query"],
                "additionalProperties": false
            }
        }),
        json!({
            "name": "selection",
            "description": "Text currently selected in the viewer.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
        json!({
            "name": "outline",
            "description": "Document outline (bookmarks) with page targets.",
            "inputSchema": {"type": "object", "properties": {}, "additionalProperties": false}
        }),
    ]
}

/// Limits for one MCP connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct McpOptions {
    /// How long the socket may stay silent before the attach stops.
    pub idle_timeout: Duration,
    /// Maximum messages handled on one connection.
    pub max_messages: u32,
}

impl Default for McpOptions {
    fn default() -> Self {
        Self {
            idle_timeout: Duration::from_secs(10),
            max_messages: 64,
        }
    }
}

/// What happened on one MCP connection.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct McpReport {
    /// `clientInfo.name` from the service initialize frame.
    pub client_name: Option<String>,
    /// Whether `initialize` was received.
    pub initialize_seen: bool,
    /// Whether `notifications/initialized` was received.
    pub initialized: bool,
    /// How many `tools/list` requests the service made.
    pub tools_list_requests: u32,
    /// Tool names returned to the service.
    pub tools_listed: Vec<String>,
    /// Tool names the service called.
    pub tool_calls: Vec<String>,
    /// Messages handled.
    pub messages: u32,
    /// Whether the connection ended on the idle timeout instead of a close.
    pub idle_timed_out: bool,
}

/// A JSON-RPC transport carrying one message per frame.
pub trait McpTransport {
    /// Receives the next message. `Ok(None)` means the peer closed;
    /// [`AssistantError::Timeout`] means the idle budget expired.
    fn receive(&mut self) -> Result<Option<Value>, AssistantError>;
    /// Sends one message.
    fn send(&mut self, message: Value) -> Result<(), AssistantError>;
}

/// JSON-RPC error codes used on this connection.
mod code {
    /// Malformed request or tool arguments.
    pub(crate) const INVALID_PARAMS: i64 = -32602;
    /// Unknown method.
    pub(crate) const METHOD_NOT_FOUND: i64 = -32601;
    /// A request arrived before `initialize`.
    pub(crate) const NOT_INITIALIZED: i64 = -32002;
}

/// One MCP server session over a transport.
pub struct McpSession<S: McpTransport> {
    transport: S,
    options: McpOptions,
    cancel: CancelFlag,
    initialize_seen: bool,
    initialized: bool,
    report: McpReport,
}

impl<S: McpTransport> McpSession<S> {
    /// Builds a session over `transport`.
    pub fn new(transport: S, options: McpOptions) -> Self {
        Self {
            transport,
            options,
            cancel: CancelFlag::new(),
            initialize_seen: false,
            initialized: false,
            report: McpReport::default(),
        }
    }

    /// Stops the session when the shared flag is cancelled.
    pub fn with_cancel(mut self, cancel: CancelFlag) -> Self {
        self.cancel = cancel;
        self
    }

    /// Serves the connection until the peer closes, the idle budget expires, the
    /// message cap is reached or the caller cancels.
    pub fn serve(mut self, provider: &dyn ToolProvider) -> Result<McpReport, AssistantError> {
        loop {
            if self.cancel.is_cancelled() {
                break;
            }
            if self.report.messages >= self.options.max_messages {
                break;
            }
            match self.transport.receive() {
                Ok(Some(message)) => {
                    self.report.messages += 1;
                    self.handle(message, provider)?;
                }
                Ok(None) => break,
                Err(AssistantError::Timeout { .. }) => {
                    self.report.idle_timed_out = true;
                    break;
                }
                Err(error) => return Err(error),
            }
        }
        Ok(self.report)
    }

    fn handle(
        &mut self,
        message: Value,
        provider: &dyn ToolProvider,
    ) -> Result<(), AssistantError> {
        let method = message.get("method").and_then(Value::as_str);
        let id = message.get("id").cloned();
        match method {
            Some("initialize") => {
                self.initialize_seen = true;
                self.report.initialize_seen = true;
                self.report.client_name = message
                    .pointer("/params/clientInfo/name")
                    .and_then(Value::as_str)
                    .map(str::to_owned);
                self.respond(
                    id,
                    json!({
                        "protocolVersion": MCP_PROTOCOL_VERSION,
                        "capabilities": {"tools": {"listChanged": false}},
                        "serverInfo": {
                            "name": MCP_SERVER_NAME,
                            "version": env!("CARGO_PKG_VERSION")
                        }
                    }),
                )
            }
            Some("notifications/initialized") => {
                self.initialized = true;
                self.report.initialized = true;
                Ok(())
            }
            Some("tools/list") => {
                if !self.initialize_seen {
                    return self.respond_error(id, code::NOT_INITIALIZED, "initialize first");
                }
                let tools = provider.tools();
                self.report.tools_list_requests += 1;
                self.report.tools_listed = tools
                    .iter()
                    .filter_map(|tool| tool.get("name").and_then(Value::as_str))
                    .map(str::to_owned)
                    .collect();
                self.respond(id, json!({ "tools": tools }))
            }
            Some("tools/call") => {
                if !self.initialize_seen {
                    return self.respond_error(id, code::NOT_INITIALIZED, "initialize first");
                }
                self.handle_call(id, &message, provider)
            }
            Some("ping") => self.respond(id, json!({})),
            Some(other) => {
                if id.is_some() {
                    self.respond_error(
                        id,
                        code::METHOD_NOT_FOUND,
                        &format!("method not found: {other}"),
                    )
                } else {
                    Ok(())
                }
            }
            None => Ok(()),
        }
    }

    fn handle_call(
        &mut self,
        id: Option<Value>,
        message: &Value,
        provider: &dyn ToolProvider,
    ) -> Result<(), AssistantError> {
        let name = message.pointer("/params/name").and_then(Value::as_str);
        let arguments = message
            .pointer("/params/arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        match dispatch(provider, name, &arguments) {
            Ok(text) => {
                if let Some(name) = name {
                    self.report.tool_calls.push(name.to_owned());
                }
                self.respond(
                    id,
                    json!({
                        "content": [{"type": "text", "text": text}],
                        "isError": false
                    }),
                )
            }
            Err(ToolError::UnknownTool(name)) => {
                self.respond_error(id, code::INVALID_PARAMS, &format!("unknown tool: {name}"))
            }
            Err(ToolError::InvalidParams(message)) => {
                self.respond_error(id, code::INVALID_PARAMS, &message)
            }
            Err(ToolError::Failed(message)) => {
                if let Some(name) = name {
                    self.report.tool_calls.push(name.to_owned());
                }
                self.respond(
                    id,
                    json!({
                        "content": [{"type": "text", "text": redact(&message)}],
                        "isError": true
                    }),
                )
            }
        }
    }

    fn respond(&mut self, id: Option<Value>, result: Value) -> Result<(), AssistantError> {
        let Some(id) = id else {
            return Ok(());
        };
        self.transport
            .send(json!({"jsonrpc": "2.0", "id": id, "result": result}))
    }

    fn respond_error(
        &mut self,
        id: Option<Value>,
        error_code: i64,
        message: &str,
    ) -> Result<(), AssistantError> {
        let Some(id) = id else {
            return Ok(());
        };
        self.transport.send(json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {"code": error_code, "message": message}
        }))
    }
}

enum ToolError {
    UnknownTool(String),
    InvalidParams(String),
    Failed(String),
}

fn dispatch(
    provider: &dyn ToolProvider,
    name: Option<&str>,
    arguments: &Value,
) -> Result<String, ToolError> {
    let name = name.ok_or_else(|| ToolError::InvalidParams("missing tool name".to_owned()))?;
    let outcome = match name {
        "document_info" => provider.document_info(),
        "page_text" => {
            let page = arguments
                .get("page")
                .and_then(Value::as_u64)
                .ok_or_else(|| {
                    ToolError::InvalidParams("page_text requires an integer page".to_owned())
                })?;
            let page = u32::try_from(page).map_err(|_| {
                ToolError::InvalidParams("page_text page is out of range".to_owned())
            })?;
            if page == 0 {
                return Err(ToolError::InvalidParams(
                    "page_text page is 1-based".to_owned(),
                ));
            }
            provider.page_text(page)
        }
        "search" => {
            let query = arguments
                .get("query")
                .and_then(Value::as_str)
                .ok_or_else(|| {
                    ToolError::InvalidParams("search requires a string query".to_owned())
                })?;
            if query.trim().is_empty() {
                return Err(ToolError::InvalidParams("search query is empty".to_owned()));
            }
            provider.search(query)
        }
        "selection" => provider.selection(),
        "outline" => provider.outline(),
        other => return Err(ToolError::UnknownTool(other.to_owned())),
    };
    outcome.map(cap_tool_text).map_err(ToolError::Failed)
}

fn cap_tool_text(mut text: String) -> String {
    if text.chars().count() <= MAX_TOOL_TEXT_CHARS {
        return text;
    }
    text = text.chars().take(MAX_TOOL_TEXT_CHARS).collect();
    text.push_str("\n[truncated by PDFXI]");
    text
}

/// A blocking WebSocket MCP transport.
pub struct WebSocketTransport {
    socket: tungstenite::WebSocket<MaybeTlsStream<TcpStream>>,
    idle_timeout: Duration,
}

impl McpTransport for WebSocketTransport {
    fn receive(&mut self) -> Result<Option<Value>, AssistantError> {
        loop {
            match self.socket.read() {
                Ok(tungstenite::Message::Text(text)) => {
                    return parse_message(text.as_str()).map(Some);
                }
                Ok(tungstenite::Message::Binary(bytes)) => {
                    let text =
                        std::str::from_utf8(&bytes).map_err(|_| AssistantError::Protocol {
                            message: "MCP binary frame is not UTF-8".to_owned(),
                        })?;
                    return parse_message(text).map(Some);
                }
                Ok(tungstenite::Message::Close(_)) => return Ok(None),
                Ok(tungstenite::Message::Ping(payload)) => {
                    let _ = self.socket.send(tungstenite::Message::Pong(payload));
                }
                Ok(tungstenite::Message::Pong(_)) | Ok(tungstenite::Message::Frame(_)) => {}
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Ok(None);
                }
                Err(tungstenite::Error::Io(error)) if is_timeout(&error) => {
                    return Err(AssistantError::Timeout {
                        what: "MCP socket idle".to_owned(),
                        millis: self.idle_timeout.as_millis() as u64,
                    });
                }
                Err(error) => {
                    return Err(AssistantError::Network {
                        message: format!("MCP socket: {}", redact(&error.to_string())),
                    });
                }
            }
        }
    }

    fn send(&mut self, message: Value) -> Result<(), AssistantError> {
        let text = serde_json::to_string(&message).map_err(|error| AssistantError::Protocol {
            message: format!("cannot encode an MCP message: {error}"),
        })?;
        self.socket
            .send(tungstenite::Message::Text(text.into()))
            .map_err(|error| AssistantError::Network {
                message: format!("MCP socket: {}", redact(&error.to_string())),
            })
    }
}

fn parse_message(text: &str) -> Result<Value, AssistantError> {
    serde_json::from_str(text).map_err(|error| AssistantError::Protocol {
        message: format!("invalid MCP JSON: {error}"),
    })
}

/// Opens the client-MCP socket.
pub(crate) fn connect(
    url: &Url,
    authorization: &str,
    timeouts: &Timeouts,
    idle_timeout: Duration,
) -> Result<WebSocketTransport, AssistantError> {
    use tungstenite::client::IntoClientRequest;

    let mut request =
        url.as_str()
            .into_client_request()
            .map_err(|error| AssistantError::Protocol {
                message: format!("invalid MCP socket URL: {error}"),
            })?;
    request.headers_mut().insert(
        "Authorization",
        http::HeaderValue::from_str(authorization).map_err(|_| {
            AssistantError::InvalidArgument {
                message: "the Basic credential is not a valid header value".to_owned(),
            }
        })?,
    );

    let host = url
        .host_str()
        .ok_or_else(|| AssistantError::InvalidEndpoint {
            reason: "the MCP socket URL has no host".to_owned(),
        })?;
    let port = url
        .port_or_known_default()
        .ok_or_else(|| AssistantError::InvalidEndpoint {
            reason: "the MCP socket URL has no port".to_owned(),
        })?;
    let address = (host, port)
        .to_socket_addrs()
        .map_err(|error| AssistantError::Network {
            message: format!("cannot resolve {host}:{port}: {error}"),
        })?
        .next()
        .ok_or_else(|| AssistantError::Network {
            message: format!("no address for {host}:{port}"),
        })?;
    let stream = TcpStream::connect_timeout(&address, timeouts.connect).map_err(|error| {
        AssistantError::Network {
            message: format!(
                "cannot connect to {host}:{port}: {}",
                redact(&error.to_string())
            ),
        }
    })?;
    stream
        .set_read_timeout(Some(idle_timeout))
        .and_then(|()| stream.set_write_timeout(Some(timeouts.read)))
        .map_err(|error| AssistantError::Io {
            message: format!("cannot configure the MCP socket: {error}"),
        })?;
    let _ = stream.set_nodelay(true);
    let stream = wrap_tls(stream, url, host)?;
    let (socket, _response) =
        tungstenite::client::client(request, stream).map_err(|error| AssistantError::Protocol {
            message: format!("MCP handshake failed: {}", redact(&error.to_string())),
        })?;
    Ok(WebSocketTransport {
        socket,
        idle_timeout,
    })
}

fn wrap_tls(
    stream: TcpStream,
    url: &Url,
    host: &str,
) -> Result<MaybeTlsStream<TcpStream>, AssistantError> {
    if url.scheme() == "ws" {
        return Ok(MaybeTlsStream::Plain(stream));
    }
    let mut roots = rustls::RootCertStore::empty();
    for certificate in rustls_native_certs::load_native_certs().certs {
        let _ = roots.add(certificate);
    }
    let config = rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|error| AssistantError::Configuration {
        message: format!("rustls protocol versions: {error}"),
    })?
    .with_root_certificates(roots)
    .with_no_client_auth();
    let server_name = rustls::pki_types::ServerName::try_from(host.to_owned()).map_err(|_| {
        AssistantError::InvalidEndpoint {
            reason: format!("{host:?} is not a valid TLS server name"),
        }
    })?;
    let connection =
        rustls::ClientConnection::new(Arc::new(config), server_name).map_err(|error| {
            AssistantError::Configuration {
                message: format!("rustls client connection: {error}"),
            }
        })?;
    Ok(MaybeTlsStream::Rustls(rustls::StreamOwned::new(
        connection, stream,
    )))
}

/// Whether a service-supplied MCP server name is acceptable.
pub(crate) fn validate_name(name: &str) -> Result<(), AssistantError> {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(AssistantError::InvalidArgument {
            message: format!("invalid MCP server name {name:?}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{
        MCP_PROTOCOL_VERSION, McpOptions, McpSession, McpTransport, ToolProvider, tool_definitions,
        validate_name,
    };
    use serde_json::{Value, json};
    use std::collections::VecDeque;
    use std::sync::{Arc, Mutex};

    struct StubProvider {
        calls: Mutex<Vec<String>>,
        fail: bool,
    }

    impl ToolProvider for StubProvider {
        fn document_info(&self) -> Result<String, String> {
            self.calls
                .lock()
                .expect("calls")
                .push("document_info".to_owned());
            Ok("PDFXI test document, 2 pages".to_owned())
        }
        fn page_text(&self, page: u32) -> Result<String, String> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("page_text:{page}"));
            Ok(format!("text of page {page}"))
        }
        fn search(&self, query: &str) -> Result<String, String> {
            self.calls
                .lock()
                .expect("calls")
                .push(format!("search:{query}"));
            Ok("1 match on page 2".to_owned())
        }
        fn selection(&self) -> Result<String, String> {
            self.calls
                .lock()
                .expect("calls")
                .push("selection".to_owned());
            Ok("selected text".to_owned())
        }
        fn outline(&self) -> Result<String, String> {
            self.calls.lock().expect("calls").push("outline".to_owned());
            if self.fail {
                return Err("the document has no outline".to_owned());
            }
            Ok("Chapter 1 -> page 1".to_owned())
        }
    }

    struct Scripted {
        incoming: VecDeque<Value>,
        sent: Arc<Mutex<Vec<Value>>>,
    }

    impl Scripted {
        fn new(messages: Vec<Value>, sent: Arc<Mutex<Vec<Value>>>) -> Self {
            Self {
                incoming: messages.into(),
                sent,
            }
        }
    }

    impl McpTransport for Scripted {
        fn receive(&mut self) -> Result<Option<Value>, super::AssistantError> {
            Ok(self.incoming.pop_front())
        }
        fn send(&mut self, message: Value) -> Result<(), super::AssistantError> {
            self.sent.lock().expect("sent").push(message);
            Ok(())
        }
    }

    fn run(messages: Vec<Value>, fail: bool) -> (Vec<Value>, super::McpReport) {
        run_with(messages, fail, McpOptions::default())
    }

    fn run_with(
        messages: Vec<Value>,
        fail: bool,
        options: McpOptions,
    ) -> (Vec<Value>, super::McpReport) {
        let sent = Arc::new(Mutex::new(Vec::new()));
        let provider = StubProvider {
            calls: Mutex::new(Vec::new()),
            fail,
        };
        let session = McpSession::new(Scripted::new(messages, Arc::clone(&sent)), options);
        let report = session.serve(&provider).expect("serve");
        let sent = sent.lock().expect("sent").clone();
        (sent, report)
    }

    fn handshake() -> Vec<Value> {
        vec![
            json!({"jsonrpc":"2.0","method":"initialize","params":{
                "protocolVersion":"2025-11-25",
                "clientInfo":{"name":"opencode","version":"2.3.2"}},"id":0}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
        ]
    }

    #[test]
    fn initialize_answers_with_the_pinned_protocol_and_server_info() {
        let messages = vec![json!({
            "jsonrpc": "2.0",
            "method": "initialize",
            "params": {"protocolVersion": "2025-11-25", "clientInfo": {"name": "opencode", "version": "2.3.2"}},
            "id": 0
        })];
        let (_, report) = run(messages, false);
        assert!(report.initialize_seen);
        assert_eq!(report.client_name.as_deref(), Some("opencode"));
        assert_eq!(MCP_PROTOCOL_VERSION, "2025-11-25");
    }

    #[test]
    fn tool_definitions_are_the_read_only_set() {
        let tools = tool_definitions();
        let names: Vec<&str> = tools
            .iter()
            .filter_map(|tool| tool.get("name").and_then(Value::as_str))
            .collect();
        assert_eq!(
            names,
            vec![
                "document_info",
                "page_text",
                "search",
                "selection",
                "outline"
            ]
        );
        for tool in &tools {
            let name = tool["name"].as_str().unwrap_or_default();
            assert!(!name.contains("shell"));
            assert!(!name.contains("file"));
            assert!(tool.get("inputSchema").is_some());
        }
    }

    #[test]
    fn initialize_response_carries_protocol_and_server_info() {
        let (sent, report) = run(handshake(), false);
        let response = sent.first().expect("initialize response");
        assert_eq!(response["jsonrpc"], "2.0");
        assert_eq!(response["id"], 0);
        assert_eq!(response["result"]["protocolVersion"], "2025-11-25");
        assert_eq!(response["result"]["serverInfo"]["name"], "pdfxi");
        assert!(response["result"]["capabilities"]["tools"].is_object());
        assert!(report.initialized);
    }

    #[test]
    fn answers_tools_list_with_the_read_only_set() {
        let mut messages = handshake();
        messages.push(json!({"jsonrpc":"2.0","method":"tools/list","id":1}));
        let (sent, report) = run(messages, false);
        let response = sent.last().expect("tools/list response");
        let tools = response["result"]["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 5);
        assert_eq!(report.tools_list_requests, 1);
        assert_eq!(report.tools_listed.len(), 5);
        assert!(report.tools_listed.contains(&"page_text".to_owned()));
    }

    #[test]
    fn dispatches_tool_calls_to_the_provider() {
        let mut messages = handshake();
        messages.push(json!({"jsonrpc":"2.0","method":"tools/call","id":2,
            "params":{"name":"page_text","arguments":{"page":3}}}));
        let (sent, report) = run(messages, false);
        let response = sent.last().expect("tool response");
        assert_eq!(response["result"]["isError"], false);
        assert_eq!(response["result"]["content"][0]["type"], "text");
        assert_eq!(response["result"]["content"][0]["text"], "text of page 3");
        assert_eq!(report.tool_calls, vec!["page_text".to_owned()]);
    }

    #[test]
    fn invalid_tool_arguments_and_unknown_tools_are_json_rpc_errors() {
        let mut messages = handshake();
        messages.push(json!({"jsonrpc":"2.0","method":"tools/call","id":3,
            "params":{"name":"page_text","arguments":{}}}));
        messages.push(json!({"jsonrpc":"2.0","method":"tools/call","id":4,
            "params":{"name":"shell","arguments":{}}}));
        messages.push(json!({"jsonrpc":"2.0","method":"nope","id":5}));
        let (sent, report) = run(messages, false);
        assert_eq!(sent[1]["error"]["code"], -32602);
        assert_eq!(sent[2]["error"]["code"], -32602);
        assert_eq!(sent[3]["error"]["code"], -32601);
        assert!(report.tool_calls.is_empty());
    }

    #[test]
    fn requests_before_initialize_are_rejected() {
        let messages = vec![json!({"jsonrpc":"2.0","method":"tools/list","id":7})];
        let (sent, report) = run(messages, false);
        assert_eq!(sent[0]["error"]["code"], -32002);
        assert!(!report.initialize_seen);
    }

    #[test]
    fn provider_failures_are_reported_as_tool_errors() {
        let mut messages = handshake();
        messages.push(json!({"jsonrpc":"2.0","method":"tools/call","id":8,
            "params":{"name":"outline","arguments":{}}}));
        let (sent, _) = run(messages, true);
        let response = sent.last().expect("tool error response");
        assert_eq!(response["result"]["isError"], true);
        assert_eq!(
            response["result"]["content"][0]["text"],
            "the document has no outline"
        );
    }

    #[test]
    fn the_message_cap_ends_the_session() {
        let mut messages = handshake();
        for id in 0..10 {
            messages.push(json!({"jsonrpc":"2.0","method":"ping","id":id}));
        }
        let (sent, report) = run_with(
            messages,
            false,
            McpOptions {
                max_messages: 3,
                ..McpOptions::default()
            },
        );
        assert_eq!(report.messages, 3);
        assert_eq!(sent.len(), 2, "initialize result plus one ping");
    }

    #[test]
    fn mcp_server_name_is_validated() {
        assert!(validate_name("pdfxi-t0251").is_ok());
        for bad in ["", "../x", "a b", "a?b", &"a".repeat(65)] {
            assert!(validate_name(bad).is_err());
        }
    }
}
