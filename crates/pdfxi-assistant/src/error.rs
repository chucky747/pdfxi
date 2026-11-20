//! Typed errors and credential redaction.

use crate::secret::Secret;

/// Maximum number of characters kept from a response body in an error.
pub const BODY_SNIPPET_LIMIT: usize = 256;

/// Errors returned by the assistant client.
///
/// Every variant is safe to log: payloads are capped and passed through
/// [`redact`], and the service password is never stored in an error.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum AssistantError {
    /// No service registration file exists at the expected location.
    #[error("no OpenCode service registration at {path}")]
    ServiceNotFound {
        /// Registration path that was checked.
        path: String,
    },

    /// The registration file exists but cannot be used.
    #[error("invalid OpenCode service registration at {path}: {reason}")]
    RegistrationInvalid {
        /// Registration path that was read.
        path: String,
        /// Why the registration was rejected.
        reason: String,
    },

    /// The endpoint URL is not acceptable to this client.
    #[error("invalid OpenCode endpoint: {reason}")]
    InvalidEndpoint {
        /// Why the URL was rejected.
        reason: String,
    },

    /// A discovered registration points outside the local machine.
    #[error(
        "discovered OpenCode endpoint {url} is not loopback; a non-loopback endpoint requires an explicit override"
    )]
    NonLoopbackEndpoint {
        /// Rejected URL, without credentials.
        url: String,
    },

    /// An explicit remote endpoint would send the credential in clear text.
    #[error("remote OpenCode endpoint {url} must use https")]
    InsecureRemoteEndpoint {
        /// Rejected URL, without credentials.
        url: String,
    },

    /// The service rejected the credential.
    #[error("OpenCode rejected the credential (HTTP {status}) for {route}")]
    Unauthorized {
        /// HTTP status, 401 or 403.
        status: u16,
        /// Route that was called, without query values.
        route: String,
    },

    /// The route does not exist on this service.
    #[error("OpenCode route {route} was not found (HTTP 404)")]
    NotFound {
        /// Route that was called.
        route: String,
    },

    /// A request or the answer wait exceeded its budget.
    #[error("{what} timed out after {millis} ms")]
    Timeout {
        /// The operation that timed out.
        what: String,
        /// The budget in milliseconds.
        millis: u64,
    },

    /// The caller cancelled the operation.
    #[error("cancelled")]
    Cancelled,

    /// A typed assistant turn ended without a successful outcome.
    #[error("assistant turn finished with outcome {outcome}")]
    TurnFailed {
        /// Outcome reported by the service idle item.
        outcome: String,
    },

    /// The transport failed before a response was available.
    #[error("network error: {message}")]
    Network {
        /// Sanitised transport message.
        message: String,
    },

    /// The service answered with something that is not HTTP or not JSON.
    #[error("protocol error: {message}")]
    Protocol {
        /// Sanitised protocol message.
        message: String,
    },

    /// JSON did not match the pinned 2.3.2 shape.
    #[error("unexpected {route} response: {message}")]
    Shape {
        /// Route whose response did not match.
        route: String,
        /// Which part of the shape drifted.
        message: String,
    },

    /// The service returned an error status with a redacted body snippet.
    #[error("OpenCode returned HTTP {status} for {route}: {body}")]
    Server {
        /// HTTP status.
        status: u16,
        /// Route that was called.
        route: String,
        /// Redacted, length-capped body snippet.
        body: String,
    },

    /// The service version is outside the pinned range.
    #[error("OpenCode service version {version} is not compatible with the pinned 2.3.x routes")]
    IncompatibleVersion {
        /// Version reported by `/api/info`.
        version: String,
    },

    /// The caller supplied an argument the client refuses.
    #[error("invalid argument: {message}")]
    InvalidArgument {
        /// Why the argument was rejected.
        message: String,
    },

    /// The client is misconfigured on this host.
    #[error("configuration error: {message}")]
    Configuration {
        /// What is missing or unusable.
        message: String,
    },

    /// A filesystem operation failed.
    #[error("io error: {message}")]
    Io {
        /// Path and reason, without credentials.
        message: String,
    },
}

impl AssistantError {
    /// The HTTP status behind this error, when it came from a response.
    pub fn http_status(&self) -> Option<u16> {
        match self {
            Self::Unauthorized { status, .. } | Self::Server { status, .. } => Some(*status),
            Self::NotFound { .. } => Some(404),
            _ => None,
        }
    }

    /// Whether the error is a cancellation.
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled)
    }
}

/// Redacts credential-looking fragments from `input` before it is logged.
///
/// Handles `password`, `passwd`, `secret`, `authorization`, `api_key`,
/// `apikey` and `token` assignments (JSON or query form) and masks the value
/// after `Basic` or `Bearer`. Control characters other than newline and tab are
/// replaced with spaces. The result is safe to show in a log line or a dialog.
pub fn redact(input: &str) -> String {
    let printable: String = input
        .chars()
        .map(|c| {
            if c.is_control() && c != '\n' && c != '\t' {
                ' '
            } else {
                c
            }
        })
        .collect();
    let masked = mask_auth_schemes(&printable);
    mask_assignments(&masked)
}

/// Whether an I/O error came from a transport read timeout.
///
/// `ureq` reports a per-read timeout as an `io::Error` that wraps its own
/// timeout error instead of using [`std::io::ErrorKind::TimedOut`], so the kind
/// alone is not enough.
pub(crate) fn is_timeout(error: &std::io::Error) -> bool {
    if matches!(
        error.kind(),
        std::io::ErrorKind::TimedOut | std::io::ErrorKind::WouldBlock
    ) {
        return true;
    }
    error
        .get_ref()
        .and_then(|inner| inner.downcast_ref::<ureq::Error>())
        .is_some_and(|inner| matches!(inner, ureq::Error::Timeout(_)))
}

/// Redacts a server response body and caps it for an error message.
pub(crate) fn redact_snippet(body: &str, secret: &Secret) -> String {
    let trimmed = body.trim();
    let without_secret = if secret.is_empty() {
        trimmed.to_owned()
    } else {
        trimmed.replace(secret.expose(), "<redacted>")
    };
    let redacted = redact(&without_secret);
    let mut out: String = redacted.chars().take(BODY_SNIPPET_LIMIT).collect();
    if redacted.chars().count() > BODY_SNIPPET_LIMIT {
        out.push_str("...");
    }
    out
}

const CREDENTIAL_KEYS: [&str; 7] = [
    "password",
    "passwd",
    "secret",
    "authorization",
    "api_key",
    "apikey",
    "token",
];

fn is_word(byte: u8) -> bool {
    byte.is_ascii_alphanumeric() || byte == b'_'
}

fn match_key(lower: &[u8], at: usize) -> Option<usize> {
    let rest = lower.get(at..)?;
    CREDENTIAL_KEYS
        .iter()
        .find(|key| rest.starts_with(key.as_bytes()))
        .map(|key| key.len())
}

fn mask_assignments(text: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut scan = 0usize;
    while scan < bytes.len() {
        let Some(key_len) = match_key(lower.as_bytes(), scan) else {
            scan += 1;
            continue;
        };
        if scan > 0 && is_word(bytes[scan - 1]) {
            scan += 1;
            continue;
        }
        let key_end = scan + key_len;
        if key_end < bytes.len() && is_word(bytes[key_end]) {
            scan = key_end;
            continue;
        }
        let allow_spaces = &lower[scan..key_end] == "authorization";
        match assignment_value(bytes, key_end, allow_spaces) {
            Some((start, end)) => {
                out.push_str(&text[copied..start]);
                out.push_str("<redacted>");
                copied = end;
                scan = end;
            }
            None => scan = key_end,
        }
    }
    out.push_str(&text[copied..]);
    out
}

/// Returns the value span of `key: value`, `key=value` or `"key":"value"`.
///
/// `allow_spaces` is used for an `Authorization` header dump, where the value
/// is a scheme and a token rather than a single word.
fn assignment_value(bytes: &[u8], key_end: usize, allow_spaces: bool) -> Option<(usize, usize)> {
    let mut index = key_end;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
        index += 1;
    }
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    if index >= bytes.len() || !matches!(bytes[index], b':' | b'=') {
        return None;
    }
    index += 1;
    while index < bytes.len() && matches!(bytes[index], b' ' | b'\t') {
        index += 1;
    }
    let quote = if index < bytes.len() && matches!(bytes[index], b'"' | b'\'') {
        let quote = bytes[index];
        index += 1;
        Some(quote)
    } else {
        None
    };
    let start = index;
    while index < bytes.len() {
        let byte = bytes[index];
        match quote {
            Some(quote) if byte == quote => break,
            Some(_) => {}
            None if matches!(byte, b',' | b'}' | b']' | b'&' | b';' | b'\n' | b'\r') => break,
            None if !allow_spaces && matches!(byte, b' ' | b'\t') => break,
            None => {}
        }
        index += 1;
    }
    (index > start).then_some((start, index))
}

fn mask_auth_schemes(text: &str) -> String {
    let mut current = text.to_owned();
    for scheme in ["basic ", "bearer "] {
        current = mask_scheme(&current, scheme);
    }
    current
}

fn mask_scheme(text: &str, scheme: &str) -> String {
    let lower = text.to_ascii_lowercase();
    let bytes = text.as_bytes();
    let mut out = String::with_capacity(text.len());
    let mut copied = 0usize;
    let mut scan = 0usize;
    while let Some(offset) = lower.get(scan..).and_then(|rest| rest.find(scheme)) {
        let start = scan + offset;
        let value_start = start + scheme.len();
        if start > 0 && is_word(bytes[start - 1]) {
            scan = value_start;
            continue;
        }
        let mut end = value_start;
        while end < bytes.len()
            && !matches!(
                bytes[end],
                b' ' | b'\t' | b'\n' | b'\r' | b'"' | b'\'' | b',' | b'}' | b']' | b';'
            )
        {
            end += 1;
        }
        if end == value_start {
            scan = value_start;
            continue;
        }
        out.push_str(&text[copied..value_start]);
        out.push_str("<redacted>");
        copied = end;
        scan = end;
    }
    out.push_str(&text[copied..]);
    out
}

#[cfg(test)]
mod tests {
    use super::{AssistantError, BODY_SNIPPET_LIMIT, redact, redact_snippet};
    use crate::secret::Secret;

    #[test]
    fn redacts_json_password() {
        let input = r#"{"password":"s3cret-value","user":"opencode"}"#;
        let output = redact(input);
        assert_eq!(output, r#"{"password":"<redacted>","user":"opencode"}"#);
        assert!(!output.contains("s3cret-value"));
    }

    #[test]
    fn redacts_basic_authorization_header() {
        let output = redact("Authorization: Basic b3BlbmNvZGU6czNjcmV0");
        assert!(!output.contains("b3BlbmNvZGU6czNjcmV0"));
        assert!(output.contains("<redacted>"));
        assert!(output.to_ascii_lowercase().starts_with("authorization:"));
    }

    #[test]
    fn redacts_bare_bearer_token() {
        let output = redact("Bearer abcdef0123456789");
        assert!(!output.contains("abcdef0123456789"));
    }

    #[test]
    fn leaves_usage_totals_alone() {
        let input = r#"{"tokens":{"input":10,"output":4},"secret_santa":"ho ho"}"#;
        let output = redact(input);
        assert!(output.contains("\"input\":10"));
        assert!(output.contains("\"output\":4"));
    }

    #[test]
    fn strips_control_characters() {
        let output = redact("a\u{0}b\u{7}c");
        assert_eq!(output, "a b c");
    }

    #[test]
    fn snippet_removes_the_service_password_and_caps_length() {
        let secret = Secret::new("hunter2-secret");
        let body = format!("{{\"error\":\"hunter2-secret {}\"}}", "x".repeat(1000));
        let snippet = redact_snippet(&body, &secret);
        assert!(!snippet.contains("hunter2-secret"));
        assert!(snippet.contains("<redacted>"));
        assert!(snippet.chars().count() <= BODY_SNIPPET_LIMIT + 3);
        assert!(snippet.ends_with("..."));
    }

    #[test]
    fn error_debug_is_secret_free() {
        let secret = Secret::new("hunter2-secret");
        let body = redact_snippet("{\"password\":\"hunter2-secret\"}", &secret);
        let error = AssistantError::Server {
            status: 500,
            route: "/api/info".to_owned(),
            body,
        };
        let rendered = format!("{error:?} {error}");
        assert!(!rendered.contains("hunter2-secret"));
        assert!(rendered.contains("<redacted>"));
    }

    #[test]
    fn http_status_is_exposed() {
        let error = AssistantError::Server {
            status: 502,
            route: "/api/model".to_owned(),
            body: String::new(),
        };
        assert_eq!(error.http_status(), Some(502));
        assert!(!error.is_cancelled());
        assert!(AssistantError::Cancelled.is_cancelled());
    }
}
