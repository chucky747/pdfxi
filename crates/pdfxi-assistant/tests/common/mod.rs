//! An in-process HTTP/1.1 stub for the assistant client tests.
//!
//! No test in this module uses the network: the stub binds an ephemeral
//! loopback port, serves canned 2.3.2 responses, records every request and
//! stops on drop.

#![allow(dead_code)]

use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::Duration;

/// A request the stub received.
#[derive(Debug, Clone)]
pub struct Request {
    /// HTTP method.
    pub method: String,
    /// Path including the query string.
    pub path: String,
    /// Lower-cased header names.
    pub headers: Vec<(String, String)>,
    /// Raw body.
    pub body: Vec<u8>,
}

impl Request {
    /// A header value by lower-case name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key == name)
            .map(|(_, value)| value.as_str())
    }

    /// The path without its query string.
    pub fn route(&self) -> &str {
        self.path.split('?').next().unwrap_or(&self.path)
    }

    /// The body decoded as JSON.
    pub fn json(&self) -> serde_json::Value {
        serde_json::from_slice(&self.body).expect("request body is JSON")
    }
}

/// A canned response.
#[derive(Debug, Clone)]
pub struct Response {
    /// HTTP status.
    pub status: u16,
    /// Content type. Ignored when `content_length` is false.
    pub content_type: &'static str,
    /// Body bytes.
    pub body: Vec<u8>,
    /// Whether to send `Content-Length`. A streaming response omits it and
    /// relies on the connection staying open.
    pub content_length: bool,
    /// Delay before the connection is closed, keeping the socket open.
    pub hold_open: Duration,
}

impl Response {
    /// A JSON response.
    pub fn json(body: impl Into<String>) -> Self {
        Self::text(200, "application/json", body)
    }

    /// A text response.
    pub fn text(status: u16, content_type: &'static str, body: impl Into<String>) -> Self {
        Self {
            status,
            content_type,
            body: body.into().into_bytes(),
            content_length: true,
            hold_open: Duration::ZERO,
        }
    }

    /// An empty response with a status.
    pub fn empty(status: u16) -> Self {
        Self::text(status, "text/plain", "")
    }

    /// A close-delimited stream that stays open for `hold_open`.
    pub fn stream(
        content_type: &'static str,
        body: impl Into<String>,
        hold_open: Duration,
    ) -> Self {
        Self {
            status: 200,
            content_type,
            body: body.into().into_bytes(),
            content_length: false,
            hold_open,
        }
    }
}

/// A running stub server.
pub struct Stub {
    address: String,
    stop: Arc<AtomicBool>,
    requests: Arc<Mutex<Vec<Request>>>,
    handle: Option<JoinHandle<()>>,
}

impl Stub {
    /// Starts a stub serving `handler` for every request.
    pub fn start(handler: impl Fn(&Request) -> Response + Send + Sync + 'static) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind stub");
        let address = format!("http://{}", listener.local_addr().expect("stub address"));
        let stop = Arc::new(AtomicBool::new(false));
        let requests = Arc::new(Mutex::new(Vec::new()));
        let handler = Arc::new(handler);
        let thread_stop = Arc::clone(&stop);
        let thread_requests = Arc::clone(&requests);
        let handle = std::thread::spawn(move || {
            for incoming in listener.incoming() {
                if thread_stop.load(Ordering::SeqCst) {
                    break;
                }
                let Ok(mut stream) = incoming else { break };
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
                let Ok(request) = read_request(&mut stream) else {
                    let _ = write_response(&mut stream, &Response::empty(400));
                    continue;
                };
                thread_requests
                    .lock()
                    .expect("request log")
                    .push(request.clone());
                let response = handler(&request);
                let _ = write_response(&mut stream, &response);
                if !response.hold_open.is_zero() {
                    std::thread::sleep(response.hold_open);
                }
            }
        });
        Self {
            address,
            stop,
            requests,
            handle: Some(handle),
        }
    }

    /// The base URL of the stub.
    pub fn url(&self) -> String {
        self.address.clone()
    }

    /// Every request received so far.
    pub fn requests(&self) -> Vec<Request> {
        self.requests.lock().expect("request log").clone()
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking accept loop.
        if let Some(address) = self.address.strip_prefix("http://") {
            let _ = TcpStream::connect(address);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

fn read_request(stream: &mut TcpStream) -> std::io::Result<Request> {
    let mut reader = BufReader::new(stream.try_clone()?);
    let mut request_line = String::new();
    if reader.read_line(&mut request_line)? == 0 {
        return Err(std::io::Error::new(
            std::io::ErrorKind::UnexpectedEof,
            "empty request",
        ));
    }
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let mut headers = Vec::new();
    loop {
        let mut line = String::new();
        if reader.read_line(&mut line)? == 0 {
            break;
        }
        let line = line.trim_end_matches(['\r', '\n']);
        if line.is_empty() {
            break;
        }
        if let Some((name, value)) = line.split_once(':') {
            headers.push((name.trim().to_ascii_lowercase(), value.trim().to_owned()));
        }
    }
    let length = headers
        .iter()
        .find(|(name, _)| name == "content-length")
        .and_then(|(_, value)| value.parse::<usize>().ok())
        .unwrap_or(0);
    let mut body = vec![0u8; length];
    if length > 0 {
        reader.read_exact(&mut body)?;
    }
    Ok(Request {
        method,
        path,
        headers,
        body,
    })
}

fn write_response(stream: &mut TcpStream, response: &Response) -> std::io::Result<()> {
    let mut head = format!(
        "HTTP/1.1 {} {}\r\nContent-Type: {}\r\nConnection: close\r\n",
        response.status,
        reason(response.status),
        response.content_type
    );
    if response.content_length {
        head.push_str(&format!("Content-Length: {}\r\n", response.body.len()));
    }
    head.push_str("\r\n");
    stream.write_all(head.as_bytes())?;
    stream.write_all(&response.body)?;
    stream.flush()
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        204 => "No Content",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        500 => "Internal Server Error",
        _ => "Unknown",
    }
}
