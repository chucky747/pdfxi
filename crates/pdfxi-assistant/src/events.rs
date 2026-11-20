//! Server-sent events from `GET /api/event`.
//!
//! The service emits `data: {"id":"evt_...","type":"...","data":{}}` frames and
//! `: heartbeat` comments. The stream is live-only, so an idle timeout and
//! cancellation both end it cleanly; the caller reconnects when it still wants
//! events.

use std::io::{ErrorKind, Read};
use std::time::Duration;

use serde_json::Value;

use crate::cancel::CancelFlag;
use crate::error::{AssistantError, is_timeout, redact};

/// Upper bound on one event line, to keep a hostile peer from growing memory.
const MAX_LINE_BYTES: usize = 1024 * 1024;

/// One parsed service event.
#[derive(Debug, Clone, PartialEq)]
pub struct Event {
    /// Event id, from the payload or the SSE `id:` field.
    pub id: Option<String>,
    /// SSE `event:` field, when the service sends one.
    pub name: Option<String>,
    /// Event type: the payload `type`, the SSE name, or `message`.
    pub kind: String,
    /// The complete JSON payload.
    pub payload: Value,
}

impl Event {
    /// The payload `data` member, when present.
    pub fn data(&self) -> Option<&Value> {
        self.payload.get("data")
    }

    /// The session id carried by the payload, when present at the top level or
    /// under `data`.
    pub fn session_id(&self) -> Option<&str> {
        self.payload
            .get("sessionID")
            .and_then(Value::as_str)
            .or_else(|| {
                self.data()
                    .and_then(|data| data.get("sessionID"))
                    .and_then(Value::as_str)
            })
    }
}

/// A blocking event stream iterator.
///
/// [`Iterator::next`] yields one event, an error for a malformed frame or an
/// idle timeout, and then `None`. Cancelling the shared [`CancelFlag`] makes the
/// next call return `None` without touching the network.
pub struct EventStream {
    reader: ureq::BodyReader<'static>,
    buffer: Vec<u8>,
    frame: SseBuffer,
    cancel: CancelFlag,
    idle_timeout: Duration,
    finished: bool,
}

impl EventStream {
    pub(crate) fn new(
        reader: ureq::BodyReader<'static>,
        cancel: CancelFlag,
        idle_timeout: Duration,
    ) -> Self {
        Self {
            reader,
            buffer: Vec::new(),
            frame: SseBuffer::default(),
            cancel,
            idle_timeout,
            finished: false,
        }
    }

    /// The idle timeout this stream was opened with.
    pub fn idle_timeout(&self) -> Duration {
        self.idle_timeout
    }

    /// Reads the next event, or `None` at end of stream or after cancellation.
    pub fn next_event(&mut self) -> Option<Result<Event, AssistantError>> {
        loop {
            if self.finished {
                return None;
            }
            if self.cancel.is_cancelled() {
                self.finished = true;
                return None;
            }
            match self.read_line() {
                Ok(None) => {
                    self.finished = true;
                    return None;
                }
                Ok(Some(line)) => {
                    if let Some(frame) = self.frame.push_line(&line) {
                        match event_from_frame(frame) {
                            Ok(event) => return Some(Ok(event)),
                            Err(error) => {
                                self.finished = true;
                                return Some(Err(error));
                            }
                        }
                    }
                }
                Err(error) => {
                    self.finished = true;
                    return Some(Err(error));
                }
            }
        }
    }

    fn read_line(&mut self) -> Result<Option<String>, AssistantError> {
        loop {
            if let Some(index) = self.buffer.iter().position(|byte| *byte == b'\n') {
                let mut line: Vec<u8> = self.buffer.drain(..=index).collect();
                line.pop();
                if line.last() == Some(&b'\r') {
                    line.pop();
                }
                return decode_line(line);
            }
            if self.buffer.len() > MAX_LINE_BYTES {
                return Err(AssistantError::Protocol {
                    message: "event stream line exceeds the size cap".to_owned(),
                });
            }
            let mut chunk = [0u8; 8192];
            match self.reader.read(&mut chunk) {
                Ok(0) => {
                    // A trailing partial line is not a complete event.
                    self.buffer.clear();
                    return Ok(None);
                }
                Ok(read) => self.buffer.extend_from_slice(&chunk[..read]),
                Err(error) if error.kind() == ErrorKind::Interrupted => {}
                Err(error) if is_timeout(&error) => {
                    return Err(AssistantError::Timeout {
                        what: "event stream idle".to_owned(),
                        millis: self.idle_timeout.as_millis() as u64,
                    });
                }
                Err(error) => {
                    return Err(AssistantError::Network {
                        message: format!(
                            "event stream read failed: {}",
                            redact(&error.to_string())
                        ),
                    });
                }
            }
        }
    }
}

impl Iterator for EventStream {
    type Item = Result<Event, AssistantError>;

    fn next(&mut self) -> Option<Self::Item> {
        self.next_event()
    }
}

fn decode_line(line: Vec<u8>) -> Result<Option<String>, AssistantError> {
    String::from_utf8(line)
        .map(Some)
        .map_err(|_| AssistantError::Protocol {
            message: "event stream is not valid UTF-8".to_owned(),
        })
}

/// One dispatched SSE frame.
#[derive(Debug, Default, PartialEq, Eq)]
struct SseFrame {
    data: String,
    name: Option<String>,
    id: Option<String>,
}

/// Accumulates SSE fields until a blank line dispatches the frame.
#[derive(Debug, Default)]
struct SseBuffer {
    data: Vec<String>,
    name: Option<String>,
    id: Option<String>,
}

impl SseBuffer {
    fn push_line(&mut self, line: &str) -> Option<SseFrame> {
        if line.is_empty() {
            if self.data.is_empty() {
                self.name = None;
                self.id = None;
                return None;
            }
            return Some(SseFrame {
                data: std::mem::take(&mut self.data).join("\n"),
                name: self.name.take(),
                id: self.id.take(),
            });
        }
        if line.starts_with(':') {
            return None;
        }
        let (field, value) = match line.split_once(':') {
            Some((field, value)) => (field, value.strip_prefix(' ').unwrap_or(value)),
            None => (line, ""),
        };
        match field {
            "data" => self.data.push(value.to_owned()),
            "event" => self.name = Some(value.to_owned()),
            "id" => self.id = Some(value.to_owned()),
            _ => {}
        }
        None
    }
}

fn event_from_frame(frame: SseFrame) -> Result<Event, AssistantError> {
    let payload: Value =
        serde_json::from_str(&frame.data).map_err(|error| AssistantError::Protocol {
            message: format!("invalid event JSON: {error}"),
        })?;
    let kind = payload
        .get("type")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or_else(|| frame.name.clone())
        .unwrap_or_else(|| "message".to_owned());
    let id = payload
        .get("id")
        .and_then(Value::as_str)
        .map(str::to_owned)
        .or(frame.id);
    Ok(Event {
        id,
        name: frame.name,
        kind,
        payload,
    })
}

#[cfg(test)]
mod tests {
    use super::{SseBuffer, event_from_frame};
    use serde_json::json;

    #[test]
    fn dispatches_a_data_frame() {
        let mut buffer = SseBuffer::default();
        assert!(
            buffer
                .push_line(r#"data: {"id":"evt_1","type":"server.connected","data":{}}"#)
                .is_none()
        );
        let frame = buffer.push_line("").expect("frame");
        let event = event_from_frame(frame).expect("event");
        assert_eq!(event.kind, "server.connected");
        assert_eq!(event.id.as_deref(), Some("evt_1"));
        assert_eq!(event.data(), Some(&json!({})));
        assert!(event.name.is_none());
    }

    #[test]
    fn ignores_heartbeat_comments_and_blank_lines() {
        let mut buffer = SseBuffer::default();
        assert!(buffer.push_line(": heartbeat").is_none());
        assert!(buffer.push_line("").is_none());
    }

    #[test]
    fn joins_multiline_data_and_keeps_event_name() {
        let mut buffer = SseBuffer::default();
        buffer.push_line("event: session.idle");
        buffer.push_line("data: {\"type\":\"session.idle\",");
        buffer.push_line("data:  \"data\":{\"sessionID\":\"ses_1\"}}");
        let frame = buffer.push_line("").expect("frame");
        let event = event_from_frame(frame).expect("event");
        assert_eq!(event.kind, "session.idle");
        assert_eq!(event.name.as_deref(), Some("session.idle"));
        assert_eq!(event.session_id(), Some("ses_1"));
    }

    #[test]
    fn malformed_json_is_a_protocol_error() {
        let mut buffer = SseBuffer::default();
        buffer.push_line("data: not json");
        let frame = buffer.push_line("").expect("frame");
        let error = event_from_frame(frame).expect_err("protocol");
        assert!(matches!(
            error,
            crate::error::AssistantError::Protocol { .. }
        ));
    }

    #[test]
    fn unknown_fields_are_ignored() {
        let mut buffer = SseBuffer::default();
        buffer.push_line("retry: 1000");
        buffer.push_line("data: {\"type\":\"x\"}");
        let frame = buffer.push_line("").expect("frame");
        assert_eq!(event_from_frame(frame).expect("event").kind, "x");
    }
}
