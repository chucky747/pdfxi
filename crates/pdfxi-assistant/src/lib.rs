//! Client for the user's local OpenCode service, per ADR 0011
//! (`docs/decisions/0011-ai-assistant-through-opencode.md`).
//!
//! PDFXI does not host a model backend. The assistant talks to the OpenCode
//! service the user already runs on this machine: it reads the service
//! registration, authenticates with HTTP Basic, lists providers and models,
//! creates a session, prompts, switches models and consumes the event stream.
//! Routes are pinned to OpenCode 2.3.2 by the T-0250 spike; the service password
//! is read at runtime, held in memory, and never logged or stored by this crate.
//!
//! # Security notes
//!
//! - Discovery accepts a registration file only when it points at loopback. A
//!   remote endpoint must be supplied explicitly, and then must use `https`.
//! - The `Authorization` header is built once per client and never formatted.
//!   [`ServiceEndpoint`], [`Assistant`] and the error type redact it.
//! - Server error bodies are capped at [`BODY_SNIPPET_LIMIT`] characters and
//!   passed through [`redact`] before they reach an error value.
//! - Requests never follow redirects, so a redirect cannot replay the
//!   credential.
//! - Every call takes a [`CancelFlag`] and every transport phase has a timeout.
//!
//! # Example
//!
//! ```no_run
//! use pdfxi_assistant::{Assistant, CancelFlag, SessionRequest};
//!
//! # fn run() -> Result<(), pdfxi_assistant::AssistantError> {
//! let assistant = Assistant::discover()?;
//! let cancel = CancelFlag::new();
//! let info = assistant.check_compatible(&cancel)?;
//! let models = assistant.models(&cancel)?;
//! let model = models
//!     .iter()
//!     .find(|model| model.label() == "opencode-go/hy4-flash-free")
//!     .ok_or_else(|| pdfxi_assistant::AssistantError::InvalidArgument {
//!         message: "acceptance model is not installed".to_owned(),
//!     })?;
//! let request = SessionRequest::new(
//!     "PDFXI assistant",
//!     model.reference(None),
//!     std::env::temp_dir(),
//! )?;
//! let session = assistant.create_session(&request, &cancel)?;
//! let answer = assistant.ask(&session.id, "Summarize page 1", &cancel)?;
//! println!("{} ({} polls, service {})", answer.text, answer.polls, info.version);
//! # Ok(())
//! # }
//! ```

#![deny(missing_docs)]
#![forbid(unsafe_code)]

mod cancel;
mod client;
mod config;
mod endpoint;
mod error;
mod events;
mod http;
mod lockdown;
mod mcp;
mod models;
mod secret;

pub use cancel::CancelFlag;
pub use client::{Assistant, PINNED_SERVICE_VERSION, PINNED_SERVICE_VERSION_PREFIX};
pub use config::{ClientConfig, Timeouts};
pub use endpoint::{EndpointSource, ServiceEndpoint, default_registration_path};
pub use error::{AssistantError, BODY_SNIPPET_LIMIT, redact};
pub use events::{Event, EventStream};
pub use lockdown::{
    ASSISTANT_MCP_NAME, BudgetedTools, GeneratedText, LOCKED_TOOL_NAMES, LockedSession,
    MAX_DOCUMENT_TEXT_CHARS_PER_CALL, MAX_DOCUMENT_TEXT_CHARS_PER_CONVERSATION, OneShotRequest,
    PermissionDecision, PermissionEffect, PermissionRule, PermissionRuleset, TextBudget,
    UNTRUSTED_TEXT_CLOSE, UNTRUSTED_TEXT_OPEN, fence_document_text, locked_actions, locked_ruleset,
    read_only_tool_definitions, verify_locked_ruleset,
};
pub use mcp::{
    MAX_TOOL_TEXT_CHARS, MCP_PROTOCOL_VERSION, MCP_SERVER_NAME, McpOptions, McpReport, McpSession,
    McpTransport, ToolProvider, tool_definitions,
};
pub use models::{
    Answer, AssistantMessage, CacheCost, CacheUsage, ContentPart, CostTier, Cursor, IdleMessage,
    Location, MessageItem, MessagePage, MessageTime, Model, ModelCapabilities, ModelLimit,
    ModelRef, ModelVariant, PromptAccepted, Provider, ServiceInfo, Session, SessionRequest,
    SessionTime, TextPart, TokenUsage, ToolPart, UserMessage, UserPayload,
};
