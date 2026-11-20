//! Live check against the OpenCode service installed on this machine.
//!
//! Ignored by default: it needs a running OpenCode desktop service, and it
//! makes exactly one generation call on the free acceptance model. Run it once
//! with `cargo test -p pdfxi-assistant --test live -- --ignored --nocapture`.
//! The password is read from the registration file by the client and never
//! printed by this test.

use std::path::PathBuf;

use pdfxi_assistant::{Assistant, CancelFlag, Provider, ServiceEndpoint, SessionRequest};

const ACCEPTANCE_MODEL: &str = "opencode-go/hy4-flash-free";

#[test]
#[ignore = "requires a running OpenCode service"]
fn live_discovery_models_and_one_prompt() {
    let endpoint = ServiceEndpoint::discover().expect("service registration");
    println!(
        "endpoint: {} (loopback: {}, version: {:?})",
        endpoint.url(),
        endpoint.is_loopback(),
        endpoint.registration_version()
    );
    let assistant = Assistant::connect(endpoint);
    let cancel = CancelFlag::new();

    let info = assistant
        .check_compatible(&cancel)
        .expect("compatible service");
    println!("service version: {} pid: {:?}", info.version, info.pid);

    let providers: Vec<Provider> = assistant.providers(&cancel).expect("providers");
    println!(
        "providers: {:?}",
        providers.iter().map(Provider::label).collect::<Vec<_>>()
    );

    let models = assistant.models(&cancel).expect("models");
    let model = models
        .iter()
        .find(|model| model.label() == ACCEPTANCE_MODEL)
        .unwrap_or_else(|| panic!("{ACCEPTANCE_MODEL} is not installed"));
    println!(
        "models: {}; acceptance model {} enabled={} context={:?} variants={:?}",
        models.len(),
        ACCEPTANCE_MODEL,
        model.is_enabled(),
        model.context_limit(),
        model.variant_ids()
    );
    assert!(model.is_enabled(), "{ACCEPTANCE_MODEL} must be enabled");

    let directory =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../target/lanes/automation/t0251-live");
    std::fs::create_dir_all(&directory).expect("create session directory");
    // `canonicalize` would add a `\\?\` verbatim prefix on Windows, which the
    // service does not expect; `absolute` normalises without resolving links.
    let directory = std::path::absolute(&directory).expect("absolute session directory");
    println!("session directory: {}", directory.display());

    let request = SessionRequest::new("PDFXI T-0251 live check", model.reference(None), &directory)
        .expect("session request");
    let session = assistant
        .create_session(&request, &cancel)
        .expect("session");
    println!("session: {}", session.id);

    let answer = assistant
        .ask(&session.id, "Reply with the single word: pong", &cancel)
        .expect("answer");
    println!(
        "answer: {:?} finish: {:?} polls: {} elapsed: {:?}",
        answer.text, answer.finish, answer.polls, answer.elapsed
    );
    println!(
        "usage: input={} output={} reasoning={} cache_read={}",
        answer.usage.input, answer.usage.output, answer.usage.reasoning, answer.usage.cache.read
    );
    assert!(
        answer.text.to_ascii_lowercase().contains("pong"),
        "answer did not contain pong: {:?}",
        answer.text
    );
}
