//! Blocking HTTP transport for the OpenCode service.
//!
//! `ureq` provides HTTP/1.1, rustls with the platform verifier, and per-phase
//! timeouts. Redirects are never followed: the request carries the service
//! credential in an `Authorization` header, so a hostile or misconfigured
//! redirect must fail instead of replaying it. The event stream uses a separate
//! agent with no total body budget and a per-read idle timeout.

use std::io::ErrorKind;
use std::sync::Arc;
use std::time::Duration;

use base64::Engine as _;
use base64::engine::general_purpose::STANDARD as BASE64;
use serde::Serialize;
use serde::de::DeserializeOwned;
use ureq::Agent;

use crate::cancel::CancelFlag;
use crate::config::Timeouts;
use crate::endpoint::ServiceEndpoint;
use crate::error::{AssistantError, redact, redact_snippet};
use crate::secret::Secret;

/// Username half of the pinned HTTP Basic credential.
const AUTH_USER: &str = "opencode";

/// Upper bound on a JSON response body.
const MAX_BODY_BYTES: usize = 4 * 1024 * 1024;

/// HTTP transport bound to one endpoint and credential.
pub(crate) struct Http {
    agent: Agent,
    base: String,
    authorization: String,
    secret: Secret,
    timeouts: Timeouts,
}

impl Http {
    /// Builds the transport and the one Basic header this client will send.
    pub(crate) fn new(endpoint: &ServiceEndpoint, timeouts: &Timeouts) -> Self {
        let secret = endpoint.password().clone();
        let credentials = format!("{AUTH_USER}:{}", secret.expose());
        let authorization = format!("Basic {}", BASE64.encode(credentials));
        Self {
            agent: build_agent(timeouts, Some(timeouts.body), timeouts.read),
            base: endpoint.url().trim_end_matches('/').to_owned(),
            authorization,
            secret,
            timeouts: timeouts.clone(),
        }
    }

    /// Performs `GET route` and decodes a JSON body.
    pub(crate) fn get_json<T: DeserializeOwned>(
        &self,
        route: &str,
        cancel: &CancelFlag,
    ) -> Result<T, AssistantError> {
        check_cancel(cancel)?;
        let response = self
            .agent
            .get(&self.url(route))
            .header("Authorization", &self.authorization)
            .header("Accept", "application/json")
            .call()
            .map_err(|error| self.transport_error(error, route))?;
        let (status, text) = self.read_body(response, route)?;
        if !(200..300).contains(&status) {
            return Err(self.status_error(status, route, &text));
        }
        parse_json(&text, route)
    }

    /// Performs `POST route` with a JSON body and decodes a JSON response.
    pub(crate) fn post_json<B: Serialize, T: DeserializeOwned>(
        &self,
        route: &str,
        body: &B,
        cancel: &CancelFlag,
    ) -> Result<T, AssistantError> {
        check_cancel(cancel)?;
        let payload = encode_json(body, route)?;
        let response = self
            .agent
            .post(&self.url(route))
            .header("Authorization", &self.authorization)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(payload)
            .map_err(|error| self.transport_error(error, route))?;
        let (status, text) = self.read_body(response, route)?;
        if !(200..300).contains(&status) {
            return Err(self.status_error(status, route, &text));
        }
        parse_json(&text, route)
    }

    /// Performs `POST route`, accepting a response with no body.
    pub(crate) fn post_empty<B: Serialize>(
        &self,
        route: &str,
        body: &B,
        cancel: &CancelFlag,
    ) -> Result<(), AssistantError> {
        check_cancel(cancel)?;
        let payload = encode_json(body, route)?;
        let response = self
            .agent
            .post(&self.url(route))
            .header("Authorization", &self.authorization)
            .header("Accept", "application/json")
            .header("Content-Type", "application/json")
            .send(payload)
            .map_err(|error| self.transport_error(error, route))?;
        let (status, text) = self.read_body(response, route)?;
        if !(200..300).contains(&status) {
            return Err(self.status_error(status, route, &text));
        }
        Ok(())
    }

    /// Opens `GET route` as a server-sent event stream.
    pub(crate) fn open_event_stream(
        &self,
        route: &str,
        idle_timeout: Duration,
    ) -> Result<ureq::BodyReader<'static>, AssistantError> {
        let agent = build_agent(&self.timeouts, None, idle_timeout);
        let response = agent
            .get(&self.url(route))
            .header("Authorization", &self.authorization)
            .header("Accept", "text/event-stream")
            .call()
            .map_err(|error| self.transport_error(error, route))?;
        let status = response.status().as_u16();
        if !(200..300).contains(&status) {
            let (status, text) = self.read_body(response, route)?;
            return Err(self.status_error(status, route, &text));
        }
        let (_parts, body) = response.into_parts();
        Ok(body.into_reader())
    }

    fn url(&self, route: &str) -> String {
        format!("{}{}", self.base, route)
    }

    /// The Basic credential, exposed only for the MCP socket handshake.
    pub(crate) fn authorization(&self) -> &str {
        &self.authorization
    }

    /// The client-MCP WebSocket URL for `name` on this endpoint.
    pub(crate) fn websocket_url(&self, name: &str) -> Result<url::Url, AssistantError> {
        let mut url =
            url::Url::parse(&self.base).map_err(|error| AssistantError::InvalidEndpoint {
                reason: format!("{}: {error}", self.base),
            })?;
        let scheme = if url.scheme() == "https" { "wss" } else { "ws" };
        url.set_scheme(scheme)
            .map_err(|()| AssistantError::InvalidEndpoint {
                reason: format!("cannot build a {scheme} URL from {}", self.base),
            })?;
        url.set_path("/api/experimental/client-mcp");
        url.set_query(Some(&format!("name={name}")));
        Ok(url)
    }

    fn read_body(
        &self,
        mut response: http::Response<ureq::Body>,
        route: &str,
    ) -> Result<(u16, String), AssistantError> {
        let status = response.status().as_u16();
        let text = response
            .body_mut()
            .with_config()
            .limit(MAX_BODY_BYTES as u64)
            .read_to_string()
            .map_err(|error| self.read_error(error, route))?;
        Ok((status, text))
    }

    fn read_error(&self, error: ureq::Error, route: &str) -> AssistantError {
        match error {
            ureq::Error::Timeout(_) => AssistantError::Timeout {
                what: route.to_owned(),
                millis: self.timeouts.body.as_millis() as u64,
            },
            other => self.transport_error(other, route),
        }
    }

    fn transport_error(&self, error: ureq::Error, route: &str) -> AssistantError {
        match error {
            ureq::Error::Timeout(_) => AssistantError::Timeout {
                what: route.to_owned(),
                millis: self.timeouts.read.as_millis() as u64,
            },
            ureq::Error::Io(error) => self.io_error(&error, route),
            ureq::Error::Protocol(error) => AssistantError::Protocol {
                message: format!("{route}: {}", redact(&error.to_string())),
            },
            ureq::Error::StatusCode(status) => self.status_error(status, route, ""),
            ureq::Error::HostNotFound => AssistantError::Network {
                message: format!("host not found for {route}"),
            },
            ureq::Error::ConnectionFailed => AssistantError::Network {
                message: format!("connection failed for {route}"),
            },
            ureq::Error::Tls(message) => AssistantError::Network {
                message: format!("TLS error for {route}: {message}"),
            },
            ureq::Error::BadUri(message) => AssistantError::InvalidEndpoint { reason: message },
            other => AssistantError::Network {
                message: format!("{route}: {}", redact(&other.to_string())),
            },
        }
    }

    fn io_error(&self, error: &std::io::Error, route: &str) -> AssistantError {
        if crate::error::is_timeout(error) {
            return AssistantError::Timeout {
                what: route.to_owned(),
                millis: self.timeouts.read.as_millis() as u64,
            };
        }
        match error.kind() {
            ErrorKind::ConnectionRefused => AssistantError::Network {
                message: format!("connection refused for {route}"),
            },
            _ => AssistantError::Network {
                message: format!("{route}: {}", redact(&error.to_string())),
            },
        }
    }

    fn status_error(&self, status: u16, route: &str, body: &str) -> AssistantError {
        match status {
            401 | 403 => AssistantError::Unauthorized {
                status,
                route: route.to_owned(),
            },
            404 => AssistantError::NotFound {
                route: route.to_owned(),
            },
            _ => AssistantError::Server {
                status,
                route: route.to_owned(),
                body: redact_snippet(body, &self.secret),
            },
        }
    }
}

impl std::fmt::Debug for Http {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Http")
            .field("base", &self.base)
            .field("authorization", &"<redacted>")
            .field("timeouts", &self.timeouts)
            .finish()
    }
}

fn build_agent(
    timeouts: &Timeouts,
    body_timeout: Option<Duration>,
    read_timeout: Duration,
) -> Agent {
    let tls = ureq::tls::TlsConfig::builder()
        .root_certs(system_root_certs())
        .unversioned_rustls_crypto_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .build();
    let config = ureq::Agent::config_builder()
        .http_status_as_error(false)
        .max_redirects(0)
        .tls_config(tls)
        .timeout_connect(Some(timeouts.connect))
        .timeout_recv_response(Some(timeouts.response))
        .timeout_per_read(Some(read_timeout))
        .timeout_recv_body(body_timeout)
        .build();
    ureq::Agent::new_with_config(config)
}

/// The operating system trust store, loaded once.
///
/// `rustls-native-certs` is Apache-2.0 OR ISC OR MIT, so TLS stays inside the
/// repository licence allowlist; `webpki-roots`/`webpki-root-certs` are
/// CDLA-Permissive-2.0 and are deliberately not used. Loopback HTTP never
/// consults these roots; a remote `https` endpoint does.
fn system_root_certs() -> ureq::tls::RootCerts {
    static ROOTS: std::sync::OnceLock<Vec<ureq::tls::Certificate<'static>>> =
        std::sync::OnceLock::new();
    let roots = ROOTS.get_or_init(|| {
        rustls_native_certs::load_native_certs()
            .certs
            .iter()
            .map(|certificate| ureq::tls::Certificate::from_der(certificate.as_ref()).to_owned())
            .collect()
    });
    ureq::tls::RootCerts::new_with_certs(roots)
}

fn check_cancel(cancel: &CancelFlag) -> Result<(), AssistantError> {
    if cancel.is_cancelled() {
        return Err(AssistantError::Cancelled);
    }
    Ok(())
}

fn encode_json<B: Serialize>(body: &B, route: &str) -> Result<Vec<u8>, AssistantError> {
    serde_json::to_vec(body).map_err(|error| AssistantError::Shape {
        route: route.to_owned(),
        message: format!("cannot encode the request body: {error}"),
    })
}

fn parse_json<T: DeserializeOwned>(text: &str, route: &str) -> Result<T, AssistantError> {
    serde_json::from_str(text).map_err(|error| AssistantError::Shape {
        route: route.to_owned(),
        message: format!("invalid JSON: {error}"),
    })
}
