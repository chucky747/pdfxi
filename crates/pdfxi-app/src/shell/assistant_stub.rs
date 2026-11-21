//! Loopback OpenCode stub shared by the assistant UI tests (T-0252/T-0253).
//!
//! The stub answers the metadata routes used by the connect probe and the
//! session, prompt and message routes used by one assistant turn. It records
//! every request (method, path, credential and body) so tests can prove what
//! was and was not sent. It is compiled only for tests.

use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use pdfxi_assistant::{ClientConfig, Timeouts};

/// Credential the stub accepts.
pub(crate) const PASSWORD: &str = "test-secret";
/// Pre-encoded Basic header for [`PASSWORD`].
pub(crate) const EXPECTED_AUTH: &str = "Basic b3BlbmNvZGU6dGVzdC1zZWNyZXQ=";

/// A compatible service.
pub(crate) const INFO: &str = r#"{"version":"2.3.2","pid":17164,"hostname":"test",
    "urls":["http://127.0.0.1:1"],"capabilities":{"persistentPty":true}}"#;
/// A service outside the pinned range.
pub(crate) const INFO_NEWER: &str = r#"{"version":"2.4.0","pid":17164,"hostname":"test",
    "urls":["http://127.0.0.1:1"]}"#;
/// Provider list with one disabled provider.
pub(crate) const PROVIDERS: &str = r#"{"data":[
    {"id":"opencode-go","name":"OpenCode Go","enabled":true},
    {"id":"opencode","name":"OpenCode","enabled":true},
    {"id":"claude-agent","name":"Claude Agent","enabled":false}]}"#;
/// Model list containing the acceptance model.
pub(crate) const MODELS: &str = r#"{"data":[
    {"id":"hy4-flash-free","modelID":"hy4-flash-free","providerID":"opencode-go",
     "type":"chat","name":"Hy4 flash Free","status":"active","enabled":true,
     "variants":[{"id":"none"},{"id":"high"}],
     "capabilities":{"tools":true,"input":["text"],"output":["text"]},
     "limit":{"context":1048576}},
    {"id":"deepseek-v4.1-flash","providerID":"opencode-go","type":"chat",
     "name":"DeepSeek V4.1 Flash","enabled":true,
     "limit":{"context":262144}}]}"#;

/// The answer the stub returns for the first prompt of a session.
pub(crate) const ANSWER: &str = "The relevant text is on page 2.";

/// One request received by the stub.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(crate) struct Recorded {
    /// HTTP method.
    pub(crate) method: String,
    /// Path with query values.
    pub(crate) path: String,
    /// Basic authorization header, when sent.
    pub(crate) authorization: Option<String>,
    /// Complete request body as UTF-8.
    pub(crate) body: String,
}

/// Per-session prompt state used to answer exactly one turn.
#[derive(Debug, Clone, Copy, Default)]
struct SessionState {
    prompts: u32,
    created: u64,
}

/// An in-process loopback stub for the connect probe and one assistant turn.
pub(crate) struct Stub {
    /// Base URL, for example `http://127.0.0.1:12345`.
    pub(crate) url: String,
    address: std::net::SocketAddr,
    requests: Arc<Mutex<Vec<Recorded>>>,
    stop: Arc<AtomicBool>,
    handle: Option<std::thread::JoinHandle<()>>,
}

impl Stub {
    /// Start the stub with a specific `/api/info` payload.
    pub(crate) fn start(info: &'static str) -> Self {
        // A blocking listener: Windows is sensitive to a non-blocking
        // listener's flag leaking into accepted sockets and resetting the
        // connection before the client reads the response.
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind loopback stub");
        let address = listener.local_addr().expect("stub address");
        let url = format!("http://{address}");
        let requests = Arc::new(Mutex::new(Vec::new()));
        let recorded = Arc::clone(&requests);
        let stop = Arc::new(AtomicBool::new(false));
        let worker_stop = Arc::clone(&stop);
        let handle = std::thread::spawn(move || {
            let mut sessions: Vec<(String, SessionState)> = Vec::new();
            while !worker_stop.load(Ordering::SeqCst) {
                let Ok((mut stream, _)) = listener.accept() else {
                    break;
                };
                if worker_stop.load(Ordering::SeqCst) {
                    break;
                }
                let _ = stream.set_nonblocking(false);
                let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
                while let Some((head, body)) = read_request(&mut stream) {
                    let request = parse_head(&head, &body);
                    if worker_stop.load(Ordering::SeqCst) {
                        break;
                    }
                    let (status, payload) = respond(info, &request, &mut sessions);
                    recorded.lock().expect("record requests").push(request);
                    let response = if status == "204 No Content" {
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
                        )
                    } else {
                        format!(
                            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\n\
                             Content-Length: {}\r\nConnection: close\r\n\r\n{payload}",
                            payload.len()
                        )
                    };
                    if stream.write_all(response.as_bytes()).is_err() {
                        break;
                    }
                    let _ = stream.flush();
                }
            }
        });
        Self {
            url,
            address,
            requests,
            stop,
            handle: Some(handle),
        }
    }

    /// Requests recorded so far, in arrival order.
    pub(crate) fn requests(&self) -> Vec<Recorded> {
        self.requests.lock().expect("requests").clone()
    }

    /// Forget recorded requests; useful before asserting that a later action
    /// sent nothing.
    pub(crate) fn clear_requests(&self) {
        self.requests.lock().expect("requests").clear();
    }
}

impl Drop for Stub {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        // Wake the blocking `accept`; the worker sees the stop flag and exits.
        if let Ok(wake) = TcpStream::connect(self.address) {
            drop(wake);
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
}

/// Route one recorded request. Session state lives in the stub's worker thread.
fn respond(
    info: &'static str,
    request: &Recorded,
    sessions: &mut Vec<(String, SessionState)>,
) -> (&'static str, String) {
    if request.authorization.as_deref() != Some(EXPECTED_AUTH) {
        return ("401 Unauthorized", String::new());
    }
    let path = request.path.as_str();
    if request.method == "GET" && path == "/api/info" {
        return ("200 OK", info.to_owned());
    }
    if request.method == "GET" && path == "/api/provider" {
        return ("200 OK", PROVIDERS.to_owned());
    }
    if request.method == "GET" && path == "/api/model" {
        return ("200 OK", MODELS.to_owned());
    }
    if request.method == "POST" && path == "/api/session" {
        let id = format!("ses_test{}", sessions.len() + 1);
        sessions.push((id.clone(), SessionState::default()));
        return ("200 OK", format!(r#"{{"data":{{"id":"{id}"}}}}"#));
    }
    if request.method == "POST" && path.ends_with("/prompt") {
        let Some(id) = session_id(path, "/prompt") else {
            return ("404 Not Found", String::new());
        };
        let Some((_, state)) = sessions.iter_mut().find(|(session, _)| session == &id) else {
            return ("404 Not Found", String::new());
        };
        state.prompts += 1;
        state.created = now_millis();
        let created = state.created;
        return (
            "200 OK",
            format!(
                r#"{{"data":{{"id":"msg_user{}","sessionID":"{id}","type":"user",
                   "time":{{"created":{created}}},"payload":{{"text":"prompt"}}}}}}"#,
                state.prompts
            ),
        );
    }
    if request.method == "GET" && path.contains("/message") {
        let Some(id) = session_id(path, "/message") else {
            return ("404 Not Found", String::new());
        };
        let Some((_, state)) = sessions.iter().find(|(session, _)| session == &id) else {
            return ("404 Not Found", String::new());
        };
        if state.prompts != 1 {
            // Only the first turn of a session completes. A second prompt has
            // no idle item, which keeps an assistant turn visibly in flight for
            // the stop and capture tests.
            return ("200 OK", r#"{"data":[]}"#.to_owned());
        }
        let created = state.created;
        return (
            "200 OK",
            format!(
                r#"{{"data":[
                   {{"id":"msg_idle","type":"idle","time":{{"created":{}}},"outcome":"succeeded"}},
                   {{"id":"msg_answer","type":"assistant","time":{{"created":{}}},
                     "content":[{{"type":"text","text":"{ANSWER}"}}],"finish":"stop"}},
                   {{"id":"msg_user1","type":"user","time":{{"created":{created}}},
                     "payload":{{"text":"prompt"}}}}]}}"#,
                created + 2,
                created + 1
            ),
        );
    }
    if request.method == "DELETE" && path.starts_with("/api/session/") {
        return ("204 No Content", String::new());
    }
    ("404 Not Found", String::new())
}

/// Extract `{id}` from `/api/session/{id}{suffix}`.
fn session_id(path: &str, suffix: &str) -> Option<String> {
    let path = path.split('?').next().unwrap_or(path);
    let rest = path.strip_prefix("/api/session/")?;
    let id = rest.strip_suffix(suffix)?;
    (!id.is_empty() && !id.contains('/')).then(|| id.to_owned())
}

/// Milliseconds since the Unix epoch, matching the service's message times.
fn now_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis() as u64)
        .unwrap_or(0)
}

/// Reads one complete request: all headers and any `Content-Length` body.
fn read_request(stream: &mut TcpStream) -> Option<(String, Vec<u8>)> {
    let mut bytes = Vec::new();
    let mut chunk = [0u8; 1024];
    let header_end = loop {
        if let Some(position) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break position + 4;
        }
        let read = read_chunk(stream, &mut chunk)?;
        if read == 0 {
            return None;
        }
        bytes.extend_from_slice(&chunk[..read]);
        if bytes.len() > 64 * 1024 {
            return None;
        }
    };
    let head = String::from_utf8(bytes[..header_end].to_vec()).ok()?;
    let content_length = head.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-length")
            .then(|| value.trim().parse::<usize>().ok())
            .flatten()
    });
    let mut body = bytes[header_end..].to_vec();
    let expected = content_length.unwrap_or(0);
    while body.len() < expected {
        let read = read_chunk(stream, &mut chunk)?;
        if read == 0 {
            return None;
        }
        body.extend_from_slice(&chunk[..read]);
        if body.len() > 1024 * 1024 {
            return None;
        }
    }
    body.truncate(expected);
    Some((head, body))
}

/// Reads one chunk, retrying a transient non-blocking result.
fn read_chunk(stream: &mut TcpStream, chunk: &mut [u8]) -> Option<usize> {
    loop {
        match stream.read(chunk) {
            Ok(read) => return Some(read),
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(_) => return None,
        }
    }
}

/// Parse the request line, authorization header and body into a record.
fn parse_head(head: &str, body: &[u8]) -> Recorded {
    let mut lines = head.lines();
    let request_line = lines.next().unwrap_or_default();
    let mut parts = request_line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_owned();
    let path = parts.next().unwrap_or_default().to_owned();
    let authorization = lines.find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("authorization")
            .then(|| value.trim().to_owned())
    });
    Recorded {
        method,
        path,
        authorization,
        body: String::from_utf8_lossy(body).into_owned(),
    }
}

/// Short client timings so a failed test fails fast.
pub(crate) fn test_config() -> ClientConfig {
    ClientConfig {
        timeouts: Timeouts {
            connect: Duration::from_secs(2),
            response: Duration::from_secs(5),
            read: Duration::from_secs(5),
            body: Duration::from_secs(5),
        },
        ..ClientConfig::default()
    }
}
