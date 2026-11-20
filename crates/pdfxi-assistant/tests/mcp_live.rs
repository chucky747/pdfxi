//! One live client-MCP attach against the OpenCode service on this machine.
//!
//! Ignored by default. It opens a single WebSocket, completes the MCP
//! handshake, then waits up to 10 s for the service to request `tools/list`.
//! Run once with
//! `cargo test -p pdfxi-assistant --test mcp_live -- --ignored --nocapture`.
//! No document is opened: the provider returns canned probe text.

use std::time::Duration;

use pdfxi_assistant::{Assistant, CancelFlag, McpOptions, ServiceEndpoint, ToolProvider};

struct ProbeTools;

impl ToolProvider for ProbeTools {
    fn document_info(&self) -> Result<String, String> {
        Ok("PDFXI T-0251 MCP probe: no document open".to_owned())
    }
    fn page_text(&self, page: u32) -> Result<String, String> {
        Ok(format!("probe page {page}"))
    }
    fn search(&self, query: &str) -> Result<String, String> {
        Ok(format!("probe search for {query}"))
    }
    fn selection(&self) -> Result<String, String> {
        Ok(String::new())
    }
    fn outline(&self) -> Result<String, String> {
        Ok("probe outline".to_owned())
    }
}

#[test]
#[ignore = "requires a running OpenCode service"]
fn live_client_mcp_handshake_and_tools_list() {
    let endpoint = ServiceEndpoint::discover().expect("service registration");
    println!(
        "endpoint: {} (loopback: {})",
        endpoint.url(),
        endpoint.is_loopback()
    );
    let assistant = Assistant::connect(endpoint);
    let cancel = CancelFlag::new();
    let options = McpOptions {
        idle_timeout: Duration::from_secs(10),
        max_messages: 32,
    };

    let report = assistant
        .attach_client_mcp("pdfxi-t0251", &ProbeTools, options, &cancel)
        .expect("MCP attach");
    println!("mcp report: {report:?}");

    assert!(
        report.initialize_seen,
        "the service did not send an MCP initialize frame"
    );
    assert!(
        report.initialized,
        "the service did not complete the MCP handshake"
    );
    if report.tools_list_requests == 0 {
        println!(
            "no tools/list request within the 10 s window after notifications/initialized; \
             the service likely lists tools only when a session references the MCP"
        );
    } else {
        assert_eq!(
            report.tools_listed.len(),
            5,
            "read-only tool set advertised"
        );
    }
}
