//! Test-only helper executable for the `pdfxi-text` sidecar bridge tests.
//!
//! This is not a shipped OCR backend. It speaks the T-0057 JSON wire format and
//! implements a handful of deterministic modes selected by the
//! `PDFXI_FAKE_MODE` environment variable (set explicitly by the bridge config
//! in tests, never inherited from ambient state). It always drains stdin first
//! so the parent's request write never blocks, even in the `hang` mode.

use std::env;
use std::io::{Read, Write};
use std::path::PathBuf;
use std::thread;
use std::time::Duration;

use pdfxi_text::ocr::{SidecarRequest, SidecarResponse, SidecarSpan};

fn span(text: String, y: f32) -> SidecarSpan {
    SidecarSpan {
        text,
        x: 0.0,
        y,
        width: 8.0,
        height: 10.0,
        confidence: 95.0,
        language: "eng".to_string(),
    }
}

fn emit(response: &SidecarResponse) {
    let json = response.to_json().expect("valid fake response");
    let mut stdout = std::io::stdout();
    let _ = stdout.write_all(json.as_bytes());
    let _ = stdout.flush();
}

fn present(name: &str) -> String {
    if env::var_os(name).is_some() {
        "present".to_string()
    } else {
        "absent".to_string()
    }
}

fn main() {
    let mode = env::var("PDFXI_FAKE_MODE").unwrap_or_else(|_| "echo".to_string());
    let args: Vec<String> = env::args().skip(1).collect();

    // Two modes deliberately never drain stdin: they prove the bridge's
    // supervisor can kill a child while a large stdin write is still blocked.
    let skip_stdin = matches!(mode.as_str(), "no_read_hang" | "early_exit_partial");
    let mut input = String::new();
    if !skip_stdin {
        let _ = std::io::stdin().read_to_string(&mut input);
    }

    match mode.as_str() {
        "echo" => {
            let request = SidecarRequest::from_json(&input).expect("valid request from bridge");
            let language = request
                .languages
                .first()
                .cloned()
                .unwrap_or_else(|| "eng".to_string());
            let mut spans = vec![span(
                format!(
                    "{}x{}@{}:{}",
                    request.width,
                    request.height,
                    request.dpi as u32,
                    request.pixels_hex.len()
                ),
                0.0,
            )];
            spans.push(span(args.join("\u{1f}"), 1.0));
            let response = SidecarResponse::new(language, spans).expect("valid response");
            emit(&response);
        }
        "echo_args" => {
            let _ = SidecarRequest::from_json(&input).expect("valid request from bridge");
            let response = SidecarResponse::new("eng", vec![span(args.join("\u{1f}"), 0.0)])
                .expect("valid response");
            emit(&response);
        }
        "echo_env" => {
            let text = format!(
                "MODE={}|SECRET={}|AWS={}|PATH={}",
                present("PDFXI_FAKE_MODE"),
                present("PDFXI_TEST_SECRET"),
                present("AWS_SECRET_ACCESS_KEY"),
                present("PATH")
            );
            let response =
                SidecarResponse::new("eng", vec![span(text, 0.0)]).expect("valid response");
            emit(&response);
        }
        "echo_cwd" => {
            let cwd = env::current_dir().unwrap_or_else(|_| PathBuf::from("<none>"));
            let response =
                SidecarResponse::new("eng", vec![span(cwd.to_string_lossy().into_owned(), 0.0)])
                    .expect("valid response");
            emit(&response);
        }
        "exit_nonzero" => {
            eprintln!("deliberate fake sidecar failure");
            std::process::exit(7);
        }
        "malformed" => {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(b"{ this is not json");
            let _ = stdout.flush();
        }
        "version2" => {
            let response = SidecarResponse::new("eng", vec![span("x".to_string(), 0.0)])
                .expect("valid response");
            let json = response
                .to_json()
                .expect("valid response")
                .replace("\"version\":1", "\"version\":2");
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(json.as_bytes());
            let _ = stdout.flush();
        }
        "oversized" => {
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(&vec![b'x'; 256 * 1024]);
            let _ = stdout.flush();
        }
        "hang" => loop {
            thread::sleep(Duration::from_millis(100));
        },
        "no_read_hang" => loop {
            // Never reads stdin: a large request fills the pipe and blocks the
            // bridge's stdin writer until the supervisor kills this process.
            thread::sleep(Duration::from_millis(100));
        },
        "early_exit_partial" => {
            // Writes a truncated response, closes stdout/stderr and exits
            // non-zero without ever reading stdin.
            let mut stdout = std::io::stdout();
            let _ = stdout.write_all(b"{\"version\":1,\"detected_language\":");
            let _ = stdout.flush();
            eprintln!("partial response then early exit");
            std::process::exit(3);
        }
        other => {
            eprintln!("unknown fake sidecar mode: {other}");
            std::process::exit(2);
        }
    }
}
