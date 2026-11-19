//! End-to-end smoke tests for the `pdfxi` command-line binary.
//!
//! Every test writes a deterministic fixture produced by `pdfxi-testkit` to a
//! unique temporary file, runs the real binary through [`std::process::Command`],
//! and inspects the exit status, stdout and stderr. No external test harness is
//! required.

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::Value;

/// A temporary PDF that removes itself when the test finishes.
struct TempPdf {
    path: PathBuf,
    display: String,
}

impl TempPdf {
    /// Write `bytes` to a unique temporary file.
    fn new(bytes: &[u8]) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("pdfxi-cli-{}-{nanos}-{id}.pdf", std::process::id());
        let path = std::env::temp_dir().join(name);
        fs::write(&path, bytes).expect("write temporary PDF");
        let display = path.to_string_lossy().into_owned();
        Self { path, display }
    }

    /// Return the path as a command-line argument.
    fn arg(&self) -> &str {
        &self.display
    }
}

impl Drop for TempPdf {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Build a one-page PDF with a caller-supplied content-stream dictionary
/// suffix, computing all cross-reference offsets from real byte lengths.
fn one_page_pdf(content_dict_extra: &str, content: &str) -> Vec<u8> {
    let dictionary = if content_dict_extra.is_empty() {
        format!("<< /Length {} >>", content.len())
    } else {
        format!("<< /Length {} {} >>", content.len(), content_dict_extra)
    };
    let objects = [
        "<< /Type /Catalog /Pages 2 0 R >>".to_owned(),
        "<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_owned(),
        "<< /Type /Page /Parent 2 0 R /MediaBox [0 0 612 792] /Contents 4 0 R >>".to_owned(),
        format!("{dictionary}\nstream\n{content}\nendstream"),
    ];

    let mut out = String::from("%PDF-1.7\n");
    let mut offsets = Vec::with_capacity(objects.len());
    for (index, object) in objects.iter().enumerate() {
        offsets.push(out.len());
        out.push_str(&format!("{} 0 obj\n{}\nendobj\n", index + 1, object));
    }
    let xref = out.len();
    out.push_str(&format!("xref\n0 {}\n", objects.len() + 1));
    out.push_str("0000000000 65535 f \n");
    for offset in &offsets {
        out.push_str(&format!("{:010} 00000 n \n", offset));
    }
    out.push_str(&format!(
        "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{}\n%%EOF\n",
        objects.len() + 1,
        xref
    ));
    out.into_bytes()
}

/// Invoke the real binary with `args`.
fn run(args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_pdfxi"))
        .args(args)
        .output()
        .expect("spawn pdfxi binary")
}

/// Decode stdout as UTF-8 lossily.
fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

/// Decode stderr as UTF-8 lossily.
fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

#[test]
fn info_reports_one_page_fixture() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&["info", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains(&format!("file: {}", pdf.arg())));
    assert!(text.contains("pdf_version: 1.7"));
    assert!(text.contains(&format!("bytes: {}", pdfxi_testkit::minimal_pdf().len())));
    assert!(text.contains("pages: 1"));
}

#[test]
fn info_reports_three_page_fixture() {
    let pdf = TempPdf::new(&pdfxi_testkit::multipage_pdf(3));
    let output = run(&["info", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    assert!(text.contains("pages: 3"), "stdout: {text}");
}

#[test]
fn info_json_has_stable_fields() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&["info", "--json", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let map = value.as_object().expect("JSON object");
    for key in ["file", "pdf_version", "bytes", "pages"] {
        assert!(map.contains_key(key), "missing field {key}");
    }
    assert_eq!(map.len(), 4, "unexpected extra fields: {map:?}");
    assert_eq!(value["file"].as_str(), Some(pdf.arg()));
    assert_eq!(value["pdf_version"].as_str(), Some("1.7"));
    assert_eq!(
        value["bytes"].as_u64(),
        Some(pdfxi_testkit::minimal_pdf().len() as u64)
    );
    assert_eq!(value["pages"].as_u64(), Some(1));
}

#[test]
fn text_extracts_from_uncompressed_stream() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&["text", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output).trim(), "Hello, PDFXI!");
}

#[test]
fn text_extracts_each_page_of_multipage_fixture() {
    let pdf = TempPdf::new(&pdfxi_testkit::multipage_pdf(3));
    let output = run(&["text", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let text = stdout(&output);
    for page in 1..=3 {
        assert!(
            text.contains(&format!("Page {page} of 3")),
            "missing page {page}: {text}"
        );
    }
}

#[test]
fn text_extracts_tj_tj_array_and_quote_operators() {
    let content = "BT [(A) -120 (B)] TJ (C) ' 1 2 (D) \" ET";
    let pdf = TempPdf::new(&one_page_pdf("", content));
    let output = run(&["text", pdf.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(stdout(&output).trim_end(), "AB\nC\nD");
}

#[test]
fn info_missing_file_fails() {
    let name = format!(
        "pdfxi-cli-missing-{}-{}.pdf",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(name);
    assert!(!path.exists(), "fixture path must not exist");
    let display = path.to_string_lossy().into_owned();
    let output = run(&["info", &display]);
    assert!(!output.status.success(), "missing file should fail");
    assert!(
        stderr(&output).contains("failed to read"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn info_non_pdf_fails() {
    let pdf = TempPdf::new(b"this is not a PDF file");
    let output = run(&["info", pdf.arg()]);
    assert!(!output.status.success(), "non-PDF input should fail");
    assert!(
        stderr(&output).contains("invalid PDF header"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn text_filtered_stream_fails() {
    let pdf = TempPdf::new(&one_page_pdf("/Filter /FlateDecode", "BT (Hello) Tj ET"));
    let output = run(&["text", pdf.arg()]);
    assert!(!output.status.success(), "filtered stream should fail");
    // The core now decodes FlateDecode transparently, so a stream that merely
    // declares the filter but carries plaintext reports a DEFLATE error. An
    // unsupported filter still reports "filtered"; both mean no text is shown.
    let message = stderr(&output);
    assert!(
        message.contains("filtered") || message.to_ascii_lowercase().contains("deflate"),
        "stderr: {message}"
    );
}

/// A unique output path that removes its file when the test finishes.
struct TempOut {
    path: PathBuf,
    display: String,
}

impl TempOut {
    /// Choose a unique, not-yet-created path with the given extension.
    fn new(extension: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!(
            "pdfxi-cli-out-{}-{nanos}-{id}.{extension}",
            std::process::id()
        );
        let path = std::env::temp_dir().join(name);
        let display = path.to_string_lossy().into_owned();
        Self { path, display }
    }

    /// Return the path as a command-line argument.
    fn arg(&self) -> &str {
        &self.display
    }
}

impl Drop for TempOut {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Build an output path whose parent directory does not exist.
#[cfg(feature = "pdfium")]
fn unwritable_output_path() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let dir = std::env::temp_dir().join(format!(
        "pdfxi-cli-no-such-dir-{}-{nanos}-{id}",
        std::process::id()
    ));
    assert!(!dir.exists(), "fixture parent directory must not exist");
    dir.join("out.png").to_string_lossy().into_owned()
}

/// Parse a binary P6 header, returning `(width, height, pixel_offset)`.
#[cfg(feature = "pdfium")]
fn parse_ppm_header(bytes: &[u8]) -> (usize, usize, usize) {
    assert!(bytes.starts_with(b"P6\n"), "not a P6 PPM");
    let rest = &bytes[3..];
    let line_end = rest
        .iter()
        .position(|byte| *byte == b'\n')
        .expect("PPM dimension line");
    let dims = std::str::from_utf8(&rest[..line_end]).expect("UTF-8 dimensions");
    let mut fields = dims.split_whitespace();
    let width: usize = fields.next().expect("PPM width").parse().expect("width");
    let height: usize = fields.next().expect("PPM height").parse().expect("height");
    assert!(rest[line_end + 1..].starts_with(b"255\n"), "PPM max value");
    (width, height, 3 + line_end + 1 + 4)
}

/// Read width and height from a PNG IHDR chunk.
#[cfg(feature = "pdfium")]
fn png_dimensions(bytes: &[u8]) -> (usize, usize) {
    assert_eq!(
        &bytes[..8],
        &[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]
    );
    assert_eq!(&bytes[8..12], &[0u8, 0, 0, 13], "IHDR length");
    assert_eq!(&bytes[12..16], &b"IHDR"[..]);
    let width = u32::from_be_bytes([bytes[16], bytes[17], bytes[18], bytes[19]]);
    let height = u32::from_be_bytes([bytes[20], bytes[21], bytes[22], bytes[23]]);
    (width as usize, height as usize)
}

#[cfg(not(feature = "pdfium"))]
#[test]
fn render_without_pdfium_reports_missing_feature() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let out = TempOut::new("png");
    let output = run(&[
        "render",
        pdf.arg(),
        "--page",
        "0",
        "--scale",
        "1.0",
        "--out",
        out.arg(),
    ]);
    assert!(!output.status.success(), "render must fail without pdfium");
    assert!(
        stderr(&output).contains("built without pdfium"),
        "stderr: {}",
        stderr(&output)
    );
}

#[cfg(feature = "pdfium")]
#[test]
fn render_minimal_pdf_to_ppm_and_png() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());

    let ppm = TempOut::new("ppm");
    let output = run(&[
        "render",
        pdf.arg(),
        "--page",
        "0",
        "--scale",
        "1.0",
        "--out",
        ppm.arg(),
        "--format",
        "ppm",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let ppm_bytes = fs::read(&ppm.path).expect("read PPM output");
    let (ppm_width, ppm_height, pixel_offset) = parse_ppm_header(&ppm_bytes);
    assert!(
        ppm_width > 0 && ppm_height > 0,
        "PPM dimensions must be positive"
    );
    assert!(
        ppm_bytes.len() > pixel_offset,
        "PPM must contain pixel bytes"
    );
    assert_eq!(
        ppm_bytes.len(),
        pixel_offset + ppm_width * ppm_height * 3,
        "P6 payload must be width * height * 3 bytes"
    );

    let png = TempOut::new("png");
    let output = run(&[
        "render",
        pdf.arg(),
        "--page",
        "0",
        "--scale",
        "1.0",
        "--out",
        png.arg(),
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let png_bytes = fs::read(&png.path).expect("read PNG output");
    let (png_width, png_height) = png_dimensions(&png_bytes);
    assert_eq!((png_width, png_height), (ppm_width, ppm_height));
    assert!(
        png_bytes.len() > 8 + 12 + 13 + 12,
        "PNG needs an IDAT chunk"
    );
    assert_eq!(
        &png_bytes[png_bytes.len() - 8..png_bytes.len() - 4],
        &b"IEND"[..],
        "PNG must end with IEND"
    );
}

#[cfg(feature = "pdfium")]
#[test]
fn render_rejects_invalid_page() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let out = TempOut::new("png");
    let output = run(&[
        "render",
        pdf.arg(),
        "--page",
        "99",
        "--scale",
        "1.0",
        "--out",
        out.arg(),
    ]);
    assert!(!output.status.success(), "out-of-range page must fail");
    assert!(
        stderr(&output).contains("out of range"),
        "stderr: {}",
        stderr(&output)
    );
}

#[cfg(feature = "pdfium")]
#[test]
fn render_rejects_invalid_scale() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    for scale in ["0", "-1", "NaN"] {
        let out = TempOut::new("png");
        let output = run(&[
            "render",
            pdf.arg(),
            "--page",
            "0",
            "--scale",
            scale,
            "--out",
            out.arg(),
        ]);
        assert!(!output.status.success(), "scale {scale} must fail");
        assert!(
            stderr(&output).contains("invalid scale"),
            "stderr for scale {scale}: {}",
            stderr(&output)
        );
    }
}

#[cfg(feature = "pdfium")]
#[test]
fn render_rejects_unwritable_output_path() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let path = unwritable_output_path();
    let output = run(&[
        "render",
        pdf.arg(),
        "--page",
        "0",
        "--scale",
        "1.0",
        "--out",
        &path,
    ]);
    assert!(!output.status.success(), "unwritable output must fail");
    assert!(
        stderr(&output).contains("failed to write"),
        "stderr: {}",
        stderr(&output)
    );
}

// ---------------------------------------------------------------------------
// search
// ---------------------------------------------------------------------------

/// Collect trimmed stdout into owned lines for exact-order assertions.
fn output_lines(output: &Output) -> Vec<String> {
    stdout(output)
        .trim_end()
        .lines()
        .map(str::to_owned)
        .collect()
}

#[test]
fn search_finds_single_match_in_single_page() {
    // minimal_pdf() shows "Hello, PDFXI!"; "PDFXI" starts at char offset 7.
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&["search", pdf.arg(), "PDFXI"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(output_lines(&output), vec!["page 1 offset 7 PDFXI"]);
}

#[test]
fn search_orders_results_by_page_then_offset() {
    let pdf = TempPdf::new(&pdfxi_testkit::multipage_pdf(3));
    let output = run(&["search", pdf.arg(), "Page"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        output_lines(&output),
        vec![
            "page 1 offset 0 Page",
            "page 2 offset 0 Page",
            "page 3 offset 0 Page",
        ]
    );

    // Matches within one page ascend by character offset.
    let pdf2 = TempPdf::new(&one_page_pdf("", "BT (hit here and hit there) Tj ET"));
    let output = run(&["search", pdf2.arg(), "hit"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        output_lines(&output),
        vec!["page 1 offset 0 hit", "page 1 offset 13 hit"]
    );
}

#[test]
fn search_is_case_insensitive_by_default_and_can_be_exact() {
    let pdf = TempPdf::new(&one_page_pdf("", "BT (Alpha alpha ALPHA) Tj ET"));

    let folded = run(&["search", pdf.arg(), "alpha"]);
    assert!(folded.status.success(), "stderr: {}", stderr(&folded));
    assert_eq!(
        output_lines(&folded),
        vec![
            "page 1 offset 0 Alpha",
            "page 1 offset 6 alpha",
            "page 1 offset 12 ALPHA",
        ],
        "matched text keeps its original casing"
    );

    let exact = run(&["search", pdf.arg(), "alpha", "--case-sensitive"]);
    assert!(exact.status.success(), "stderr: {}", stderr(&exact));
    assert_eq!(output_lines(&exact), vec!["page 1 offset 6 alpha"]);
}

#[test]
fn search_honors_whole_word() {
    // "cat" occurs at 0, inside "category" at 4, and at 13.
    let pdf = TempPdf::new(&one_page_pdf("", "BT (cat category cat) Tj ET"));
    let any = run(&["search", pdf.arg(), "cat"]);
    assert!(any.status.success(), "stderr: {}", stderr(&any));
    assert_eq!(output_lines(&any).len(), 3);

    let words = run(&["search", pdf.arg(), "cat", "--whole-word"]);
    assert!(words.status.success(), "stderr: {}", stderr(&words));
    assert_eq!(
        output_lines(&words),
        vec!["page 1 offset 0 cat", "page 1 offset 13 cat"],
        "the 'cat' inside 'category' is not a whole word"
    );
}

#[test]
fn search_no_match_exits_zero_with_stderr_note() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    for args in [
        vec!["search", pdf.arg(), "zzzz"],
        vec!["search", pdf.arg(), "zzzz", "--json"],
    ] {
        let output = run(&args);
        assert!(
            output.status.success(),
            "no matches must exit 0: {}",
            stderr(&output)
        );
        assert!(
            stdout(&output).is_empty(),
            "no matches must print zero lines: {:?}",
            stdout(&output)
        );
        assert!(
            stderr(&output).contains("no matches"),
            "stderr: {}",
            stderr(&output)
        );
    }
}

#[test]
fn search_json_has_stable_fields() {
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&["search", pdf.arg(), "PDFXI", "--json"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let map = value.as_object().expect("JSON object");
    for key in ["file", "query", "matches"] {
        assert!(map.contains_key(key), "missing field {key}");
    }
    assert_eq!(map.len(), 3, "unexpected extra fields: {map:?}");
    assert_eq!(value["file"].as_str(), Some(pdf.arg()));
    assert_eq!(value["query"].as_str(), Some("PDFXI"));

    let matches = value["matches"].as_array().expect("matches array");
    assert_eq!(matches.len(), 1);
    let first = matches[0].as_object().expect("match object");
    for key in ["page", "offset", "text"] {
        assert!(first.contains_key(key), "missing match field {key}");
    }
    assert_eq!(first.len(), 3, "unexpected extra match fields: {first:?}");
    assert_eq!(matches[0]["page"].as_u64(), Some(1));
    assert_eq!(matches[0]["offset"].as_u64(), Some(7));
    assert_eq!(matches[0]["text"].as_str(), Some("PDFXI"));
}

#[test]
fn search_missing_file_fails() {
    let name = format!(
        "pdfxi-cli-search-missing-{}-{}.pdf",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos()
    );
    let path = std::env::temp_dir().join(name);
    assert!(!path.exists(), "fixture path must not exist");
    let display = path.to_string_lossy().into_owned();
    let output = run(&["search", &display, "anything"]);
    assert!(!output.status.success(), "missing file should fail");
    assert!(
        stderr(&output).contains("failed to read"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn search_unsupported_filter_reports_clear_error() {
    let pdf = TempPdf::new(&one_page_pdf("/Filter /ASCII85Decode", "BT (Hello) Tj ET"));
    let output = run(&["search", pdf.arg(), "Hello"]);
    assert!(!output.status.success(), "unsupported filter should fail");
    let message = stderr(&output);
    assert!(
        message.contains("unsupported filter") && message.contains("ASCII85Decode"),
        "stderr: {message}"
    );
    assert!(
        message.contains("page 1"),
        "error should name the failing page: {message}"
    );
}

// ---------------------------------------------------------------------------
// action / droplet
// ---------------------------------------------------------------------------

/// A temporary action file written from the tab-delimited text format.
struct TempActionFile {
    path: PathBuf,
    display: String,
}

impl TempActionFile {
    /// Writes `text` to a unique temporary `.act` file.
    fn new(text: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let name = format!("pdfxi-cli-action-{}-{nanos}-{id}.act", std::process::id());
        let path = std::env::temp_dir().join(name);
        fs::write(&path, text).expect("write temporary action file");
        let display = path.to_string_lossy().into_owned();
        Self { path, display }
    }

    /// Return the path as a command-line argument.
    fn arg(&self) -> &str {
        &self.display
    }
}

impl Drop for TempActionFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// A three-step action (import, watermark, export) in the stable text format.
fn staging_action(name: &str, description: &str) -> String {
    format!(
        "ACTION\t{name}\t{description}\n\
         STEP\tload\timport\n\
         STEP\tstamp\twatermark\n\
         PARAM\ttext\tDRAFT\n\
         STEP\tsave\texport\n\
         PARAM\ttarget\tout.pdf\n"
    )
}

/// A unique path that is guaranteed not to exist.
fn missing_path() -> String {
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let id = NEXT.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    std::env::temp_dir()
        .join(format!(
            "pdfxi-cli-action-missing-{}-{nanos}-{id}.pdf",
            std::process::id()
        ))
        .to_string_lossy()
        .into_owned()
}

#[test]
fn action_list_parses_multiple_actions_in_file_order() {
    let file = TempActionFile::new(&format!(
        "{}\n{}",
        staging_action("first", "Description one"),
        staging_action("second", "Description two")
    ));
    let output = run(&["action", "list", file.arg()]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        output_lines(&output),
        vec![
            "first\t3\tDescription one".to_owned(),
            "second\t3\tDescription two".to_owned(),
        ]
    );
}

#[test]
fn action_list_rejects_malformed_file() {
    let file = TempActionFile::new("GARBAGE\n");
    let output = run(&["action", "list", file.arg()]);
    assert!(!output.status.success(), "malformed action file must fail");
    assert!(
        stderr(&output).contains("invalid action file"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).is_empty(),
        "no action lines should be printed"
    );
}

#[test]
fn action_show_prints_steps_and_parameters() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let output = run(&["action", "show", file.arg(), "nightly"]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    assert_eq!(
        output_lines(&output),
        vec![
            "name: nightly",
            "description: Stage every night",
            "steps: 3",
            "1 load import",
            "2 stamp watermark",
            "    params: text=DRAFT",
            "3 save export",
            "    params: target=out.pdf",
        ]
    );
}

#[test]
fn action_show_missing_name_errors() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let output = run(&["action", "show", file.arg(), "missing"]);
    assert!(!output.status.success(), "unknown action must fail");
    let message = stderr(&output);
    assert!(
        message.contains("no action named 'missing'"),
        "stderr: {message}"
    );
    assert!(
        message.contains("nightly"),
        "available names should be listed: {message}"
    );
}

#[test]
fn action_run_orders_files_deterministically() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let a = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let b = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let c = TempPdf::new(&pdfxi_testkit::minimal_pdf());

    let first = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        a.arg(),
        b.arg(),
        c.arg(),
    ]);
    assert!(first.status.success(), "stderr: {}", stderr(&first));
    assert_eq!(
        output_lines(&first),
        vec![
            format!("job-0\t{}\tsucceeded\tsucceeded steps=3", a.arg()),
            format!("job-1\t{}\tsucceeded\tsucceeded steps=3", b.arg()),
            format!("job-2\t{}\tsucceeded\tsucceeded steps=3", c.arg()),
        ]
    );

    let second = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        a.arg(),
        b.arg(),
        c.arg(),
    ]);
    assert_eq!(
        stdout(&first),
        stdout(&second),
        "run output must be deterministic"
    );
}

#[test]
fn action_run_isolates_one_failing_file() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let good_a = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let empty = TempPdf::new(b"");
    let good_b = TempPdf::new(&pdfxi_testkit::minimal_pdf());

    let output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        good_a.arg(),
        empty.arg(),
        good_b.arg(),
    ]);
    assert!(
        !output.status.success(),
        "a failing job must set a non-zero exit"
    );
    assert_eq!(
        output_lines(&output),
        vec![
            format!("job-0\t{}\tsucceeded\tsucceeded steps=3", good_a.arg()),
            format!("job-1\t{}\tfailed\tfailed steps=0 at=load", empty.arg()),
            format!("job-2\t{}\tsucceeded\tsucceeded steps=3", good_b.arg()),
        ]
    );
    assert!(
        stderr(&output).contains("1 of 3 job(s) failed"),
        "stderr: {}",
        stderr(&output)
    );
}

#[test]
fn action_run_validates_before_any_job_runs() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let good = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        good.arg(),
        &missing_path(),
    ]);
    assert!(!output.status.success(), "a missing input must fail");
    assert!(
        stderr(&output).contains("does not exist") && stderr(&output).contains("no jobs were run"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(
        stdout(&output).is_empty(),
        "validation must happen before any job is run: {:?}",
        stdout(&output)
    );
}

#[test]
fn action_run_missing_action_name_errors_before_any_job_runs() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let good = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&[
        "action",
        "run",
        file.arg(),
        "missing",
        "--input",
        good.arg(),
    ]);
    assert!(!output.status.success(), "an unknown action must fail");
    assert!(
        stderr(&output).contains("no action named 'missing'"),
        "stderr: {}",
        stderr(&output)
    );
    assert!(stdout(&output).is_empty(), "no job lines should be printed");
}

#[test]
fn action_run_json_has_stable_shape() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        pdf.arg(),
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));

    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let map = value.as_object().expect("JSON object");
    assert_eq!(map.len(), 2, "unexpected extra fields: {map:?}");
    assert_eq!(value["action"].as_str(), Some("nightly"));
    let jobs = value["jobs"].as_array().expect("jobs array");
    assert_eq!(jobs.len(), 1);

    let job = jobs[0].as_object().expect("job object");
    for key in ["id", "input", "state", "outcome"] {
        assert!(job.contains_key(key), "missing job field {key}");
    }
    assert_eq!(job.len(), 4, "unexpected extra job fields: {job:?}");
    assert_eq!(job["id"].as_str(), Some("job-0"));
    assert_eq!(job["input"].as_str(), Some(pdf.arg()));
    assert_eq!(job["state"].as_str(), Some("succeeded"));

    let outcome = job["outcome"].as_object().expect("outcome object");
    for key in [
        "state",
        "completed_steps",
        "failed_step",
        "message",
        "summary",
        "steps",
    ] {
        assert!(outcome.contains_key(key), "missing outcome field {key}");
    }
    assert_eq!(
        outcome.len(),
        6,
        "unexpected extra outcome fields: {outcome:?}"
    );
    assert_eq!(outcome["state"].as_str(), Some("succeeded"));
    assert_eq!(outcome["summary"].as_str(), Some("succeeded steps=3"));
    assert!(outcome["failed_step"].is_null());
    assert!(outcome["message"].is_null());
    let completed = outcome["completed_steps"]
        .as_array()
        .expect("completed_steps array");
    assert_eq!(completed.len(), 3);
    assert_eq!(completed[0].as_str(), Some("load"));

    let steps = outcome["steps"].as_array().expect("steps array");
    assert_eq!(steps.len(), 3);
    let first = steps[0].as_object().expect("step object");
    for key in ["step_id", "kind", "message", "artifact_kind", "artifact"] {
        assert!(first.contains_key(key), "missing step field {key}");
    }
    assert_eq!(first.len(), 5, "unexpected extra step fields: {first:?}");
    assert_eq!(first["step_id"].as_str(), Some("load"));
    assert_eq!(first["kind"].as_str(), Some("import"));
    assert_eq!(first["artifact_kind"].as_str(), Some("bytes"));
    assert_eq!(
        first["artifact"].as_str(),
        Some(format!("bytes({})", pdfxi_testkit::minimal_pdf().len()).as_str())
    );
    assert_eq!(steps[2]["kind"].as_str(), Some("export"));
    assert_eq!(steps[2]["artifact_kind"].as_str(), Some("path"));
    assert_eq!(steps[2]["artifact"].as_str(), Some("out.pdf"));
}

#[test]
fn action_run_output_dir_resolves_export_target_without_writing() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let dir = std::env::temp_dir().join(format!("pdfxi-cli-action-outdir-{}", std::process::id()));
    let dir_text = dir.to_string_lossy().into_owned();
    let output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        pdf.arg(),
        "--output-dir",
        &dir_text,
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let steps = value["jobs"][0]["outcome"]["steps"]
        .as_array()
        .expect("steps array");
    let expected = dir.join("out.pdf").to_string_lossy().into_owned();
    assert_eq!(steps[2]["artifact"].as_str(), Some(expected.as_str()));
    assert!(
        !dir.join("out.pdf").exists(),
        "the echo executor must not write files"
    );
}

#[test]
fn droplet_matches_action_run_with_defaults() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let a = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let b = TempPdf::new(&pdfxi_testkit::minimal_pdf());

    let action_text_output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        a.arg(),
        b.arg(),
    ]);
    assert!(
        action_text_output.status.success(),
        "stderr: {}",
        stderr(&action_text_output)
    );
    let droplet_text_output = run(&["droplet", file.arg(), "nightly", a.arg(), b.arg()]);
    assert!(
        droplet_text_output.status.success(),
        "stderr: {}",
        stderr(&droplet_text_output)
    );
    assert_eq!(stdout(&action_text_output), stdout(&droplet_text_output));

    let action_json_output = run(&[
        "action",
        "run",
        file.arg(),
        "nightly",
        "--input",
        a.arg(),
        b.arg(),
        "--json",
    ]);
    let droplet_json_output = run(&["droplet", file.arg(), "nightly", a.arg(), b.arg(), "--json"]);
    assert!(
        action_json_output.status.success(),
        "stderr: {}",
        stderr(&action_json_output)
    );
    assert!(
        droplet_json_output.status.success(),
        "stderr: {}",
        stderr(&droplet_json_output)
    );
    let action_value: Value =
        serde_json::from_str(&stdout(&action_json_output)).expect("valid JSON");
    let droplet_value: Value =
        serde_json::from_str(&stdout(&droplet_json_output)).expect("valid JSON");
    assert_eq!(action_value, droplet_value);
}

#[test]
fn droplet_isolates_a_failing_file_and_exits_nonzero() {
    let file = TempActionFile::new(&staging_action("nightly", "Stage every night"));
    let good = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let empty = TempPdf::new(b"");
    let output = run(&["droplet", file.arg(), "nightly", good.arg(), empty.arg()]);
    assert!(
        !output.status.success(),
        "a failing droplet job must exit non-zero"
    );
    assert_eq!(
        output_lines(&output),
        vec![
            format!("job-0\t{}\tsucceeded\tsucceeded steps=3", good.arg()),
            format!("job-1\t{}\tfailed\tfailed steps=0 at=load", empty.arg()),
        ]
    );
}

/// A temporary output directory that removes itself (and its contents) on drop.
struct TempActionDir {
    path: PathBuf,
}

impl TempActionDir {
    /// Creates a uniquely named, empty temporary directory.
    fn new(tag: &str) -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let id = NEXT.fetch_add(1, Ordering::Relaxed);
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let path = std::env::temp_dir().join(format!(
            "pdfxi-cli-{tag}-{}-{nanos}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create temporary action output dir");
        Self { path }
    }
}

impl Drop for TempActionDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.path);
    }
}

/// An import-then-export action with a caller-supplied export target.
fn export_action(target: &str) -> String {
    // The action text format escapes backslashes, so double them for paths.
    let escaped = target.replace('\\', "\\\\");
    format!(
        "ACTION\texport-case\tStage one file\n\
         STEP\tload\timport\n\
         STEP\tsave\texport\n\
         PARAM\ttarget\t{escaped}\n"
    )
}

#[test]
fn action_run_rejects_escaping_export_targets() {
    let out_dir = TempActionDir::new("outdir");
    let out_arg = out_dir.path.to_string_lossy().into_owned();
    let pdf = TempPdf::new(&pdfxi_testkit::minimal_pdf());
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();

    // 1. A parent-directory target must be rejected before a path is recorded.
    let escape_name = format!("pdfxi-cli-escape-{nanos}.pdf");
    let outside = std::env::temp_dir().join(&escape_name);
    assert!(!outside.exists(), "escape sentinel must start absent");
    let escape_file = TempActionFile::new(&export_action(&format!("../{escape_name}")));
    let output = run(&[
        "action",
        "run",
        escape_file.arg(),
        "export-case",
        "--input",
        pdf.arg(),
        "--output-dir",
        &out_arg,
        "--json",
    ]);
    assert!(!output.status.success(), "traversal target must fail");
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    assert_eq!(value["jobs"][0]["state"].as_str(), Some("failed"));
    assert_eq!(
        value["jobs"][0]["outcome"]["failed_step"].as_str(),
        Some("save")
    );
    let message = value["jobs"][0]["outcome"]["message"]
        .as_str()
        .unwrap_or("");
    assert!(
        message.contains("forbidden path component"),
        "message: {message}"
    );
    assert!(
        !outside.exists(),
        "no file may be written outside the output dir"
    );

    // 2. An absolute Windows path must also be rejected.
    let absolute = std::env::temp_dir().join(format!("pdfxi-cli-abs-{nanos}.pdf"));
    assert!(!absolute.exists(), "absolute sentinel must start absent");
    let absolute_file = TempActionFile::new(&export_action(&absolute.to_string_lossy()));
    let output = run(&[
        "action",
        "run",
        absolute_file.arg(),
        "export-case",
        "--input",
        pdf.arg(),
        "--output-dir",
        &out_arg,
        "--json",
    ]);
    assert!(!output.status.success(), "absolute target must fail");
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let message = value["jobs"][0]["outcome"]["message"]
        .as_str()
        .unwrap_or("");
    assert!(message.contains("relative path"), "message: {message}");
    assert!(
        !absolute.exists(),
        "no file may be written at the absolute target"
    );

    // 3. A normal relative name stays under the output dir and writes nothing.
    let ok_file = TempActionFile::new(&export_action("ok.pdf"));
    let output = run(&[
        "action",
        "run",
        ok_file.arg(),
        "export-case",
        "--input",
        pdf.arg(),
        "--output-dir",
        &out_arg,
        "--json",
    ]);
    assert!(output.status.success(), "stderr: {}", stderr(&output));
    let value: Value = serde_json::from_str(&stdout(&output)).expect("valid JSON");
    let steps = value["jobs"][0]["outcome"]["steps"]
        .as_array()
        .expect("steps array");
    let expected = out_dir.path.join("ok.pdf").to_string_lossy().into_owned();
    assert_eq!(steps[1]["artifact"].as_str(), Some(expected.as_str()));
    assert!(
        !out_dir.path.join("ok.pdf").exists(),
        "the echo executor must not write files"
    );
}
