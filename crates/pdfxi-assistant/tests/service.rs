//! Offline tests against an in-process HTTP stub serving pinned 2.3.2 shapes.

mod common;

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use common::{Request, Response, Stub};
use pdfxi_assistant::{
    Assistant, AssistantError, BODY_SNIPPET_LIMIT, CancelFlag, ClientConfig, ModelRef,
    PermissionEffect, PermissionRule, Provider, ServiceEndpoint, SessionRequest, Timeouts,
    locked_ruleset,
};

const PASSWORD: &str = "test-secret";
const EXPECTED_AUTH: &str = "Basic b3BlbmNvZGU6dGVzdC1zZWNyZXQ=";

const INFO: &str = r#"{"version":"2.3.2","pid":17164,"hostname":"test",
    "urls":["http://127.0.0.1:1"],"paths":{"tmp":"/tmp"},
    "capabilities":{"persistentPty":true}}"#;

const PROVIDERS: &str = r#"{"location":{"directory":"/tmp"},"data":[
    {"id":"opencode-go","name":"OpenCode Go","enabled":true,"type":"api"},
    {"id":"opencode","name":"OpenCode","enabled":true},
    {"id":"claude-agent","name":"Claude Agent","enabled":false}]}"#;

const MODELS: &str = r#"{"data":[
    {"id":"hy4-flash-free","modelID":"hy4-flash-free","providerID":"opencode-go",
     "type":"chat","name":"Hy4 flash Free","status":"active","enabled":true,
     "variants":[{"id":"none"},{"id":"high"}],
     "capabilities":{"tools":true,"input":["text","image"],"output":["text"]},
     "limit":{"context":1048576,"input":983040,"output":65536},
     "cost":[{"input":0,"output":0,"cache":{"read":0,"write":0}}]},
    {"id":"deepseek-v4.1-flash","providerID":"opencode-go","type":"chat",
     "name":"DeepSeek V4.1 Flash","enabled":false,
     "limit":{"context":262144},"cost":[{"input":0.2,"output":0.4}]}]}"#;

const SESSION: &str = r#"{"data":{"id":"ses_test","projectID":"prj_1",
    "model":{"providerID":"opencode-go","id":"hy4-flash-free","variant":"default"},
    "cost":0,"tokens":{"input":0,"output":0,"reasoning":0,"cache":{"read":0,"write":0}},
    "time":{"created":1795210770502,"updated":1795210770502},
    "title":"PDFXI test","subpath":"","location":{"directory":"/tmp"}}}"#;

const PROMPT_ACCEPTED: &str = r#"{"data":{"id":"msg_user","sessionID":"ses_test",
    "time":{"created":1000},"type":"user",
    "payload":{"text":"Reply with the single word: pong"},"delivery":"steer"}}"#;

const MESSAGES_FIRST: &str = r#"{"data":[
    {"id":"msg_user","sessionID":"ses_test","time":{"created":1000},"type":"user",
     "payload":{"text":"Reply with the single word: pong"}}],
    "cursor":{"previous":null,"next":null}}"#;

const MESSAGES_DONE: &str = r#"{"data":[
    {"id":"msg_idle","time":{"created":1200},"type":"idle","outcome":"succeeded"},
    {"id":"msg_answer","time":{"created":1100,"completed":1150},
     "type":"assistant","agent":"orchestrator",
     "model":{"providerID":"opencode-go","id":"hy4-flash-free","variant":"default"},
     "content":[{"type":"reasoning","text":"think"},{"type":"text","text":"pong"}],
     "finish":"stop","rawFinish":"stop","cost":0,
     "tokens":{"input":8896,"output":4,"reasoning":26,"cache":{"read":2048,"write":0}}},
    {"id":"msg_user","sessionID":"ses_test","time":{"created":1000},"type":"user",
     "payload":{"text":"Reply with the single word: pong"}}],
    "cursor":{"previous":null,"next":null}}"#;

fn test_config() -> ClientConfig {
    ClientConfig {
        timeouts: Timeouts {
            connect: Duration::from_secs(2),
            response: Duration::from_secs(5),
            read: Duration::from_secs(5),
            body: Duration::from_secs(5),
        },
        poll_interval: Duration::from_millis(20),
        max_wait: Duration::from_secs(2),
        event_idle_timeout: Duration::from_secs(2),
    }
}

fn client(stub: &Stub, config: ClientConfig) -> Assistant {
    client_with_password(stub, PASSWORD, config)
}

fn client_with_password(stub: &Stub, password: &str, config: ClientConfig) -> Assistant {
    let endpoint = ServiceEndpoint::explicit(&stub.url(), password).expect("endpoint");
    Assistant::connect_with(endpoint, config)
}

fn authorized(request: &Request) -> bool {
    request.header("authorization") == Some(EXPECTED_AUTH)
}

fn service(polls: Arc<AtomicUsize>) -> impl Fn(&Request) -> Response + Send + Sync + 'static {
    move |request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match (request.method.as_str(), request.route()) {
            ("GET", "/api/info") => Response::json(INFO),
            ("GET", "/api/provider") => Response::json(PROVIDERS),
            ("GET", "/api/model") => Response::json(MODELS),
            ("POST", "/api/session") => Response::json(SESSION),
            ("POST", "/api/session/ses_test/prompt") => Response::json(PROMPT_ACCEPTED),
            ("POST", "/api/session/ses_test/model") => Response::empty(204),
            ("GET", "/api/session/ses_test/message") => {
                if polls.fetch_add(1, Ordering::SeqCst) == 0 {
                    Response::json(MESSAGES_FIRST)
                } else {
                    Response::json(MESSAGES_DONE)
                }
            }
            _ => Response::empty(404),
        }
    }
}

#[test]
fn sends_basic_auth_and_lists_typed_models() {
    let stub = Stub::start(service(Arc::new(AtomicUsize::new(0))));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();

    let info = assistant.info(&cancel).expect("info");
    assert_eq!(info.version, "2.3.2");
    assert!(info.is_compatible());
    assert_eq!(info.pid, Some(17164));
    assert_eq!(
        assistant
            .check_compatible(&cancel)
            .expect("compatible")
            .version,
        "2.3.2"
    );

    let providers: Vec<Provider> = assistant.providers(&cancel).expect("providers");
    assert_eq!(providers.len(), 3);
    assert_eq!(providers[0].id, "opencode-go");
    assert_eq!(providers[0].label(), "OpenCode Go");
    assert!(!providers[2].is_enabled());

    let models = assistant.models(&cancel).expect("models");
    assert_eq!(models.len(), 2);
    let model = models
        .iter()
        .find(|model| model.label() == "opencode-go/hy4-flash-free")
        .expect("acceptance model");
    assert!(model.is_enabled());
    assert_eq!(model.context_limit(), Some(1_048_576));
    assert_eq!(model.variant_ids(), vec!["none", "high"]);
    assert_eq!(model.name.as_deref(), Some("Hy4 flash Free"));

    let requests = stub.requests();
    assert_eq!(requests.len(), 4);
    for request in &requests {
        assert_eq!(
            request.header("authorization"),
            Some(EXPECTED_AUTH),
            "every request carries the Basic credential"
        );
        assert!(request.header("accept").is_some());
        assert!(!request.path.contains(PASSWORD));
    }
}

#[test]
fn creates_a_session_prompts_and_switches_the_model() {
    let stub = Stub::start(service(Arc::new(AtomicUsize::new(0))));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    let directory = std::env::temp_dir();

    let request = SessionRequest::new(
        "PDFXI test",
        ModelRef::new("opencode-go", "hy4-flash-free"),
        &directory,
    )
    .expect("request");
    let session = assistant
        .create_session(&request, &cancel)
        .expect("session");
    assert_eq!(session.id, "ses_test");
    assert_eq!(
        session.model.as_ref().map(ModelRef::label),
        Some("opencode-go/hy4-flash-free".to_owned())
    );

    let accepted = assistant
        .prompt(&session.id, "Reply with the single word: pong", &cancel)
        .expect("prompt");
    assert_eq!(accepted.id, "msg_user");
    assert_eq!(accepted.created, 1000);
    assert_eq!(accepted.session_id.as_deref(), Some("ses_test"));

    assistant
        .set_model(
            &session.id,
            &ModelRef::new("opencode-go", "hy4-flash-free").with_variant("high"),
            &cancel,
        )
        .expect("model switch");

    let requests = stub.requests();
    assert_eq!(requests.len(), 3);

    let create = &requests[0];
    assert_eq!(create.method, "POST");
    assert_eq!(create.route(), "/api/session");
    let body = create.json();
    assert_eq!(body["title"], "PDFXI test");
    assert_eq!(body["model"]["providerID"], "opencode-go");
    assert_eq!(body["model"]["id"], "hy4-flash-free");
    assert!(body["model"].get("variant").is_none());
    assert_eq!(
        body["location"]["directory"],
        directory.to_string_lossy().as_ref()
    );

    let prompt = &requests[1];
    assert_eq!(prompt.route(), "/api/session/ses_test/prompt");
    assert_eq!(prompt.json()["text"], "Reply with the single word: pong");

    let switch = &requests[2];
    assert_eq!(switch.route(), "/api/session/ses_test/model");
    assert_eq!(switch.json()["model"]["variant"], "high");
}

#[test]
fn ask_polls_until_the_idle_item_and_returns_text_usage_and_finish() {
    let polls = Arc::new(AtomicUsize::new(0));
    let stub = Stub::start(service(Arc::clone(&polls)));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();

    let answer = assistant
        .ask("ses_test", "Reply with the single word: pong", &cancel)
        .expect("answer");
    assert_eq!(answer.text, "pong");
    assert_eq!(answer.finish.as_deref(), Some("stop"));
    assert_eq!(answer.usage.input, 8896);
    assert_eq!(answer.usage.output, 4);
    assert_eq!(answer.usage.reasoning, 26);
    assert_eq!(answer.usage.cache.read, 2048);
    assert_eq!(answer.message_id, "msg_answer");
    assert_eq!(answer.polls, 2, "one empty poll, then the completed turn");
    assert!(answer.elapsed < Duration::from_secs(2));
    assert_eq!(polls.load(Ordering::SeqCst), 2);
}

#[test]
fn ask_times_out_without_an_idle_item() {
    let stub = Stub::start(move |request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match (request.method.as_str(), request.route()) {
            ("POST", "/api/session/ses_test/prompt") => Response::json(PROMPT_ACCEPTED),
            ("GET", "/api/session/ses_test/message") => Response::json(MESSAGES_FIRST),
            _ => Response::empty(404),
        }
    });
    let mut config = test_config();
    config.max_wait = Duration::from_millis(150);
    let assistant = client(&stub, config);
    let cancel = CancelFlag::new();

    let started = Instant::now();
    let error = assistant
        .ask("ses_test", "hello", &cancel)
        .expect_err("bounded wait");
    assert!(matches!(
        error,
        pdfxi_assistant::AssistantError::Timeout { .. }
    ));
    assert!(started.elapsed() < Duration::from_secs(2));
}

#[test]
fn ask_stops_when_cancelled_mid_flight() {
    let stub = Stub::start(move |request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match (request.method.as_str(), request.route()) {
            ("POST", "/api/session/ses_test/prompt") => Response::json(PROMPT_ACCEPTED),
            ("GET", "/api/session/ses_test/message") => Response::json(MESSAGES_FIRST),
            _ => Response::empty(404),
        }
    });
    let mut config = test_config();
    config.max_wait = Duration::from_secs(5);
    let assistant = client(&stub, config);
    let cancel = CancelFlag::new();
    let trigger = cancel.clone();
    let canceller = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(120));
        trigger.cancel();
    });

    let started = Instant::now();
    let error = assistant
        .ask("ses_test", "hello", &cancel)
        .expect_err("cancelled");
    canceller.join().expect("canceller thread");
    assert!(error.is_cancelled());
    assert!(started.elapsed() < Duration::from_secs(3));
}

#[test]
fn a_cancelled_flag_stops_before_any_request() {
    let stub = Stub::start(service(Arc::new(AtomicUsize::new(0))));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    cancel.cancel();

    assert!(
        assistant
            .models(&cancel)
            .expect_err("cancelled")
            .is_cancelled()
    );
    assert!(
        assistant
            .ask("ses_test", "hello", &cancel)
            .expect_err("cancelled")
            .is_cancelled()
    );
    assert!(stub.requests().is_empty(), "no request reached the service");
}

#[test]
fn unauthorized_is_typed_and_never_echoes_the_password() {
    let stub = Stub::start(service(Arc::new(AtomicUsize::new(0))));
    let assistant = client_with_password(&stub, "wrong-secret", test_config());
    let cancel = CancelFlag::new();

    let error = assistant.info(&cancel).expect_err("unauthorized");
    let rendered = format!("{error} {error:?}");
    match &error {
        pdfxi_assistant::AssistantError::Unauthorized { status, .. } => assert_eq!(*status, 401),
        other => panic!("expected Unauthorized, got {other:?}"),
    }
    assert!(!rendered.contains("wrong-secret"));
    assert!(!rendered.contains(EXPECTED_AUTH));
}

#[test]
fn server_error_body_is_redacted_and_capped() {
    let body = format!(
        "{{\"error\":\"boom\",\"password\":\"{PASSWORD}\",\"echo\":\"Authorization: Basic {}\",\"filler\":\"{}\"}}",
        EXPECTED_AUTH.trim_start_matches("Basic "),
        "x".repeat(500)
    );
    let stub = Stub::start(move |_request: &Request| {
        Response::text(500, "application/json", body.clone())
    });
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();

    let error = assistant.info(&cancel).expect_err("server error");
    let rendered = format!("{error} {error:?}");
    match &error {
        pdfxi_assistant::AssistantError::Server { status, body, .. } => {
            assert_eq!(*status, 500);
            assert!(!body.contains(PASSWORD));
            assert!(!body.contains("b3BlbmNvZGU6dGVzdC1zZWNyZXQ="));
            assert!(body.chars().count() <= BODY_SNIPPET_LIMIT + 3);
        }
        other => panic!("expected Server, got {other:?}"),
    }
    assert!(!rendered.contains(PASSWORD));
    assert!(!rendered.contains("b3BlbmNvZGU6dGVzdC1zZWNyZXQ="));
}

#[test]
fn not_found_and_shape_drift_are_typed() {
    let stub = Stub::start(|_request: &Request| Response::empty(404));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    assert!(matches!(
        assistant.info(&cancel).expect_err("not found"),
        pdfxi_assistant::AssistantError::NotFound { .. }
    ));

    let drifted = Stub::start(|_request: &Request| Response::json(r#"{"data":[{"id":"x"}]}"#));
    let assistant = client(&drifted, test_config());
    let error = assistant.models(&cancel).expect_err("shape drift");
    match error {
        pdfxi_assistant::AssistantError::Shape { route, message } => {
            assert_eq!(route, "/api/model");
            assert!(message.contains("JSON"));
        }
        other => panic!("expected Shape, got {other:?}"),
    }
}

#[test]
fn incompatible_service_version_is_rejected() {
    let stub = Stub::start(|_request: &Request| Response::json(r#"{"version":"2.4.0","pid":1}"#));
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    match assistant
        .check_compatible(&cancel)
        .expect_err("incompatible")
    {
        pdfxi_assistant::AssistantError::IncompatibleVersion { version } => {
            assert_eq!(version, "2.4.0");
        }
        other => panic!("expected IncompatibleVersion, got {other:?}"),
    }
}

#[test]
fn event_stream_yields_events_then_reports_the_idle_timeout() {
    let stub = Stub::start(|request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match request.route() {
            "/api/event" => Response::stream(
                "text/event-stream",
                "data: {\"id\":\"evt_1\",\"type\":\"server.connected\",\"data\":{}}\n\n: heartbeat\n\n",
                Duration::from_millis(700),
            ),
            _ => Response::empty(404),
        }
    });
    let mut config = test_config();
    config.event_idle_timeout = Duration::from_millis(150);
    let assistant = client(&stub, config);
    let cancel = CancelFlag::new();
    let mut stream = assistant.events(&cancel).expect("event stream");

    let first = stream.next_event().expect("one event").expect("event");
    assert_eq!(first.kind, "server.connected");
    assert_eq!(first.id.as_deref(), Some("evt_1"));

    let second = stream.next_event().expect("idle result").expect_err("idle");
    assert!(
        matches!(second, pdfxi_assistant::AssistantError::Timeout { .. }),
        "expected an idle timeout, got {second:?}"
    );
    assert!(stream.next_event().is_none(), "the stream is finished");

    let requests = stub.requests();
    assert_eq!(requests.len(), 1);
    assert_eq!(requests[0].header("accept"), Some("text/event-stream"));
    assert_eq!(requests[0].header("authorization"), Some(EXPECTED_AUTH));
}

#[test]
fn event_stream_stops_cleanly_when_cancelled() {
    let stub = Stub::start(|request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        Response::stream(
            "text/event-stream",
            "data: {\"id\":\"evt_1\",\"type\":\"server.connected\",\"data\":{}}\n\n",
            Duration::from_millis(700),
        )
    });
    let mut config = test_config();
    config.event_idle_timeout = Duration::from_secs(2);
    let assistant = client(&stub, config);
    let cancel = CancelFlag::new();
    let mut stream = assistant.events(&cancel).expect("event stream");
    cancel.cancel();
    assert!(stream.next_event().is_none());
}

#[test]
fn event_type_filter_is_validated_and_forwarded() {
    let stub = Stub::start(|request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        Response::stream(
            "text/event-stream",
            "data: {\"type\":\"session.idle\"}\n\n",
            Duration::from_millis(200),
        )
    });
    let mut config = test_config();
    config.event_idle_timeout = Duration::from_millis(100);
    let assistant = client(&stub, config);
    let cancel = CancelFlag::new();

    let mut stream = assistant
        .events_with_types(&["session.idle", "session.text.delta"], &cancel)
        .expect("filtered stream");
    assert_eq!(
        stream.next_event().expect("event").expect("event").kind,
        "session.idle"
    );
    assert_eq!(
        stub.requests()[0].path,
        "/api/event?types=session.idle,session.text.delta"
    );

    assert!(
        assistant
            .events_with_types(&["session.idle\r\nX: y"], &cancel)
            .is_err()
    );
}

fn locked_session_json(rules: &[PermissionRule]) -> String {
    serde_json::json!({
        "data": {
            "id": "ses_test",
            "projectID": "prj_1",
            "model": {"providerID": "opencode-go", "id": "hy4-flash-free", "variant": "default"},
            "cost": 0,
            "tokens": {"input": 0, "output": 0, "reasoning": 0, "cache": {"read": 0, "write": 0}},
            "time": {"created": 1000, "updated": 1000},
            "title": "PDFXI locked test",
            "subpath": "",
            "location": {"directory": "/tmp"},
            "permissions": rules
        }
    })
    .to_string()
}

#[test]
fn locked_session_is_read_back_and_verified() {
    let rules = locked_ruleset();
    let body = locked_session_json(&rules);
    let stub = Stub::start(move |request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match (request.method.as_str(), request.route()) {
            ("GET", "/api/info") => Response::json(INFO),
            ("POST", "/api/session") => Response::json(body.clone()),
            ("GET", "/api/session/ses_test") => Response::json(body.clone()),
            _ => Response::empty(404),
        }
    });
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    let request = SessionRequest::new(
        "PDFXI locked test",
        ModelRef::new("opencode-go", "hy4-flash-free"),
        std::env::temp_dir(),
    )
    .expect("request");
    let locked = assistant
        .create_locked_session(&request, &cancel)
        .expect("verified locked session");
    assert_eq!(locked.rules(), rules.as_slice());

    let requests = stub.requests();
    let created = requests
        .iter()
        .find(|request| request.method == "POST" && request.route() == "/api/session")
        .expect("create request");
    let sent = created.json();
    let sent_rules = sent["permissions"].as_array().expect("permissions array");
    assert_eq!(sent_rules.len(), 6);
    assert_eq!(sent_rules[0]["action"], "*");
    assert_eq!(sent_rules[0]["effect"], "deny");
    assert!(
        requests
            .iter()
            .any(|request| request.method == "GET" && request.route() == "/api/session/ses_test")
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "POST" && request.route() == "/api/session")
            .count(),
        1,
        "exactly one create request"
    );
    assert_eq!(
        requests
            .iter()
            .filter(|request| request.method == "GET" && request.route() == "/api/session/ses_test")
            .count(),
        1,
        "exactly one read-back request"
    );
    assert_eq!(
        requests[0].route(),
        "/api/info",
        "the version gate runs before any session is created"
    );
}

#[test]
fn locked_session_refuses_an_incompatible_service_before_creating_a_session() {
    for version in ["2.4.0", "9.9.9"] {
        let body = format!(r#"{{"version":"{version}","pid":1}}"#);
        let stub = Stub::start(move |request: &Request| {
            if !authorized(request) {
                return Response::empty(401);
            }
            match (request.method.as_str(), request.route()) {
                ("GET", "/api/info") => Response::json(body.clone()),
                _ => Response::empty(404),
            }
        });
        let assistant = client(&stub, test_config());
        let cancel = CancelFlag::new();
        let request = SessionRequest::new(
            "PDFXI locked test",
            ModelRef::new("opencode-go", "hy4-flash-free"),
            std::env::temp_dir(),
        )
        .expect("request");
        match assistant
            .create_locked_session(&request, &cancel)
            .expect_err("an incompatible service must be refused")
        {
            AssistantError::IncompatibleVersion { version: reported } => {
                assert_eq!(reported, version);
            }
            other => panic!("expected IncompatibleVersion, got {other:?}"),
        }
        let requests = stub.requests();
        assert_eq!(requests.len(), 1, "only /api/info is requested");
        assert_eq!(requests[0].route(), "/api/info");
        assert!(
            !requests
                .iter()
                .any(|request| request.method == "POST" && request.route() == "/api/session"),
            "no session is created for {version}"
        );
    }
}

#[test]
fn widened_read_back_deletes_the_session_and_fails_closed() {
    let mut widened = locked_ruleset();
    widened.push(PermissionRule::new("shell", "*", PermissionEffect::Allow));
    let body = locked_session_json(&widened);
    let stub = Stub::start(move |request: &Request| {
        if !authorized(request) {
            return Response::empty(401);
        }
        match (request.method.as_str(), request.route()) {
            ("GET", "/api/info") => Response::json(INFO),
            ("POST", "/api/session") => Response::json(body.clone()),
            ("GET", "/api/session/ses_test") => Response::json(body.clone()),
            ("DELETE", "/api/session/ses_test") => Response::empty(204),
            _ => Response::empty(404),
        }
    });
    let assistant = client(&stub, test_config());
    let cancel = CancelFlag::new();
    let request = SessionRequest::new(
        "PDFXI locked test",
        ModelRef::new("opencode-go", "hy4-flash-free"),
        std::env::temp_dir(),
    )
    .expect("request");
    let error = assistant
        .create_locked_session(&request, &cancel)
        .expect_err("widened read-back must fail");
    assert!(matches!(error, AssistantError::LockdownUnverified { .. }));
    assert!(stub
        .requests()
        .iter()
        .any(|request| request.method == "DELETE" && request.route() == "/api/session/ses_test"));
}
