//! The high-level OpenCode assistant client.
//!
//! Every route is pinned to the shapes recorded in ADR 0011 for OpenCode
//! 2.3.2. One [`Assistant`] holds one endpoint and one credential; it performs
//! no I/O until a method is called.

use std::fmt;
use std::time::Instant;

use serde::Serialize;

use crate::cancel::CancelFlag;
use crate::config::ClientConfig;
use crate::endpoint::ServiceEndpoint;
use crate::error::AssistantError;
use crate::events::EventStream;
use crate::http::Http;
use crate::mcp::{self, McpOptions, McpReport, McpSession, ToolProvider};
use crate::models::{
    Answer, DataEnvelope, ListEnvelope, MessageItem, MessagePage, Model, ModelRef, PromptAccepted,
    Provider, ServiceInfo, Session, SessionRequest,
};

/// Service version the routes in this crate were verified against.
pub const PINNED_SERVICE_VERSION: &str = "2.3.2";

/// Version prefix accepted as compatible.
pub const PINNED_SERVICE_VERSION_PREFIX: &str = "2.3.";

const INFO_ROUTE: &str = "/api/info";
const PROVIDER_ROUTE: &str = "/api/provider";
const MODEL_ROUTE: &str = "/api/model";
const SESSION_ROUTE: &str = "/api/session";
const EVENT_ROUTE: &str = "/api/event";

/// A client for one OpenCode service.
pub struct Assistant {
    endpoint: ServiceEndpoint,
    http: Http,
    config: ClientConfig,
}

impl Assistant {
    /// Builds a client with the default timings.
    pub fn connect(endpoint: ServiceEndpoint) -> Self {
        Self::connect_with(endpoint, ClientConfig::default())
    }

    /// Builds a client with explicit timings.
    pub fn connect_with(endpoint: ServiceEndpoint, config: ClientConfig) -> Self {
        let http = Http::new(&endpoint, &config.timeouts);
        Self {
            endpoint,
            http,
            config,
        }
    }

    /// Discovers the local service and builds a client.
    pub fn discover() -> Result<Self, AssistantError> {
        Ok(Self::connect(ServiceEndpoint::discover()?))
    }

    /// Builds a client for a user-supplied remote service.
    pub fn connect_explicit(url: &str, password: &str) -> Result<Self, AssistantError> {
        Ok(Self::connect(ServiceEndpoint::explicit(url, password)?))
    }

    /// The endpoint this client talks to.
    pub fn endpoint(&self) -> &ServiceEndpoint {
        &self.endpoint
    }

    /// The timings this client uses.
    pub fn config(&self) -> &ClientConfig {
        &self.config
    }

    /// `GET /api/info`.
    pub fn info(&self, cancel: &CancelFlag) -> Result<ServiceInfo, AssistantError> {
        self.http.get_json(INFO_ROUTE, cancel)
    }

    /// `GET /api/info`, rejecting a service outside the pinned `2.3.x` range.
    pub fn check_compatible(&self, cancel: &CancelFlag) -> Result<ServiceInfo, AssistantError> {
        let info = self.info(cancel)?;
        if info.is_compatible() {
            Ok(info)
        } else {
            Err(AssistantError::IncompatibleVersion {
                version: info.version,
            })
        }
    }

    /// `GET /api/provider`.
    pub fn providers(&self, cancel: &CancelFlag) -> Result<Vec<Provider>, AssistantError> {
        let envelope: ListEnvelope<Provider> = self.http.get_json(PROVIDER_ROUTE, cancel)?;
        Ok(envelope.data)
    }

    /// `GET /api/model`.
    pub fn models(&self, cancel: &CancelFlag) -> Result<Vec<Model>, AssistantError> {
        let envelope: ListEnvelope<Model> = self.http.get_json(MODEL_ROUTE, cancel)?;
        Ok(envelope.data)
    }

    /// `POST /api/session`.
    pub fn create_session(
        &self,
        request: &SessionRequest,
        cancel: &CancelFlag,
    ) -> Result<Session, AssistantError> {
        let envelope: DataEnvelope<Session> =
            self.http.post_json(SESSION_ROUTE, request, cancel)?;
        Ok(envelope.data)
    }

    /// `POST /api/session/{id}/prompt`. The route enqueues the turn and returns.
    pub fn prompt(
        &self,
        session_id: &str,
        text: &str,
        cancel: &CancelFlag,
    ) -> Result<PromptAccepted, AssistantError> {
        if text.trim().is_empty() {
            return Err(AssistantError::InvalidArgument {
                message: "the prompt is empty".to_owned(),
            });
        }
        let route = format!("{}/prompt", session_route(session_id)?);
        let envelope: DataEnvelope<MessageItem> =
            self.http.post_json(&route, &PromptBody { text }, cancel)?;
        match envelope.data {
            MessageItem::User(message) => Ok(PromptAccepted::from(message)),
            _ => Err(AssistantError::Shape {
                route,
                message: "expected a user message in data".to_owned(),
            }),
        }
    }

    /// `GET /api/session/{id}/message`, newest first.
    pub fn messages(
        &self,
        session_id: &str,
        cancel: &CancelFlag,
    ) -> Result<MessagePage, AssistantError> {
        let route = format!("{}/message?limit=50&order=desc", session_route(session_id)?);
        let envelope: ListEnvelope<MessageItem> = self.http.get_json(&route, cancel)?;
        Ok(MessagePage {
            items: envelope.data,
            cursor: envelope.cursor,
        })
    }

    /// `POST /api/session/{id}/model`. The route answers 204 with no body.
    pub fn set_model(
        &self,
        session_id: &str,
        model: &ModelRef,
        cancel: &CancelFlag,
    ) -> Result<(), AssistantError> {
        let route = format!("{}/model", session_route(session_id)?);
        self.http.post_empty(&route, &ModelBody { model }, cancel)
    }

    /// Sends one prompt and waits for the assistant's answer.
    ///
    /// Polls `GET /api/session/{id}/message` every
    /// [`ClientConfig::poll_interval`] until the turn's idle item arrives or
    /// [`ClientConfig::max_wait`] expires. Reasoning content is never returned
    /// as the answer.
    pub fn ask(
        &self,
        session_id: &str,
        text: &str,
        cancel: &CancelFlag,
    ) -> Result<Answer, AssistantError> {
        let accepted = self.prompt(session_id, text, cancel)?;
        let started = Instant::now();
        let deadline = started + self.config.max_wait;
        let mut polls = 0u32;
        loop {
            if cancel.is_cancelled() {
                return Err(AssistantError::Cancelled);
            }
            if Instant::now() >= deadline {
                return Err(AssistantError::Timeout {
                    what: format!("assistant turn on {session_id}"),
                    millis: self.config.max_wait.as_millis() as u64,
                });
            }
            polls += 1;
            let page = self.messages(session_id, cancel)?;
            if let Some(idle) = page.idle_since(accepted.created) {
                if !idle.succeeded() {
                    return Err(AssistantError::TurnFailed {
                        outcome: idle.outcome().to_owned(),
                    });
                }
                let Some(message) = page.assistant_since(accepted.created) else {
                    return Err(AssistantError::Shape {
                        route: "/api/session/{id}/message".to_owned(),
                        message: format!(
                            "turn finished without an assistant message (idle {})",
                            idle.id
                        ),
                    });
                };
                return Ok(Answer {
                    session_id: session_id.to_owned(),
                    message_id: message.id.clone(),
                    text: message.text(),
                    usage: message.tokens.clone(),
                    finish: message.finish.clone(),
                    cost: message.cost,
                    polls,
                    elapsed: started.elapsed(),
                });
            }
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                continue;
            }
            std::thread::sleep(self.config.poll_interval.min(remaining));
        }
    }

    /// Attaches this process as a read-only client-MCP server.
    ///
    /// Connects the WebSocket described by ADR 0011, completes the MCP
    /// handshake, answers `tools/list` from `provider` and dispatches
    /// `tools/call` until the peer closes, the idle budget expires or the
    /// message cap is reached. Only the five read-only document tools defined
    /// by [`crate::tool_definitions`] are advertised.
    pub fn attach_client_mcp(
        &self,
        name: &str,
        provider: &dyn ToolProvider,
        options: McpOptions,
        cancel: &CancelFlag,
    ) -> Result<McpReport, AssistantError> {
        mcp::validate_name(name)?;
        let url = self.http.websocket_url(name)?;
        let transport = mcp::connect(
            &url,
            self.http.authorization(),
            &self.config.timeouts,
            options.idle_timeout,
        )?;
        McpSession::new(transport, options)
            .with_cancel(cancel.clone())
            .serve(provider)
    }

    /// Opens `GET /api/event` with the configured idle timeout.
    pub fn events(&self, cancel: &CancelFlag) -> Result<EventStream, AssistantError> {
        self.events_with_types(&[], cancel)
    }

    /// Opens `GET /api/event` and subscribes to the given event types.
    pub fn events_with_types(
        &self,
        types: &[&str],
        cancel: &CancelFlag,
    ) -> Result<EventStream, AssistantError> {
        let mut route = EVENT_ROUTE.to_owned();
        if !types.is_empty() {
            for kind in types {
                validate_event_type(kind)?;
            }
            route.push_str("?types=");
            route.push_str(&types.join(","));
        }
        let reader = self
            .http
            .open_event_stream(&route, self.config.event_idle_timeout)?;
        Ok(EventStream::new(
            reader,
            cancel.clone(),
            self.config.event_idle_timeout,
        ))
    }
}

impl fmt::Debug for Assistant {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Assistant")
            .field("endpoint", &self.endpoint)
            .field("config", &self.config)
            .finish()
    }
}

#[derive(Serialize)]
struct PromptBody<'a> {
    text: &'a str,
}

#[derive(Serialize)]
struct ModelBody<'a> {
    model: &'a ModelRef,
}

fn session_route(session_id: &str) -> Result<String, AssistantError> {
    validate_token(session_id, "session id")?;
    Ok(format!("{SESSION_ROUTE}/{session_id}"))
}

fn validate_event_type(value: &str) -> Result<(), AssistantError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-' | b'.'));
    if valid {
        Ok(())
    } else {
        Err(AssistantError::InvalidArgument {
            message: format!("invalid event type {value:?}"),
        })
    }
}

fn validate_token(value: &str, what: &str) -> Result<(), AssistantError> {
    let valid = !value.is_empty()
        && value.len() <= 128
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'));
    if valid {
        Ok(())
    } else {
        Err(AssistantError::InvalidArgument {
            message: format!("invalid {what} {value:?}"),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::{session_route, validate_token};
    use crate::error::AssistantError;

    #[test]
    fn session_route_rejects_path_injection() {
        assert_eq!(
            session_route("ses_abc-123").expect("valid"),
            "/api/session/ses_abc-123"
        );
        for bad in ["", "../escape", "ses/one", "ses one", "ses?x=1"] {
            assert!(matches!(
                session_route(bad),
                Err(AssistantError::InvalidArgument { .. })
            ));
        }
    }

    #[test]
    fn tokens_have_a_length_cap() {
        assert!(validate_token(&"a".repeat(129), "id").is_err());
        assert!(validate_token(&"a".repeat(128), "id").is_ok());
    }
}
