//! Process-level tests for the Tesseract sidecar bridge.
//!
//! Every test spawns the crate-owned `fake_sidecar` helper binary. No external
//! tool, network access, or GUI is involved, and no test mutates process-global
//! environment state. The only deliberate waits are short bounded delays used
//! to prove timeout and cancellation kill a hanging child.

use std::env;
use std::path::PathBuf;
use std::thread;
use std::time::{Duration, Instant};

use pdfxi_text::CancellationToken;
use pdfxi_text::ocr::{
    OcrEngine, OcrError, OcrImage, OcrOptions, OcrPixelFormat, SUPPORTED_SIDECAR_VERSION,
};
use pdfxi_text::sidecar::{
    DEFAULT_ARGUMENTS, SidecarConfig, SidecarError, SidecarOcrEngine, WorkingDirectoryPolicy,
};

fn fake_exe() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_fake_sidecar"))
}

fn config_with_mode(mode: &str) -> SidecarConfig {
    SidecarConfig::new(fake_exe(), ["eng"])
        .expect("valid config")
        .with_env("PDFXI_FAKE_MODE", mode)
        .expect("mode env")
}

fn image() -> OcrImage {
    OcrImage::new(
        2,
        2,
        300.0,
        OcrPixelFormat::Grayscale,
        vec![0, 128, 200, 255],
    )
    .expect("valid image")
}

fn recognize(config: SidecarConfig) -> Result<pdfxi_text::ocr::OcrPage, SidecarError> {
    let engine = SidecarOcrEngine::new(config).expect("valid engine");
    engine.recognize_page(
        image(),
        &OcrOptions::new(vec!["eng".to_string()]),
        0,
        &CancellationToken::new(),
    )
}

#[test]
fn round_trip_request_and_response_through_bridge() {
    let page = recognize(config_with_mode("echo")).expect("recognition succeeds");
    assert_eq!(page.page(), 0);
    assert_eq!(page.detected_language(), "eng");
    assert_eq!(page.span_count(), 2);
    assert_eq!(page.spans()[0].text, "2x2@300:8");
    assert_eq!(page.spans()[0].language, "eng");
    assert_eq!(page.spans()[0].confidence, 95.0);
    assert_eq!(page.spans()[0].bbox.x, 0.0);
    assert_eq!(page.spans()[0].bbox.y, 0.0);
    // The second span echoes the fixed argument vector back verbatim.
    assert_eq!(page.spans()[1].text, DEFAULT_ARGUMENTS.join("\u{1f}"));
}

#[test]
fn spawn_failure_is_typed() {
    let config = SidecarConfig::new("__pdfxi_no_such_sidecar__", ["eng"]).expect("valid config");
    let result = recognize(config);
    assert!(
        matches!(result, Err(SidecarError::SpawnFailed { ref path, .. }) if path.contains("__pdfxi_no_such_sidecar__")),
        "unexpected result: {result:?}"
    );
}

#[test]
fn non_zero_exit_is_typed_with_stderr() {
    let result = recognize(config_with_mode("exit_nonzero"));
    match result {
        Err(SidecarError::NonZeroExit { code, stderr }) => {
            assert_eq!(code, 7);
            assert!(stderr.contains("deliberate fake sidecar failure"));
        }
        other => panic!("expected NonZeroExit, got {other:?}"),
    }
}

#[test]
fn malformed_json_is_typed() {
    let result = recognize(config_with_mode("malformed"));
    assert!(
        matches!(result, Err(SidecarError::MalformedResponse(_))),
        "unexpected result: {result:?}"
    );
}

#[test]
fn version_mismatch_is_typed() {
    let result = recognize(config_with_mode("version2"));
    match result {
        Err(SidecarError::VersionMismatch { expected, found }) => {
            assert_eq!(expected, SUPPORTED_SIDECAR_VERSION);
            assert_eq!(found, 2);
        }
        other => panic!("expected VersionMismatch, got {other:?}"),
    }
}

#[test]
fn oversized_stdout_is_typed_and_kills_child() {
    let config = config_with_mode("oversized")
        .with_max_stdout_bytes(1024)
        .expect("limit");
    let result = recognize(config);
    assert!(
        matches!(result, Err(SidecarError::OversizedOutput { max }) if max == 1024),
        "unexpected result: {result:?}"
    );
}

#[test]
fn timeout_kills_child_and_returns_promptly() {
    let config = config_with_mode("hang")
        .with_timeout(Duration::from_millis(250))
        .expect("timeout");
    let start = Instant::now();
    let result = recognize(config);
    let elapsed = start.elapsed();
    assert!(
        matches!(result, Err(SidecarError::Timeout { .. })),
        "unexpected result: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(3),
        "supervisor must kill the hung child promptly, took {elapsed:?}"
    );
}

#[test]
fn cancellation_kills_child_and_returns_promptly() {
    let config = config_with_mode("hang")
        .with_timeout(Duration::from_secs(10))
        .expect("timeout");
    let engine = SidecarOcrEngine::new(config).expect("engine");
    let token = CancellationToken::new();
    let watcher = token.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        watcher.cancel();
    });

    let start = Instant::now();
    let result = engine.recognize_page(
        image(),
        &OcrOptions::new(vec!["eng".to_string()]),
        0,
        &token,
    );
    let elapsed = start.elapsed();
    canceller.join().expect("canceller thread");

    assert!(
        matches!(result, Err(SidecarError::Cancelled)),
        "unexpected result: {result:?}"
    );
    assert!(
        elapsed < Duration::from_secs(2),
        "cancellation must be honoured promptly, took {elapsed:?}"
    );
}

#[test]
fn trait_recognize_maps_cancellation_to_contract_error() {
    let config = config_with_mode("hang")
        .with_timeout(Duration::from_secs(10))
        .expect("timeout");
    let token = CancellationToken::new();
    let engine = SidecarOcrEngine::with_cancellation(config, token.clone()).expect("engine");
    let watcher = token.clone();
    let canceller = thread::spawn(move || {
        thread::sleep(Duration::from_millis(60));
        watcher.cancel();
    });

    let result = engine.recognize(image(), &OcrOptions::new(vec!["eng".to_string()]));
    canceller.join().expect("canceller thread");
    assert_eq!(result, Err(OcrError::Cancelled));
}

#[test]
fn arguments_are_passed_verbatim_without_shell_interpretation() {
    let literal = "$(whoami) && echo pwned; *";
    let config = config_with_mode("echo_args")
        .with_arguments(vec!["--json".to_string(), literal.to_string()])
        .expect("literal arguments");
    let page = recognize(config).expect("recognition succeeds");
    assert_eq!(
        page.spans()[0].text,
        ["--json", literal].join("\u{1f}"),
        "a shell would have expanded or split these characters"
    );
}

#[test]
fn no_ambient_secret_environment_is_forwarded() {
    // The config forwards no ambient names and sets only the explicit mode.
    let page = recognize(config_with_mode("echo_env")).expect("recognition succeeds");
    let text = &page.spans()[0].text;
    assert!(
        text.contains("MODE=present"),
        "explicit env is forwarded: {text}"
    );
    assert!(
        text.contains("SECRET=absent"),
        "ambient secret must not leak: {text}"
    );
    assert!(
        text.contains("AWS=absent"),
        "ambient credential must not leak: {text}"
    );
}

#[test]
fn fixed_working_directory_policy_is_applied() {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let config = config_with_mode("echo_cwd")
        .with_working_directory(WorkingDirectoryPolicy::Fixed(dir.clone()))
        .expect("cwd policy");
    let page = recognize(config).expect("recognition succeeds");
    assert_eq!(
        page.spans()[0].text,
        dir.to_string_lossy(),
        "child must run in the configured directory"
    );
}
