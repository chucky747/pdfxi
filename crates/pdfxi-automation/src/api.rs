//! Local, line-delimited JSON automation API.
//!
//! This module defines a versioned request/response protocol that works over
//! plain text lines, with no sockets and no dependency on a transport. One
//! request line produces exactly one response line. Both envelopes are typed
//! and serialize to a canonical JSON form, so output is deterministic and easy
//! to assert in tests or consume from a script.
//!
//! # Protocol
//!
//! Request (one line):
//!
//! ```json
//! {"command":"dry-run","id":"1","params":{"file":"actions.txt"},"version":1}
//! ```
//!
//! Response (one line):
//!
//! ```json
//! {"error":null,"id":"1","ok":true,"result":{"action":"nightly","ok":true,"steps":2},"version":1}
//! ```
//!
//! On failure `ok` is `false`, `result` is `null`, and `error` is an object
//! with `code` and `message`. All four keys are always present.
//!
//! # Safety and bounds
//!
//! Lines longer than [`MAX_LINE_BYTES`] are rejected before parsing. The JSON
//! parser bounds depth and node count, and every command validates its
//! parameters before touching an [`ApiHost`]. File access and execution are
//! delegated to the host, so this module itself performs no I/O. Toolset
//! aliases that shadow dangerous built-ins are rejected at validation time,
//! and aliases whose target action contains a [`DANGEROUS_KINDS`] step are
//! rejected at run time.

use std::collections::BTreeMap;
use std::fmt;

use crate::action::Action;
use crate::error::AutomationError;
use crate::json::{JsonError, JsonValue};
use crate::library::ActionLibrary;
use crate::toolset::{DANGEROUS_KINDS, Toolset, ToolsetError};

/// Protocol version understood by [`ApiEngine`].
pub const API_VERSION: u32 = 1;
/// Largest request line accepted, in bytes.
pub const MAX_LINE_BYTES: usize = 1_048_576;

/// Commands understood by the engine.
pub const SUPPORTED_COMMANDS: &[&str] = &[
    "version",
    "commands",
    "list-actions",
    "show-action",
    "dry-run",
    "run-batch",
    "list-toolsets",
    "show-toolset",
    "validate-toolset",
    "run-toolset",
];

/// A typed request envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct Request {
    /// Caller-supplied correlation id, echoed verbatim in the response.
    pub id: String,
    /// Command name to dispatch.
    pub command: String,
    /// Command-specific parameters, always a JSON object.
    pub params: JsonValue,
}

impl Request {
    /// Creates a request with the given id, command, and parameters.
    pub fn new(id: impl Into<String>, command: impl Into<String>, params: JsonValue) -> Self {
        Self {
            id: id.into(),
            command: command.into(),
            params,
        }
    }

    /// Serializes the request with the current protocol version.
    pub fn to_json(&self) -> String {
        JsonValue::object([
            ("command".to_owned(), JsonValue::from(self.command.as_str())),
            ("id".to_owned(), JsonValue::from(self.id.as_str())),
            ("params".to_owned(), self.params.clone()),
            ("version".to_owned(), JsonValue::number(API_VERSION as u64)),
        ])
        .to_string()
    }
}

/// A typed protocol or command error.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ApiError {
    /// Stable machine-readable code.
    code: String,
    /// Human-readable message.
    message: String,
}

impl ApiError {
    /// Creates an error with a stable code and a message.
    pub fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
        }
    }

    /// Returns the stable error code.
    pub fn code(&self) -> &str {
        &self.code
    }

    /// Returns the human-readable message.
    pub fn message(&self) -> &str {
        &self.message
    }
}

impl fmt::Display for ApiError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for ApiError {}

fn io_error(context: &str, message: String) -> ApiError {
    ApiError::new("host_error", format!("{context}: {message}"))
}

/// A typed response envelope.
#[derive(Debug, Clone, PartialEq)]
pub struct Response {
    /// Correlation id from the request, or `""` when the line could not be read.
    pub id: String,
    /// `true` when the command succeeded.
    pub ok: bool,
    /// Command result on success, otherwise `None`.
    pub result: Option<JsonValue>,
    /// Error details on failure, otherwise `None`.
    pub error: Option<ApiError>,
    /// Protocol version echoed from the engine.
    pub version: u32,
}

impl Response {
    /// Creates a successful response.
    pub fn success(id: impl Into<String>, result: JsonValue) -> Self {
        Self {
            id: id.into(),
            ok: true,
            result: Some(result),
            error: None,
            version: API_VERSION,
        }
    }

    /// Creates a failed response.
    pub fn failure(
        id: impl Into<String>,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            id: id.into(),
            ok: false,
            result: None,
            error: Some(ApiError::new(code, message)),
            version: API_VERSION,
        }
    }

    /// Serializes the response to canonical, compact JSON.
    pub fn to_json(&self) -> String {
        let error = match &self.error {
            Some(error) => JsonValue::object([
                ("code".to_owned(), JsonValue::from(error.code())),
                ("message".to_owned(), JsonValue::from(error.message())),
            ]),
            None => JsonValue::Null,
        };
        let result = self.result.clone().unwrap_or(JsonValue::Null);
        JsonValue::object([
            ("error".to_owned(), error),
            ("id".to_owned(), JsonValue::from(self.id.as_str())),
            ("ok".to_owned(), JsonValue::from(self.ok)),
            ("result".to_owned(), result),
            ("version".to_owned(), JsonValue::number(self.version as u64)),
        ])
        .to_string()
    }

    /// Parses a response produced by [`Response::to_json`].
    pub fn parse(text: &str) -> Result<Self, JsonError> {
        let value = JsonValue::parse(text)?;
        let entries = value
            .as_object()
            .ok_or_else(|| JsonError::new(0, "response must be a JSON object"))?;
        let id = require_str(entries, "id")?;
        let ok = entries
            .get("ok")
            .and_then(|value| match value {
                JsonValue::Bool(flag) => Some(*flag),
                _ => None,
            })
            .ok_or_else(|| JsonError::new(0, "response 'ok' must be a boolean"))?;
        let version = entries
            .get("version")
            .and_then(JsonValue::as_u64)
            .and_then(|value| u32::try_from(value).ok())
            .ok_or_else(|| JsonError::new(0, "response 'version' must be an integer"))?;
        let result = match entries.get("result") {
            None | Some(JsonValue::Null) => None,
            Some(value) => Some(value.clone()),
        };
        let error = match entries.get("error") {
            None | Some(JsonValue::Null) => None,
            Some(value) => {
                let fields = value
                    .as_object()
                    .ok_or_else(|| JsonError::new(0, "response 'error' must be an object"))?;
                Some(ApiError::new(
                    require_str(fields, "code")?,
                    require_str(fields, "message")?,
                ))
            }
        };
        Ok(Self {
            id,
            ok,
            result,
            error,
            version,
        })
    }
}

fn require_str(entries: &BTreeMap<String, JsonValue>, key: &str) -> Result<String, JsonError> {
    entries
        .get(key)
        .and_then(JsonValue::as_str)
        .map(str::to_owned)
        .ok_or_else(|| JsonError::new(0, format!("response '{key}' must be a string")))
}

/// Host operations the engine delegates to its caller.
///
/// Keeping file access and execution behind this trait makes the protocol
/// testable with an in-memory fake ([`MemoryHost`]) while the CLI supplies a
/// host backed by real files and the staging document executor.
pub trait ApiHost {
    /// Reads a UTF-8 file, returning an error message on failure.
    fn read_file(&self, path: &str) -> Result<String, String>;

    /// Executes `action` over `inputs`, returning a JSON result object.
    fn run_action(&self, action: &Action, inputs: &[String]) -> Result<JsonValue, String>;
}

/// A deterministic, in-memory [`ApiHost`] for tests and examples.
///
/// `run_action` does not execute anything; it returns a stable summary of the
/// action name, step count, and inputs so protocol tests stay content-free.
#[derive(Debug, Default)]
pub struct MemoryHost {
    files: BTreeMap<String, String>,
}

impl MemoryHost {
    /// Creates an empty host.
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds or replaces a text file addressed by `path`.
    pub fn insert_file(&mut self, path: impl Into<String>, text: impl Into<String>) {
        self.files.insert(path.into(), text.into());
    }

    /// Builder-style variant of [`MemoryHost::insert_file`].
    pub fn with_file(mut self, path: impl Into<String>, text: impl Into<String>) -> Self {
        self.insert_file(path, text);
        self
    }
}

impl ApiHost for MemoryHost {
    fn read_file(&self, path: &str) -> Result<String, String> {
        self.files
            .get(path)
            .cloned()
            .ok_or_else(|| format!("no in-memory file named '{path}'"))
    }

    fn run_action(&self, action: &Action, inputs: &[String]) -> Result<JsonValue, String> {
        let jobs: Vec<JsonValue> = inputs
            .iter()
            .enumerate()
            .map(|(index, input)| {
                JsonValue::object([
                    ("id".to_owned(), JsonValue::from(format!("job-{index}"))),
                    ("input".to_owned(), JsonValue::from(input.as_str())),
                    ("state".to_owned(), JsonValue::from("succeeded")),
                ])
            })
            .collect();
        Ok(JsonValue::object([
            ("action".to_owned(), JsonValue::from(action.name())),
            ("jobs".to_owned(), JsonValue::Array(jobs)),
            ("steps".to_owned(), JsonValue::number(action.len() as u64)),
        ]))
    }
}

/// The versioned request dispatcher.
#[derive(Debug, Clone, Copy)]
pub struct ApiEngine {
    version: u32,
}

impl Default for ApiEngine {
    fn default() -> Self {
        Self::new()
    }
}

impl ApiEngine {
    /// Creates an engine using [`API_VERSION`].
    pub fn new() -> Self {
        Self {
            version: API_VERSION,
        }
    }

    /// Creates an engine that expects a specific version (for mismatch tests).
    pub fn with_version(version: u32) -> Self {
        Self { version }
    }

    /// Returns the expected protocol version.
    pub fn version(self) -> u32 {
        self.version
    }

    /// Returns the commands this engine can dispatch.
    pub fn supported_commands() -> &'static [&'static str] {
        SUPPORTED_COMMANDS
    }

    /// Parses one request line into a typed [`Request`].
    pub fn parse_request(&self, line: &str) -> Result<Request, ApiError> {
        if line.len() > MAX_LINE_BYTES {
            return Err(ApiError::new(
                "line_too_long",
                format!(
                    "request is {} bytes, which exceeds the {MAX_LINE_BYTES}-byte limit",
                    line.len()
                ),
            ));
        }
        let value = JsonValue::parse(line)
            .map_err(|error| ApiError::new("invalid_json", error.to_string()))?;
        let entries = value
            .as_object()
            .ok_or_else(|| ApiError::new("invalid_request", "request must be a JSON object"))?;

        let found = entries
            .get("version")
            .and_then(JsonValue::as_u64)
            .ok_or_else(|| {
                ApiError::new(
                    "version_mismatch",
                    format!("missing or invalid 'version'; expected {}", self.version),
                )
            })?;
        if found != u64::from(self.version) {
            return Err(ApiError::new(
                "version_mismatch",
                format!(
                    "unsupported protocol version {found}; expected {}",
                    self.version
                ),
            ));
        }

        let id = match entries.get("id") {
            Some(JsonValue::String(id)) => id.clone(),
            Some(JsonValue::Number(id)) => id.clone(),
            _ => {
                return Err(ApiError::new(
                    "invalid_request",
                    "field 'id' must be a string or number",
                ));
            }
        };
        let command = entries
            .get("command")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| {
                ApiError::new(
                    "invalid_request",
                    "field 'command' must be a non-empty string",
                )
            })?
            .to_owned();
        if command.is_empty() {
            return Err(ApiError::new(
                "invalid_request",
                "field 'command' must be a non-empty string",
            ));
        }
        let params = match entries.get("params") {
            None | Some(JsonValue::Null) => JsonValue::Object(BTreeMap::new()),
            Some(value @ JsonValue::Object(_)) => value.clone(),
            Some(_) => {
                return Err(ApiError::new(
                    "invalid_request",
                    "field 'params' must be an object",
                ));
            }
        };
        Ok(Request {
            id,
            command,
            params,
        })
    }

    /// Handles one request line, returning exactly one canonical response line.
    ///
    /// This method never panics: every input, including oversized or malformed
    /// text, yields a response with `ok: false`.
    pub fn handle_line(&self, line: &str, host: &dyn ApiHost) -> String {
        let mut response = match self.parse_request(line) {
            Ok(request) => self.handle_request(request, host),
            Err(error) => Response::failure("", error.code(), error.message()),
        };
        response.version = self.version;
        response.to_json()
    }

    /// Dispatches an already-parsed request.
    pub fn handle_request(&self, request: Request, host: &dyn ApiHost) -> Response {
        let id = request.id.clone();
        let result = match self.dispatch(&request, host) {
            Ok(result) => result,
            Err(error) => {
                let mut response = Response::failure(id, error.code(), error.message());
                response.version = self.version;
                return response;
            }
        };
        let mut response = Response::success(id, result);
        response.version = self.version;
        response
    }

    fn dispatch(&self, request: &Request, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
        match request.command.as_str() {
            "version" => Ok(JsonValue::object([
                ("engine".to_owned(), JsonValue::from("pdfxi-automation")),
                (
                    "version".to_owned(),
                    JsonValue::number(u64::from(self.version)),
                ),
            ])),
            "commands" => Ok(JsonValue::object([(
                "commands".to_owned(),
                JsonValue::Array(
                    SUPPORTED_COMMANDS
                        .iter()
                        .map(|name| JsonValue::from(*name))
                        .collect(),
                ),
            )])),
            "list-actions" => list_actions(&request.params, host),
            "show-action" => show_action(&request.params, host),
            "dry-run" => dry_run(&request.params, host),
            "run-batch" => run_batch(&request.params, host),
            "list-toolsets" => list_toolsets(&request.params, host),
            "show-toolset" => show_toolset(&request.params, host),
            "validate-toolset" => validate_toolset(&request.params, host),
            "run-toolset" => run_toolset(&request.params, host),
            other => Err(ApiError::new(
                "unknown_command",
                format!(
                    "unknown command '{other}'; supported commands: {}",
                    SUPPORTED_COMMANDS.join(", ")
                ),
            )),
        }
    }
}

fn require_string(params: &JsonValue, key: &str) -> Result<String, ApiError> {
    params
        .get(key)
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_owned)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_params",
                format!("parameter '{key}' must be a non-empty string"),
            )
        })
}

fn require_strings(params: &JsonValue, key: &str) -> Result<Vec<String>, ApiError> {
    let values = params
        .get(key)
        .and_then(JsonValue::as_array)
        .ok_or_else(|| {
            ApiError::new(
                "invalid_params",
                format!("parameter '{key}' must be an array of strings"),
            )
        })?;
    values
        .iter()
        .map(|value| {
            value.as_str().map(str::to_owned).ok_or_else(|| {
                ApiError::new(
                    "invalid_params",
                    format!("parameter '{key}' must contain only strings"),
                )
            })
        })
        .collect()
}

fn load_library(
    params: &JsonValue,
    host: &dyn ApiHost,
) -> Result<(String, ActionLibrary), ApiError> {
    let file = require_string(params, "file")?;
    let text = host
        .read_file(&file)
        .map_err(|error| io_error(&format!("failed to read action file '{file}'"), error))?;
    let library = ActionLibrary::from_text(&text)
        .map_err(|error| ApiError::new("invalid_action_file", error.to_string()))?;
    Ok((file, library))
}

fn action_to_json(action: &Action) -> JsonValue {
    let steps: Vec<JsonValue> = action
        .steps()
        .map(|step| {
            let parameters: BTreeMap<String, JsonValue> = step
                .parameters()
                .map(|(key, value)| (key.to_owned(), JsonValue::from(value)))
                .collect();
            JsonValue::object([
                ("id".to_owned(), JsonValue::from(step.id())),
                ("kind".to_owned(), JsonValue::from(step.kind().as_str())),
                ("parameters".to_owned(), JsonValue::Object(parameters)),
            ])
        })
        .collect();
    JsonValue::object([
        (
            "description".to_owned(),
            JsonValue::from(action.description()),
        ),
        ("name".to_owned(), JsonValue::from(action.name())),
        ("steps".to_owned(), JsonValue::Array(steps)),
    ])
}

fn list_actions(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let (file, library) = load_library(params, host)?;
    let actions: Vec<JsonValue> = library
        .actions()
        .map(|action| {
            JsonValue::object([
                (
                    "description".to_owned(),
                    JsonValue::from(action.description()),
                ),
                ("name".to_owned(), JsonValue::from(action.name())),
                ("steps".to_owned(), JsonValue::number(action.len() as u64)),
            ])
        })
        .collect();
    Ok(JsonValue::object([
        ("actions".to_owned(), JsonValue::Array(actions)),
        ("file".to_owned(), JsonValue::from(file.as_str())),
    ]))
}

fn show_action(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let name = require_string(params, "action")?;
    let (_, library) = load_library(params, host)?;
    let action = library
        .get(&name)
        .ok_or_else(|| action_not_found(&name, &library))?;
    Ok(action_to_json(action))
}

fn action_not_found(name: &str, library: &ActionLibrary) -> ApiError {
    let names: Vec<&str> = library.names().collect();
    ApiError::new(
        "action_not_found",
        format!(
            "no action named '{name}' in this file; available actions: {}",
            if names.is_empty() {
                "<none>".to_owned()
            } else {
                names.join(", ")
            }
        ),
    )
}

/// Validates an action file (or one named action) without executing it.
fn dry_run(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let (_, library) = load_library(params, host)?;
    match params.get("action") {
        None | Some(JsonValue::Null) => Ok(JsonValue::object([
            (
                "actions".to_owned(),
                JsonValue::number(library.len() as u64),
            ),
            ("ok".to_owned(), JsonValue::from(true)),
        ])),
        Some(value) => {
            let name = value.as_str().ok_or_else(|| {
                ApiError::new("invalid_params", "parameter 'action' must be a string")
            })?;
            let action = library
                .get(name)
                .ok_or_else(|| action_not_found(name, &library))?;
            action.validate().map_err(|error: AutomationError| {
                ApiError::new("invalid_action", error.to_string())
            })?;
            Ok(JsonValue::object([
                ("action".to_owned(), JsonValue::from(action.name())),
                ("ok".to_owned(), JsonValue::from(true)),
                ("steps".to_owned(), JsonValue::number(action.len() as u64)),
            ]))
        }
    }
}

fn run_batch(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let name = require_string(params, "action")?;
    let inputs = require_strings(params, "inputs")?;
    if inputs.is_empty() {
        return Err(ApiError::new(
            "invalid_params",
            "parameter 'inputs' must contain at least one path",
        ));
    }
    let (_, library) = load_library(params, host)?;
    let action = library
        .get(&name)
        .ok_or_else(|| action_not_found(&name, &library))?;
    action
        .validate()
        .map_err(|error: AutomationError| ApiError::new("invalid_action", error.to_string()))?;
    let run = host
        .run_action(action, &inputs)
        .map_err(|error| io_error("batch execution failed", error))?;
    Ok(JsonValue::object([
        ("action".to_owned(), JsonValue::from(action.name())),
        ("run".to_owned(), run),
    ]))
}

fn load_toolset(params: &JsonValue, host: &dyn ApiHost) -> Result<(String, Toolset), ApiError> {
    let file = require_string(params, "file")?;
    let text = host
        .read_file(&file)
        .map_err(|error| io_error(&format!("failed to read toolset file '{file}'"), error))?;
    let toolset = Toolset::from_json(&text).map_err(toolset_error)?;
    Ok((file, toolset))
}

fn toolset_error(error: ToolsetError) -> ApiError {
    ApiError::new(error.code(), error.to_string())
}

fn tool_to_json(alias: &str, tool: &crate::toolset::Tool) -> JsonValue {
    let defaults: BTreeMap<String, JsonValue> = tool
        .defaults()
        .iter()
        .map(|(key, value)| (key.clone(), JsonValue::from(value.as_str())))
        .collect();
    JsonValue::object([
        ("action".to_owned(), JsonValue::from(tool.action())),
        ("alias".to_owned(), JsonValue::from(alias)),
        ("defaults".to_owned(), JsonValue::Object(defaults)),
        ("file".to_owned(), JsonValue::from(tool.file())),
    ])
}

fn list_toolsets(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let (file, toolset) = load_toolset(params, host)?;
    let tools: Vec<JsonValue> = toolset
        .tools()
        .map(|(alias, tool)| tool_to_json(alias, tool))
        .collect();
    Ok(JsonValue::object([
        ("file".to_owned(), JsonValue::from(file.as_str())),
        ("name".to_owned(), JsonValue::from(toolset.name())),
        ("tools".to_owned(), JsonValue::Array(tools)),
    ]))
}

fn show_toolset(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let alias = require_string(params, "tool")?;
    let (_, toolset) = load_toolset(params, host)?;
    let tool = toolset
        .get(&alias)
        .ok_or_else(|| unknown_tool(&alias, &toolset))?;
    Ok(tool_to_json(&alias, tool))
}

fn unknown_tool(alias: &str, toolset: &Toolset) -> ApiError {
    let names: Vec<&str> = toolset.aliases().collect();
    ApiError::new(
        "unknown_tool",
        format!(
            "toolset '{}' has no tool named '{alias}'; available tools: {}",
            toolset.name(),
            if names.is_empty() {
                "<none>".to_owned()
            } else {
                names.join(", ")
            }
        ),
    )
}

fn validate_toolset(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let (_, toolset) = load_toolset(params, host)?;
    toolset.validate().map_err(toolset_error)?;
    Ok(JsonValue::object([
        ("name".to_owned(), JsonValue::from(toolset.name())),
        ("ok".to_owned(), JsonValue::from(true)),
        ("tools".to_owned(), JsonValue::number(toolset.len() as u64)),
    ]))
}

fn run_toolset(params: &JsonValue, host: &dyn ApiHost) -> Result<JsonValue, ApiError> {
    let alias = require_string(params, "tool")?;
    let inputs = require_strings(params, "inputs")?;
    if inputs.is_empty() {
        return Err(ApiError::new(
            "invalid_params",
            "parameter 'inputs' must contain at least one path",
        ));
    }
    let (_, toolset) = load_toolset(params, host)?;
    let tool = toolset
        .get(&alias)
        .ok_or_else(|| unknown_tool(&alias, &toolset))?;

    let text = host.read_file(tool.file()).map_err(|error| {
        io_error(
            &format!("failed to read action file '{}'", tool.file()),
            error,
        )
    })?;
    let library = ActionLibrary::from_text(&text)
        .map_err(|error| ApiError::new("invalid_action_file", error.to_string()))?;
    let mut action = library
        .get(tool.action())
        .ok_or_else(|| action_not_found(tool.action(), &library))?
        .clone();

    for step in action.steps() {
        if DANGEROUS_KINDS.contains(&step.kind()) {
            return Err(ApiError::new(
                "dangerous_operation",
                format!(
                    "tool '{alias}' aliases a dangerous operation ('{}' step '{}'); it cannot be run through a toolset",
                    step.kind().as_str(),
                    step.id()
                ),
            ));
        }
    }

    let defaults: Vec<(String, String)> = tool
        .defaults()
        .iter()
        .map(|(key, value)| (key.clone(), value.clone()))
        .collect();
    action.apply_defaults(&defaults);
    let run = host
        .run_action(&action, &inputs)
        .map_err(|error| io_error("toolset execution failed", error))?;
    Ok(JsonValue::object([
        ("action".to_owned(), JsonValue::from(action.name())),
        ("run".to_owned(), run),
        ("tool".to_owned(), JsonValue::from(alias.as_str())),
    ]))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::action::{Action, ActionStep, StepKind};

    const ACTIONS: &str = "ACTION\tnightly\tNightly staging\n\
STEP\tload\timport\n\
PARAM\tsource\tin.pdf\n\
STEP\tsave\texport\n\
PARAM\ttarget\tout.pdf\n\
\n\
ACTION\tdanger\tUnsafe\n\
STEP\tsec\tsecurity\n\
PARAM\tpassword\tsecret\n";

    fn toolset_with(alias: &str, file: &str, action: &str) -> String {
        format!(
            "{{\"version\":1,\"name\":\"tools\",\"tools\":{{\"{alias}\":{{\"file\":\"{file}\",\"action\":\"{action}\",\"defaults\":{{\"source\":\"default.pdf\"}}}}}}}}"
        )
    }

    fn host() -> MemoryHost {
        MemoryHost::new()
            .with_file("actions.txt", ACTIONS)
            .with_file(
                "tools.json",
                toolset_with("daily", "actions.txt", "nightly"),
            )
    }

    fn request(command: &str, params: JsonValue) -> String {
        Request::new("req-1", command, params).to_json()
    }

    #[test]
    fn request_and_response_round_trip() {
        let engine = ApiEngine::new();
        let line = request("version", JsonValue::Object(BTreeMap::new()));
        let parsed = engine.parse_request(&line).expect("valid request");
        assert_eq!(parsed.id, "req-1");
        assert_eq!(parsed.command, "version");
        assert_eq!(parsed.params, JsonValue::Object(BTreeMap::new()));

        let response_line = engine.handle_line(&line, &host());
        let response = Response::parse(&response_line).expect("valid response");
        assert_eq!(response.id, "req-1");
        assert!(response.ok);
        assert_eq!(response.version, API_VERSION);
        assert_eq!(
            response
                .result
                .as_ref()
                .and_then(|value| value.get("engine").cloned()),
            Some(JsonValue::from("pdfxi-automation"))
        );
        assert_eq!(
            Response::parse(&response.to_json()).expect("canonical"),
            response
        );
    }

    #[test]
    fn unknown_command_returns_a_typed_error() {
        let engine = ApiEngine::new();
        let line = request("teleport", JsonValue::Object(BTreeMap::new()));
        let response = Response::parse(&engine.handle_line(&line, &host())).expect("response");
        assert!(!response.ok);
        assert_eq!(response.result, None);
        let error = response.error.expect("error");
        assert_eq!(error.code(), "unknown_command");
        assert!(error.message().contains("teleport"));
        assert!(error.message().contains("run-batch"));
    }

    #[test]
    fn malformed_json_is_robust() {
        let engine = ApiEngine::new();
        for line in [
            "not json",
            "{",
            "[]",
            "{}",
            "{\"id\":\"x\",\"command\":\"version\",\"version\":1,\"params\":[]}",
            "{\"id\":\"x\",\"command\":\"\",\"version\":1}",
            "{\"id\":\"x\",\"command\":\"version\",\"version\":1} trailing",
        ] {
            let response =
                Response::parse(&engine.handle_line(line, &host())).expect("one response line");
            assert!(!response.ok, "must fail for {line:?}");
            assert_eq!(response.id, "", "unparseable lines have no correlation id");
        }
    }

    #[test]
    fn oversize_line_is_rejected_before_parsing() {
        let engine = ApiEngine::new();
        let oversized = "x".repeat(MAX_LINE_BYTES + 1);
        let response = Response::parse(&engine.handle_line(&oversized, &host())).expect("response");
        assert!(!response.ok);
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("line_too_long".to_owned())
        );
    }

    #[test]
    fn version_mismatch_is_rejected() {
        let engine = ApiEngine::with_version(2);
        let line = request("version", JsonValue::Object(BTreeMap::new()));
        let response = Response::parse(&engine.handle_line(&line, &host())).expect("response");
        assert!(!response.ok);
        let error = response.error.expect("error");
        assert_eq!(error.code(), "version_mismatch");
        assert!(error.message().contains("expected 2"));
    }

    #[test]
    fn dry_run_validates_without_executing() {
        let engine = ApiEngine::new();
        let params = JsonValue::object([
            ("file".to_owned(), JsonValue::from("actions.txt")),
            ("action".to_owned(), JsonValue::from("nightly")),
        ]);
        let response = Response::parse(&engine.handle_line(&request("dry-run", params), &host()))
            .expect("response");
        assert!(response.ok);
        let result = response.result.expect("result");
        assert_eq!(result.get("steps").and_then(JsonValue::as_u64), Some(2));

        let missing = JsonValue::object([
            ("file".to_owned(), JsonValue::from("actions.txt")),
            ("action".to_owned(), JsonValue::from("missing")),
        ]);
        let response = Response::parse(&engine.handle_line(&request("dry-run", missing), &host()))
            .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("action_not_found".to_owned())
        );
    }

    #[test]
    fn list_and_show_actions_use_saved_files() {
        let engine = ApiEngine::new();
        let params = JsonValue::object([("file".to_owned(), JsonValue::from("actions.txt"))]);
        let response =
            Response::parse(&engine.handle_line(&request("list-actions", params), &host()))
                .expect("response");
        assert!(response.ok);
        let actions = response
            .result
            .and_then(|value| value.get("actions").cloned())
            .and_then(|value| value.as_array().map(<[_]>::to_vec))
            .expect("actions array");
        assert_eq!(actions.len(), 2);
        assert_eq!(
            actions[0].get("name").and_then(JsonValue::as_str),
            Some("nightly")
        );

        let params = JsonValue::object([
            ("file".to_owned(), JsonValue::from("actions.txt")),
            ("action".to_owned(), JsonValue::from("nightly")),
        ]);
        let response =
            Response::parse(&engine.handle_line(&request("show-action", params), &host()))
                .expect("response");
        assert_eq!(
            response
                .result
                .and_then(|value| value.get("name").cloned())
                .and_then(|value| value.as_str().map(str::to_owned)),
            Some("nightly".to_owned())
        );
    }

    #[test]
    fn run_batch_delegates_to_the_host() {
        let engine = ApiEngine::new();
        let params = JsonValue::object([
            ("file".to_owned(), JsonValue::from("actions.txt")),
            ("action".to_owned(), JsonValue::from("nightly")),
            (
                "inputs".to_owned(),
                JsonValue::from(vec!["a.pdf".to_owned(), "b.pdf".to_owned()]),
            ),
        ]);
        let response = Response::parse(&engine.handle_line(&request("run-batch", params), &host()))
            .expect("response");
        assert!(response.ok);
        let run = response
            .result
            .and_then(|value| value.get("run").cloned())
            .expect("run result");
        let jobs = run
            .get("jobs")
            .and_then(JsonValue::as_array)
            .expect("jobs array");
        assert_eq!(jobs.len(), 2);
    }

    #[test]
    fn toolset_parse_validate_list_and_run() {
        let engine = ApiEngine::new();
        let params = JsonValue::object([("file".to_owned(), JsonValue::from("tools.json"))]);
        let response = Response::parse(
            &engine.handle_line(&request("list-toolsets", params.clone()), &host()),
        )
        .expect("list response");
        assert!(response.ok);
        let tools = response
            .result
            .and_then(|value| value.get("tools").cloned())
            .and_then(|value| value.as_array().map(<[_]>::to_vec))
            .expect("tools array");
        assert_eq!(tools.len(), 1);
        assert_eq!(
            tools[0].get("alias").and_then(JsonValue::as_str),
            Some("daily")
        );

        let response =
            Response::parse(&engine.handle_line(&request("validate-toolset", params), &host()))
                .expect("validate response");
        assert!(response.ok);

        let params = JsonValue::object([
            ("file".to_owned(), JsonValue::from("tools.json")),
            ("tool".to_owned(), JsonValue::from("daily")),
            (
                "inputs".to_owned(),
                JsonValue::from(vec!["a.pdf".to_owned()]),
            ),
        ]);
        let response =
            Response::parse(&engine.handle_line(&request("run-toolset", params), &host()))
                .expect("run response");
        assert!(response.ok, "{:?}", response.error);
        let result = response.result.expect("result");
        assert_eq!(
            result.get("tool").and_then(JsonValue::as_str),
            Some("daily")
        );
        let run = result.get("run").expect("run");
        assert_eq!(run.get("steps").and_then(JsonValue::as_u64), Some(2));
    }

    #[test]
    fn alias_rejection_covers_unknown_dangerous_and_wrapped_operations() {
        let engine = ApiEngine::new();
        let unknown = JsonValue::object([
            ("file".to_owned(), JsonValue::from("tools.json")),
            ("tool".to_owned(), JsonValue::from("no-such-tool")),
            (
                "inputs".to_owned(),
                JsonValue::from(vec!["a.pdf".to_owned()]),
            ),
        ]);
        let response =
            Response::parse(&engine.handle_line(&request("run-toolset", unknown), &host()))
                .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("unknown_tool".to_owned())
        );

        let dangerous_name = toolset_with("shell", "actions.txt", "nightly");
        let dangerous_host = host().with_file("dangerous.json", dangerous_name);
        let params = JsonValue::object([("file".to_owned(), JsonValue::from("dangerous.json"))]);
        let response = Response::parse(
            &engine.handle_line(&request("validate-toolset", params), &dangerous_host),
        )
        .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("dangerous_alias".to_owned())
        );

        let wrapped = toolset_with("daily", "actions.txt", "danger");
        let wrapped_host = host().with_file("wrapped.json", wrapped);
        let params = JsonValue::object([
            ("file".to_owned(), JsonValue::from("wrapped.json")),
            ("tool".to_owned(), JsonValue::from("daily")),
            (
                "inputs".to_owned(),
                JsonValue::from(vec!["a.pdf".to_owned()]),
            ),
        ]);
        let response =
            Response::parse(&engine.handle_line(&request("run-toolset", params), &wrapped_host))
                .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("dangerous_operation".to_owned())
        );
    }

    #[test]
    fn output_is_deterministic_and_keys_are_sorted() {
        let engine = ApiEngine::new();
        let line = request("version", JsonValue::Object(BTreeMap::new()));
        let first = engine.handle_line(&line, &host());
        let second = engine.handle_line(&line, &host());
        assert_eq!(first, second);
        assert_eq!(
            first,
            "{\"error\":null,\"id\":\"req-1\",\"ok\":true,\"result\":{\"engine\":\"pdfxi-automation\",\"version\":1},\"version\":1}"
        );
        let failure = engine.handle_line("not json", &host());
        assert_eq!(failure, engine.handle_line("not json", &host()));
        assert!(failure.starts_with("{\"error\":{\"code\":\"invalid_json\","));
    }

    #[test]
    fn defaults_are_applied_only_to_missing_step_parameters() {
        let mut action = Action::from_steps(
            "nightly",
            "",
            [
                ActionStep::new("load", StepKind::Import),
                ActionStep::new("save", StepKind::Export).with_parameter("source", "explicit.pdf"),
            ],
        )
        .expect("valid action");
        action.apply_defaults(&[("source".to_owned(), "default.pdf".to_owned())]);
        assert_eq!(
            action.step("load").and_then(|s| s.parameter("source")),
            Some("default.pdf")
        );
        assert_eq!(
            action.step("save").and_then(|s| s.parameter("source")),
            Some("explicit.pdf")
        );
    }

    #[test]
    fn host_and_parameter_errors_are_typed() {
        let engine = ApiEngine::new();
        let missing_file = JsonValue::object([("file".to_owned(), JsonValue::from("missing.txt"))]);
        let response =
            Response::parse(&engine.handle_line(&request("list-actions", missing_file), &host()))
                .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("host_error".to_owned())
        );

        let no_params = JsonValue::Object(BTreeMap::new());
        let response =
            Response::parse(&engine.handle_line(&request("run-batch", no_params), &host()))
                .expect("response");
        assert_eq!(
            response.error.map(|error| error.code().to_owned()),
            Some("invalid_params".to_owned())
        );
    }
}
