//! Typed request and response shapes pinned to OpenCode 2.3.2.
//!
//! The shapes follow the live transcript in ADR 0011. Unknown fields are
//! ignored so a minor service release does not break parsing, while a missing
//! required field produces [`crate::AssistantError::Shape`] instead of a
//! silent default.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AssistantError;

/// Envelope used by the 2.3.2 list routes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct ListEnvelope<T> {
    pub(crate) data: Vec<T>,
    #[serde(default)]
    pub(crate) cursor: Option<Cursor>,
}

/// Envelope used by the 2.3.2 single-object routes.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub(crate) struct DataEnvelope<T> {
    pub(crate) data: T,
}

/// Opaque message-page cursor.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct Cursor {
    /// Cursor for the previous page, when the service reports one.
    #[serde(default)]
    pub previous: Option<String>,
    /// Cursor for the next page, when the service reports one.
    #[serde(default)]
    pub next: Option<String>,
}

/// A `provider/id` model reference as it appears on the wire.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelRef {
    /// Provider id, for example `opencode-go`.
    #[serde(rename = "providerID")]
    pub provider_id: String,
    /// Model id, for example `hy4-flash-free`.
    pub id: String,
    /// Variant id. The service fills `default` when it is omitted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub variant: Option<String>,
}

impl ModelRef {
    /// Builds a reference from a provider and model id.
    pub fn new(provider_id: impl Into<String>, id: impl Into<String>) -> Self {
        Self {
            provider_id: provider_id.into(),
            id: id.into(),
            variant: None,
        }
    }

    /// Sets the variant.
    pub fn with_variant(mut self, variant: impl Into<String>) -> Self {
        self.variant = Some(variant.into());
        self
    }

    /// The `provider/id` label used in the UI.
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider_id, self.id)
    }
}

/// A model variant, for example `none` or `high`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelVariant {
    /// Variant id.
    pub id: String,
    /// Optional human-readable name.
    #[serde(default)]
    pub name: Option<String>,
}

/// What a model accepts and can do.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelCapabilities {
    /// Whether the model can call tools.
    #[serde(default)]
    pub tools: bool,
    /// Accepted input kinds, for example `text` and `image`.
    #[serde(default)]
    pub input: Vec<String>,
    /// Produced output kinds.
    #[serde(default)]
    pub output: Vec<String>,
}

/// Token limits for a model.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct ModelLimit {
    /// Total context window in tokens.
    #[serde(default)]
    pub context: u64,
    /// Maximum input tokens, when the service reports one.
    #[serde(default)]
    pub input: Option<u64>,
    /// Maximum output tokens, when the service reports one.
    #[serde(default)]
    pub output: Option<u64>,
}

/// Cache pricing for a model.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct CacheCost {
    /// Price per million cached input tokens read.
    #[serde(default)]
    pub read: f64,
    /// Price per million tokens written to the cache.
    #[serde(default)]
    pub write: f64,
}

/// One pricing tier, in USD per million tokens.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq)]
pub struct CostTier {
    /// Input price.
    #[serde(default)]
    pub input: f64,
    /// Output price.
    #[serde(default)]
    pub output: f64,
    /// Cache pricing, when reported.
    #[serde(default)]
    pub cache: Option<CacheCost>,
}

/// One entry of `GET /api/model`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Model {
    /// Model id.
    pub id: String,
    /// Model id as reported by the provider, when it differs.
    #[serde(default, rename = "modelID")]
    pub model_id: Option<String>,
    /// Owning provider id.
    #[serde(rename = "providerID")]
    pub provider_id: String,
    /// Model kind, normally `chat`.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
    /// Human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Lifecycle status, for example `active`.
    #[serde(default)]
    pub status: Option<String>,
    /// Whether the user has enabled the model.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Selectable variants.
    #[serde(default)]
    pub variants: Vec<ModelVariant>,
    /// Capabilities, when reported.
    #[serde(default)]
    pub capabilities: Option<ModelCapabilities>,
    /// Limits, when reported.
    #[serde(default)]
    pub limit: Option<ModelLimit>,
    /// Pricing tiers.
    #[serde(default)]
    pub cost: Vec<CostTier>,
}

impl Model {
    /// The `provider/id` label used in the UI and the acceptance test.
    pub fn label(&self) -> String {
        format!("{}/{}", self.provider_id, self.id)
    }

    /// A reference suitable for session creation or a model switch.
    pub fn reference(&self, variant: Option<&str>) -> ModelRef {
        ModelRef {
            provider_id: self.provider_id.clone(),
            id: self.id.clone(),
            variant: variant.map(str::to_owned),
        }
    }

    /// Whether the model is enabled. An absent flag counts as enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// Total context window in tokens.
    pub fn context_limit(&self) -> Option<u64> {
        self.limit.as_ref().map(|limit| limit.context)
    }

    /// Whether the model advertises tool support.
    pub fn supports_tools(&self) -> bool {
        self.capabilities.as_ref().is_some_and(|caps| caps.tools)
    }

    /// Variant ids in the order the service reports them.
    pub fn variant_ids(&self) -> Vec<&str> {
        self.variants
            .iter()
            .map(|variant| variant.id.as_str())
            .collect()
    }
}

/// One entry of `GET /api/provider`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Provider {
    /// Provider id.
    pub id: String,
    /// Human-readable name.
    #[serde(default)]
    pub name: Option<String>,
    /// Whether the user has enabled the provider.
    #[serde(default)]
    pub enabled: Option<bool>,
    /// Provider kind, when reported.
    #[serde(default, rename = "type")]
    pub kind: Option<String>,
}

impl Provider {
    /// Whether the provider is enabled. An absent flag counts as enabled.
    pub fn is_enabled(&self) -> bool {
        self.enabled.unwrap_or(true)
    }

    /// The name when present, otherwise the id.
    pub fn label(&self) -> &str {
        self.name.as_deref().unwrap_or(&self.id)
    }
}

/// `GET /api/info`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct ServiceInfo {
    /// Service version, for example `2.3.2`.
    pub version: String,
    /// Process id of the service, when reported.
    #[serde(default)]
    pub pid: Option<u32>,
    /// Hostname, when reported.
    #[serde(default)]
    pub hostname: Option<String>,
    /// Advertised listener URLs, including any tunnel URL.
    #[serde(default)]
    pub urls: Vec<String>,
    /// Service paths, when reported.
    #[serde(default)]
    pub paths: Option<Value>,
    /// Capability flags, when reported.
    #[serde(default)]
    pub capabilities: Option<Value>,
}

impl ServiceInfo {
    /// Whether the version is inside the pinned `2.3.x` range.
    pub fn is_compatible(&self) -> bool {
        self.version.starts_with("2.3.")
    }
}

/// Directory a session is rooted in.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct Location {
    /// Absolute directory path.
    pub directory: String,
}

/// Session timestamps in milliseconds since the Unix epoch.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct SessionTime {
    /// Creation time.
    #[serde(default)]
    pub created: u64,
    /// Last update time.
    #[serde(default)]
    pub updated: u64,
}

/// A created session.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct Session {
    /// Session id, matching `^ses`.
    pub id: String,
    /// Owning project id, when reported.
    #[serde(default, rename = "projectID")]
    pub project_id: Option<String>,
    /// Selected model.
    #[serde(default)]
    pub model: Option<ModelRef>,
    /// Accumulated cost.
    #[serde(default)]
    pub cost: f64,
    /// Accumulated token usage.
    #[serde(default)]
    pub tokens: Option<TokenUsage>,
    /// Timestamps.
    #[serde(default)]
    pub time: Option<SessionTime>,
    /// Session title.
    #[serde(default)]
    pub title: Option<String>,
    /// Session location.
    #[serde(default)]
    pub location: Option<Location>,
}

/// Body of `POST /api/session`.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct SessionRequest {
    /// Session title shown in OpenCode.
    pub title: String,
    /// Model the session starts with.
    pub model: ModelRef,
    /// Directory the session is rooted in.
    pub location: Location,
}

impl SessionRequest {
    /// Builds a request, rejecting a relative directory.
    pub fn new(
        title: impl Into<String>,
        model: ModelRef,
        directory: impl Into<PathBuf>,
    ) -> Result<Self, AssistantError> {
        let directory = directory.into();
        if !directory.is_absolute() {
            return Err(AssistantError::InvalidArgument {
                message: format!("session directory {} is not absolute", directory.display()),
            });
        }
        Ok(Self {
            title: title.into(),
            model,
            location: Location {
                directory: directory.to_string_lossy().into_owned(),
            },
        })
    }
}

/// Timestamps on a message, in milliseconds since the Unix epoch.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct MessageTime {
    /// Creation time.
    #[serde(default)]
    pub created: u64,
    /// First streamed token.
    #[serde(default, rename = "firstToken")]
    pub first_token: Option<u64>,
    /// Last streamed token.
    #[serde(default)]
    pub streamed: Option<u64>,
    /// Completion time.
    #[serde(default)]
    pub completed: Option<u64>,
}

/// Text payload of a user message.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct UserPayload {
    /// Prompt text.
    #[serde(default)]
    pub text: String,
}

/// A user message.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct UserMessage {
    /// Message id.
    pub id: String,
    /// Owning session id.
    #[serde(default, rename = "sessionID")]
    pub session_id: Option<String>,
    /// Timestamps.
    #[serde(default)]
    pub time: MessageTime,
    /// Prompt payload.
    #[serde(default)]
    pub payload: Option<UserPayload>,
    /// Delivery mode, for example `steer`.
    #[serde(default)]
    pub delivery: Option<String>,
}

impl UserMessage {
    /// Creation time in milliseconds.
    pub fn created(&self) -> u64 {
        self.time.created
    }

    /// Prompt text when present.
    pub fn text(&self) -> Option<&str> {
        self.payload.as_ref().map(|payload| payload.text.as_str())
    }
}

/// One content item of an assistant message.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum ContentPart {
    /// Answer text.
    Text(TextPart),
    /// Model reasoning. Never rendered as the answer.
    Reasoning(TextPart),
    /// Any other content kind reported by a newer service.
    #[serde(other)]
    Other,
}

impl ContentPart {
    /// The text of a text or reasoning part.
    pub fn text(&self) -> Option<&str> {
        match self {
            Self::Text(part) | Self::Reasoning(part) => Some(part.text.as_str()),
            Self::Other => None,
        }
    }
}

/// Text carried by a content item.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TextPart {
    /// The text.
    #[serde(default)]
    pub text: String,
}

/// An assistant message.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
pub struct AssistantMessage {
    /// Message id.
    pub id: String,
    /// Timestamps.
    #[serde(default)]
    pub time: MessageTime,
    /// Agent that produced the message.
    #[serde(default)]
    pub agent: Option<String>,
    /// Model that produced the message.
    #[serde(default)]
    pub model: Option<ModelRef>,
    /// Content items.
    #[serde(default)]
    pub content: Vec<ContentPart>,
    /// Finish reason, for example `stop`.
    #[serde(default)]
    pub finish: Option<String>,
    /// Provider-specific finish reason.
    #[serde(default, rename = "rawFinish")]
    pub raw_finish: Option<String>,
    /// Cost of the message.
    #[serde(default)]
    pub cost: f64,
    /// Token usage.
    #[serde(default)]
    pub tokens: TokenUsage,
}

impl AssistantMessage {
    /// The answer text: `text` content parts joined by newlines, reasoning
    /// excluded.
    pub fn text(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Text(part) => Some(part.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// The reasoning text, kept separate from the answer.
    pub fn reasoning(&self) -> String {
        self.content
            .iter()
            .filter_map(|part| match part {
                ContentPart::Reasoning(part) => Some(part.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Creation time in milliseconds.
    pub fn created(&self) -> u64 {
        self.time.created
    }

    /// Whether the message reports a completion time.
    pub fn is_finished(&self) -> bool {
        self.time.completed.is_some()
    }
}

/// Terminal idle item that follows a turn.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq, Eq)]
pub struct IdleMessage {
    /// Message id.
    pub id: String,
    /// Timestamps.
    #[serde(default)]
    pub time: MessageTime,
    /// Turn outcome, for example `succeeded`.
    #[serde(default)]
    pub outcome: Option<String>,
}

impl IdleMessage {
    /// Creation time in milliseconds.
    pub fn created(&self) -> u64 {
        self.time.created
    }

    /// Whether the turn succeeded.
    pub fn succeeded(&self) -> bool {
        self.outcome.as_deref() == Some("succeeded")
    }

    /// The outcome, or `unknown`.
    pub fn outcome(&self) -> &str {
        self.outcome.as_deref().unwrap_or("unknown")
    }
}

/// One item of `GET /api/session/{id}/message`.
#[derive(Debug, Clone, Deserialize, Serialize, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum MessageItem {
    /// A user message.
    User(UserMessage),
    /// An assistant message.
    Assistant(AssistantMessage),
    /// The terminal idle item.
    Idle(IdleMessage),
    /// Any other item type reported by a newer service.
    #[serde(other)]
    Other,
}

/// Token usage reported by the service.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct TokenUsage {
    /// Input tokens.
    #[serde(default)]
    pub input: u64,
    /// Output tokens.
    #[serde(default)]
    pub output: u64,
    /// Reasoning tokens.
    #[serde(default)]
    pub reasoning: u64,
    /// Cache usage.
    #[serde(default)]
    pub cache: CacheUsage,
}

/// Cache token counters.
#[derive(Debug, Clone, Default, Deserialize, Serialize, PartialEq, Eq)]
pub struct CacheUsage {
    /// Cache tokens read.
    #[serde(default)]
    pub read: u64,
    /// Cache tokens written.
    #[serde(default)]
    pub write: u64,
}

/// A page of messages, newest first as the service returns them.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct MessagePage {
    /// Items in service order.
    pub items: Vec<MessageItem>,
    /// Page cursor, when the service reports one.
    pub cursor: Option<Cursor>,
}

impl MessagePage {
    /// The newest idle item created at or after `created`.
    pub fn idle_since(&self, created: u64) -> Option<&IdleMessage> {
        self.items.iter().find_map(|item| match item {
            MessageItem::Idle(idle) if idle.created() >= created => Some(idle),
            _ => None,
        })
    }

    /// The newest assistant message created at or after `created`.
    pub fn assistant_since(&self, created: u64) -> Option<&AssistantMessage> {
        self.items.iter().find_map(|item| match item {
            MessageItem::Assistant(message) if message.created() >= created => Some(message),
            _ => None,
        })
    }
}

/// Result of `POST /api/session/{id}/prompt`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PromptAccepted {
    /// Id of the accepted user message.
    pub id: String,
    /// Owning session id, when reported.
    pub session_id: Option<String>,
    /// Creation time in milliseconds.
    pub created: u64,
    /// Text the service accepted.
    pub text: Option<String>,
}

impl From<UserMessage> for PromptAccepted {
    fn from(message: UserMessage) -> Self {
        let created = message.created();
        let text = message.text().map(str::to_owned);
        Self {
            id: message.id,
            session_id: message.session_id,
            created,
            text,
        }
    }
}

/// A completed assistant turn.
#[derive(Debug, Clone, PartialEq)]
pub struct Answer {
    /// Session the answer belongs to.
    pub session_id: String,
    /// Assistant message id.
    pub message_id: String,
    /// Answer text, reasoning excluded.
    pub text: String,
    /// Token usage of the message.
    pub usage: TokenUsage,
    /// Finish reason, for example `stop`.
    pub finish: Option<String>,
    /// Cost of the message.
    pub cost: f64,
    /// Number of message polls it took.
    pub polls: u32,
    /// Wall-clock time from prompt to answer.
    pub elapsed: std::time::Duration,
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const MODEL_JSON: &str = r#"{
        "id":"hy4-flash-free","modelID":"hy4-flash-free","providerID":"opencode-go",
        "type":"chat","name":"Hy4 flash Free","status":"active","enabled":true,
        "variants":[{"id":"none"},{"id":"high"}],
        "capabilities":{"tools":true,"input":["text","image"],"output":["text"]},
        "limit":{"context":1048576,"input":983040,"output":65536},
        "cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}],
        "unknownField":{"ignored":true}
    }"#;

    #[test]
    fn parses_the_pinned_model_shape() {
        let model: Model = serde_json::from_str(MODEL_JSON).expect("model");
        assert_eq!(model.label(), "opencode-go/hy4-flash-free");
        assert_eq!(model.name.as_deref(), Some("Hy4 flash Free"));
        assert!(model.is_enabled());
        assert_eq!(model.context_limit(), Some(1_048_576));
        assert_eq!(model.variant_ids(), vec!["none", "high"]);
        assert!(model.supports_tools());
        assert_eq!(model.cost.len(), 1);
        assert_eq!(model.cost[0].input, 0.0);
        assert_eq!(
            model.cost[0].cache.as_ref().map(|cache| cache.read),
            Some(0.0)
        );
        let reference = model.reference(Some("high"));
        assert_eq!(
            serde_json::to_value(&reference).expect("reference"),
            json!({"providerID":"opencode-go","id":"hy4-flash-free","variant":"high"})
        );
    }

    #[test]
    fn model_without_variant_omits_it_on_the_wire() {
        let reference = ModelRef::new("opencode-go", "hy4-flash-free");
        assert_eq!(
            serde_json::to_value(&reference).expect("reference"),
            json!({"providerID":"opencode-go","id":"hy4-flash-free"})
        );
    }

    #[test]
    fn model_missing_provider_is_a_shape_error() {
        let error = serde_json::from_str::<Model>(r#"{"id":"x"}"#).expect_err("missing provider");
        assert!(error.to_string().contains("providerID"));
    }

    #[test]
    fn parses_assistant_message_and_extracts_only_answer_text() {
        let raw = r#"{
            "id":"msg_1","type":"assistant",
            "time":{"created":1795210777670,"firstToken":1795210780260,
                    "streamed":1795210780683,"completed":1795210780751},
            "agent":"orchestrator",
            "model":{"id":"hy4-flash-free","providerID":"opencode-go","variant":"default"},
            "content":[{"type":"reasoning","text":"think"},{"type":"text","text":"pong"}],
            "finish":"stop","rawFinish":"stop","cost":0,
            "tokens":{"input":8896,"output":4,"reasoning":26,"cache":{"read":2048,"write":0}}
        }"#;
        let item: MessageItem = serde_json::from_str(raw).expect("message");
        let MessageItem::Assistant(message) = item else {
            panic!("expected an assistant message");
        };
        assert_eq!(message.text(), "pong");
        assert_eq!(message.reasoning(), "think");
        assert_eq!(message.finish.as_deref(), Some("stop"));
        assert!(message.is_finished());
        assert_eq!(message.tokens.input, 8896);
        assert_eq!(message.tokens.cache.read, 2048);
        assert_eq!(message.created(), 1795210777670);
    }

    #[test]
    fn parses_idle_item_and_outcome() {
        let raw = r#"{"id":"msg_2","time":{"created":1795210780755},"type":"idle","outcome":"succeeded"}"#;
        let item: MessageItem = serde_json::from_str(raw).expect("idle");
        let MessageItem::Idle(idle) = item else {
            panic!("expected an idle item");
        };
        assert!(idle.succeeded());
        assert_eq!(idle.outcome(), "succeeded");
        assert_eq!(idle.created(), 1795210780755);
    }

    #[test]
    fn unknown_message_type_is_ignored() {
        let item: MessageItem =
            serde_json::from_str(r#"{"type":"permission","id":"msg_3"}"#).expect("unknown");
        assert!(matches!(item, MessageItem::Other));
    }

    #[test]
    fn page_finds_newest_matching_items() {
        let page = MessagePage {
            items: vec![
                serde_json::from_str(
                    r#"{"id":"msg_2","time":{"created":30},"type":"idle","outcome":"succeeded"}"#,
                )
                .expect("idle"),
                serde_json::from_str(
                    r#"{"id":"msg_1","time":{"created":20},"type":"assistant","content":[{"type":"text","text":"pong"}]}"#,
                )
                .expect("assistant"),
                serde_json::from_str(
                    r#"{"id":"msg_0","time":{"created":10},"type":"user","payload":{"text":"hi"}}"#,
                )
                .expect("user"),
            ],
            cursor: None,
        };
        assert!(page.idle_since(15).is_some_and(IdleMessage::succeeded));
        assert_eq!(
            page.assistant_since(15).map(AssistantMessage::text),
            Some("pong".to_owned())
        );
        assert!(page.assistant_since(25).is_none());
    }

    #[test]
    fn session_request_requires_an_absolute_directory() {
        let model = ModelRef::new("opencode-go", "hy4-flash-free");
        let error = SessionRequest::new("t", model.clone(), "relative/dir").expect_err("relative");
        assert!(matches!(error, AssistantError::InvalidArgument { .. }));
        let request = SessionRequest::new("t", model, std::env::temp_dir()).expect("absolute");
        assert_eq!(
            serde_json::to_value(&request).expect("request"),
            json!({
                "title":"t",
                "model":{"providerID":"opencode-go","id":"hy4-flash-free"},
                "location":{"directory": request.location.directory}
            })
        );
    }

    #[test]
    fn service_info_compatibility_is_pinned_to_2_3() {
        let info = ServiceInfo {
            version: "2.3.2".to_owned(),
            pid: Some(17164),
            hostname: None,
            urls: vec!["http://127.0.0.1:28894".to_owned()],
            paths: None,
            capabilities: None,
        };
        assert!(info.is_compatible());
        let newer = ServiceInfo {
            version: "2.4.0".to_owned(),
            ..info
        };
        assert!(!newer.is_compatible());
    }
}
