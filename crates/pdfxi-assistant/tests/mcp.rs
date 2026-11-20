//! Offline tests for the client-MCP server over the public transport trait.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use pdfxi_assistant::{
    AssistantError, CancelFlag, McpOptions, McpReport, McpSession, McpTransport, ToolProvider,
};
use serde_json::{Value, json};

struct Recorder {
    incoming: VecDeque<Value>,
    sent: Arc<Mutex<Vec<Value>>>,
    idle: bool,
}

impl McpTransport for Recorder {
    fn receive(&mut self) -> Result<Option<Value>, AssistantError> {
        if let Some(message) = self.incoming.pop_front() {
            return Ok(Some(message));
        }
        if self.idle {
            return Err(AssistantError::Timeout {
                what: "test idle".to_owned(),
                millis: 10,
            });
        }
        Ok(None)
    }

    fn send(&mut self, message: Value) -> Result<(), AssistantError> {
        self.sent.lock().expect("sent").push(message);
        Ok(())
    }
}

struct Document;

impl ToolProvider for Document {
    fn document_info(&self) -> Result<String, String> {
        Ok("live.pdf, 2 pages".to_owned())
    }
    fn page_text(&self, page: u32) -> Result<String, String> {
        Ok(format!("page {page} text"))
    }
    fn search(&self, query: &str) -> Result<String, String> {
        Ok(format!("1 match for {query}"))
    }
    fn selection(&self) -> Result<String, String> {
        Ok(String::new())
    }
    fn outline(&self) -> Result<String, String> {
        Ok("Chapter 1 -> 1".to_owned())
    }
}

fn serve(messages: Vec<Value>, idle: bool) -> (Arc<Mutex<Vec<Value>>>, McpReport) {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let session = McpSession::new(
        Recorder {
            incoming: messages.into(),
            sent: Arc::clone(&sent),
            idle,
        },
        McpOptions::default(),
    );
    let report = session.serve(&Document).expect("serve");
    (sent, report)
}

#[test]
fn completes_the_handshake_and_answers_tools_list() {
    let (sent, report) = serve(
        vec![
            json!({"jsonrpc":"2.0","method":"initialize","params":{
                "protocolVersion":"2025-11-25",
                "clientInfo":{"name":"opencode","version":"2.3.2"}},"id":0}),
            json!({"jsonrpc":"2.0","method":"notifications/initialized"}),
            json!({"jsonrpc":"2.0","method":"tools/list","id":1}),
            json!({"jsonrpc":"2.0","method":"tools/call","id":2,
                "params":{"name":"search","arguments":{"query":"invoice"}}}),
        ],
        false,
    );
    assert!(report.initialized);
    assert_eq!(report.client_name.as_deref(), Some("opencode"));
    assert_eq!(report.tools_list_requests, 1);
    assert_eq!(report.tools_listed.len(), 5);
    assert_eq!(report.tool_calls, vec!["search".to_owned()]);

    let sent = sent.lock().expect("sent");
    assert_eq!(sent[0]["result"]["protocolVersion"], "2025-11-25");
    assert_eq!(
        sent[1]["result"]["tools"].as_array().expect("tools").len(),
        5
    );
    assert_eq!(
        sent[2]["result"]["content"][0]["text"],
        "1 match for invoice"
    );
}

#[test]
fn idle_timeout_ends_the_connection() {
    let (_, report) = serve(
        vec![
            json!({"jsonrpc":"2.0","method":"initialize","params":{"clientInfo":{"name":"opencode"}},"id":0}),
        ],
        true,
    );
    assert!(report.idle_timed_out);
    assert_eq!(report.messages, 1);
}

#[test]
fn cancellation_stops_before_the_next_message() {
    let sent = Arc::new(Mutex::new(Vec::new()));
    let cancel = CancelFlag::new();
    cancel.cancel();
    let session = McpSession::new(
        Recorder {
            incoming: vec![json!({"jsonrpc":"2.0","method":"initialize","id":0})].into(),
            sent: Arc::clone(&sent),
            idle: false,
        },
        McpOptions {
            idle_timeout: Duration::from_millis(50),
            max_messages: 8,
        },
    )
    .with_cancel(cancel);
    let report = session.serve(&Document).expect("serve");
    assert_eq!(report.messages, 0);
    assert!(sent.lock().expect("sent").is_empty());
}
